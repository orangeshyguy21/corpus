//! remote.rs — a thin client for remote opencode servers.
//!
//! One `RemoteClient` per server, each used on its own worker thread
//! (blocking reqwest never touches the UI thread). The client is kept
//! deliberately thin and pinned to the documented endpoint surface; the
//! actual response shapes are expected to drift, so every reader is
//! tolerant and stringifies defensively.
//!
//! Endpoints (see the plan):
//!   GET  /global/health             liveness
//!   GET  /agent                     discover operator/researcher agents
//!   POST /session                   create a session           -> id
//!   POST /session/:id/prompt_async  fire a mission (non-blocking)
//!   GET  /session/:id/message       transcript (poll fallback)
//!   POST /session/:id/abort         kill switch
//!   GET  /mcp                       corpus-MCP gate health
//! Server auth is HTTP basic (OPENCODE_SERVER_PASSWORD style).

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

/// One configured remote server (`{name, url, username?, password?}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Display name for the picker.
    pub name: String,
    /// Base URL, e.g. `http://localhost:8000` (no trailing slash needed).
    pub url: String,
    /// Optional HTTP basic-auth username.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// Optional HTTP basic-auth password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// A servers config file: a TOML list of servers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServersConfig {
    #[serde(default)]
    pub servers: Vec<ServerConfig>,
}

impl ServersConfig {
    /// Load from `CORPUS_SERVERS` or `~/.config/corpus/servers.toml`.
    /// An absent file yields an empty config, never an error.
    pub fn load() -> Self {
        let path = std::env::var("CORPUS_SERVERS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| default_config_path());
        match std::fs::read_to_string(&path) {
            Ok(raw) => toml::from_str(&raw).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }
}

fn default_config_path() -> PathBuf {
    #[cfg(unix)]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        PathBuf::from(format!("{home}/.config/corpus/servers.toml"))
    }
    #[cfg(not(unix))]
    {
        PathBuf::from("servers.toml")
    }
}

/// A thin blocking HTTP client against one remote server.
#[derive(Debug, Clone)]
pub struct RemoteClient {
    base: String,
    client: reqwest::blocking::Client,
    username: Option<String>,
    password: Option<String>,
}

/// Health + MCP status of a server (shown as badges in the picker).
#[derive(Debug, Clone, Default)]
pub struct ServerStatus {
    pub healthy: bool,
    pub mcp_ok: bool,
    pub notes: String,
}

impl RemoteClient {
    /// Build a client for a config entry; the URL is the base origin.
    pub fn new(cfg: &ServerConfig) -> Result<Self, String> {
        let base = cfg.url.trim_end_matches('/').to_string();
        if !base.starts_with("http://") && !base.starts_with("https://") {
            return Err(format!("server url must start with http(s)://: {base}"));
        }
        Ok(Self {
            base,
            client: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .map_err(|e| format!("reqwest client: {e}"))?,
            username: cfg.username.clone(),
            password: cfg.password.clone(),
        })
    }

    fn get(&self, path: &str) -> Result<reqwest::blocking::Response, String> {
        let mut req = self.client.get(format!("{}{path}", self.base));
        if let (Some(u), Some(p)) = (&self.username, &self.password) {
            req = req.basic_auth(u, Some(p));
        }
        req.send().map_err(|e| format!("GET {path}: {e}"))
    }

    fn post(&self, path: &str, body: Option<&serde_json::Value>) -> Result<reqwest::blocking::Response, String> {
        let mut req = self.client.post(format!("{}{path}", self.base));
        if let (Some(u), Some(p)) = (&self.username, &self.password) {
            req = req.basic_auth(u, Some(p));
        }
        if let Some(body) = body {
            req = req.json(body);
        }
        req.send().map_err(|e| format!("POST {path}: {e}"))
    }

    /// Liveness probe. Some servers answer with a JSON `{ok: true}`;
    /// a 2xx alone counts as healthy otherwise.
    pub fn health(&self) -> Result<ServerStatus, String> {
        let resp = self.get("/global/health")?;
        let ok = resp.status().is_success();
        let mut mcp_ok = false;
        let mut notes = String::new();
        if ok {
            // A 200 is enough; if the body parses as JSON, grab a message.
            if let Ok(body) = resp.text() {
                if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                    if let Some(msg) = json.get("message").and_then(|v| v.as_str()) {
                        notes = msg.to_string();
                    }
                }
            }
            mcp_ok = self.mcp().unwrap_or(false);
        }
        Ok(ServerStatus {
            healthy: ok,
            mcp_ok,
            notes,
        })
    }

    /// Whether the corpus MCP gate is healthy on this server.
    pub fn mcp(&self) -> Result<bool, String> {
        let resp = self.get("/mcp")?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        if let Ok(text) = resp.text() {
            let val = serde_json::from_str::<serde_json::Value>(&text).unwrap_or_default();
            return Ok(val.get("ok").and_then(|v| v.as_bool()).unwrap_or(true));
        }
        Ok(true)
    }

    /// Discover agents (`operator`, `researcher`, ...).
    pub fn agents(&self) -> Result<Vec<String>, String> {
        let resp = self.get("/agent")?;
        let text = resp.text().map_err(|e| format!("GET /agent body: {e}"))?;
        Ok(parse_agents(&text))
    }

    /// Create a session and return its id.
    pub fn start_session(&self) -> Result<String, String> {
        let resp = self.post("/session", Some(&serde_json::json!({})))?;
        let text = resp.text().map_err(|e| format!("POST /session body: {e}"))?;
        extract_id(&text).ok_or_else(|| format!("no session id in: {text}"))
    }

    /// Fire a mission on a session without blocking for completion.
    pub fn prompt_async(
        &self,
        session_id: &str,
        agent: &str,
        model: Option<&str>,
        prompt: &str,
    ) -> Result<(), String> {
        let mut body = serde_json::json!({
            "agent": agent,
            "parts": [prompt],
        });
        if let Some(model) = model {
            body["model"] = serde_json::Value::String(model.to_string());
        }
        let resp = self.post(&format!("/session/{session_id}/prompt_async"), Some(&body))?;
        if !resp.status().is_success() {
            return Err(format!("prompt_async returned {}", resp.status()));
        }
        Ok(())
    }

    /// Fetch the transcript for a session as readable lines.
    pub fn messages(&self, session_id: &str) -> Result<Vec<String>, String> {
        let resp = self.get(&format!("/session/{session_id}/message"))?;
        let text = resp.text().map_err(|e| format!("GET message: {e}"))?;
        Ok(parse_messages(&text))
    }

    /// Kill a session.
    pub fn abort(&self, session_id: &str) -> Result<(), String> {
        let resp = self.post(
            &format!("/session/{session_id}/abort"),
            Some(&serde_json::json!({})),
        )?;
        if !resp.status().is_success() {
            return Err(format!("abort returned {}", resp.status()));
        }
        Ok(())
    }
}

/// A running remote session, mirroring `mission::Runner`'s channel/abort
/// shape so the Missions view treats local and remote uniformly.
#[derive(Debug)]
pub struct Session {
    stop: Arc<AtomicBool>,
    rx: Receiver<String>,
    tail: Option<JoinHandle<()>>,
    running: bool,
}

impl Session {
    /// Start a remote mission on a worker thread: create the session,
    /// fire the prompt, then poll the transcript until abort or the
    /// session goes away. Lines dedupe by message id.
    pub fn start(cfg: &ServerConfig, agent: &str, model: Option<&str>, mission: &str) -> Result<Self, String> {
        let cfg = cfg.clone();
        let agent = agent.to_string();
        let model = model.map(str::to_string);
        let mission = mission.to_string();

        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let tx2 = tx.clone();

        let tail = std::thread::spawn(move || {
            let client = match RemoteClient::new(&cfg) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx2.send(format!("remote client error: {e}\n"));
                    return;
                }
            };
            let session_id = match client.start_session() {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx2.send(format!("session error: {e}\n"));
                    return;
                }
            };
            let _ = tx2.send(format!("# remote session {session_id} on {}\n", cfg.name));
            if let Err(e) = client.prompt_async(&session_id, &agent, model.as_deref(), &mission) {
                let _ = tx2.send(format!("prompt error: {e}\n"));
            }

            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut errors = 0;
            while !stop_flag.load(Ordering::Relaxed) {
                match client.messages(&session_id) {
                    Ok(lines) => {
                        errors = 0;
                        for line in lines {
                            let key = line.clone();
                            if seen.insert(key) {
                                let _ = tx2.send(line);
                            }
                        }
                    }
                    Err(e) => {
                        errors += 1;
                        if errors >= 3 {
                            let _ = tx2.send(format!("\n(remote session ended: {e})\n"));
                            break;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(600));
            }
            let _ = client.abort(&session_id);
        });

        Ok(Self {
            stop,
            rx,
            tail: Some(tail),
            running: true,
        })
    }

    /// Drain new transcript lines; clears `running` when the worker ends.
    pub fn poll(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(line) => out.push(line),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.running = false;
                    break;
                }
            }
        }
        out
    }

    /// Still streaming?
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Ask the worker to abort the server session and stop polling.
    pub fn abort(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(tail) = self.tail.take() {
            let _ = tail.join();
        }
    }
}

/// Tolerant `/agent` parser: accept an object `{agents:[...]}`, a plain
/// JSON list, or a list of `{name/id}` objects. Falls back to raw text
/// words. Never fails.
fn parse_agents(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        let list: Vec<serde_json::Value> = match &json {
            serde_json::Value::Array(a) => a.clone(),
            serde_json::Value::Object(m) => m
                .get("agents")
                .or_else(|| m.get("agent"))
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for value in list {
            if let Some(s) = value.as_str() {
                out.push(s.to_string());
            } else if let Some(name) = value
                .get("name")
                .or_else(|| value.get("id"))
                .and_then(|x| x.as_str())
            {
                out.push(name.to_string());
            }
        }
        if !out.is_empty() {
            out.sort();
            out.dedup();
            return out;
        }
    }
    text.split_whitespace().map(str::to_string).collect()
}

/// Extract a session id from a `/session` response: `{id}` or a bare id.
fn extract_id(text: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(text) {
        if let Some(s) = json.get("id").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = json.as_str() {
            return Some(s.to_string());
        }
    }
    None
}

/// Convert a `/message` payload into readable transcript lines.
/// Accepts a JSON array of messages or a list of `{parts:[{text}]}`.
fn parse_messages(text: &str) -> Vec<String> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![text.to_string()];
    };
    let msgs: Vec<serde_json::Value> = match &json {
        serde_json::Value::Array(a) => a.clone(),
        serde_json::Value::Object(m) => m
            .get("parts")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    msgs.iter().filter_map(format_message).collect()
}

/// Render one message object as a single readable line.
fn format_message(msg: &serde_json::Value) -> Option<String> {
    let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("assistant");
    let text = if let Some(parts) = msg.get("parts").and_then(|v| v.as_array()) {
        parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        msg.get("text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    };
    if text.trim().is_empty() {
        return None;
    }
    Some(format!("> {role}: {text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_agents_object_and_array() {
        assert_eq!(parse_agents(r#"{"agents":["operator","researcher"]}"#), vec!["operator".to_string(), "researcher".to_string()]);
        assert_eq!(parse_agents(r#"["operator"]"#), vec!["operator".to_string()]);
        assert_eq!(parse_agents(r#"[{"name":"researcher"}]"#), vec!["researcher".to_string()]);
    }

    #[test]
    fn extracts_session_id() {
        assert_eq!(extract_id(r#"{"id":"abc-123"}"#).as_deref(), Some("abc-123"));
        assert_eq!(extract_id(r#""sess-9""#).as_deref(), Some("sess-9"));
        assert_eq!(extract_id("nope"), None);
    }

    #[test]
    fn renders_messages() {
        let text = r#"[
            {"role":"user","parts":[{"type":"text","text":"hello"}]},
            {"role":"assistant","parts":[{"type":"text","text":"world"}]}
        ]"#;
        let lines = parse_messages(text);
        assert_eq!(lines, vec!["> user: hello".to_string(), "> assistant: world".to_string()]);
    }
}
