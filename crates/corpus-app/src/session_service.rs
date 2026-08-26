//! The small OpenCode session surface the app owns.
//!
//! P2.1 proved that the served HTTP API is dramatically cheaper than
//! spawning `opencode` for each read, but also that its `directory` query is
//! not an authorization boundary and its cross-process event stream is not
//! useful in OpenCode 1.18.18.  This module consequently validates every
//! id-addressed read against the returned session directory, version-gates
//! the HTTP adapter, and uses the exact owner's message and process-local
//! status projections for mission supervision.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::Value;

use corpus_observe::is_compatible_opencode_version;
pub(crate) use corpus_observe::MINIMUM_OPENCODE_VERSION;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionBackend {
    Http,
    Cli,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServiceHealth {
    pub backend: SessionBackend,
    pub version: String,
    pub compatible: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummary {
    pub id: String,
    pub directory: PathBuf,
    pub created_ms: u64,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct SessionMessage {
    pub id: String,
    pub role: String,
    pub text: Vec<String>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub cost: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionRef {
    pub id: String,
    pub directory: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionTurnState {
    Pending,
    Active,
    Completed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PromptDeliveryState {
    Pending,
    Active,
    Acknowledged,
    Failed { error: String, retry_ready: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Contract is explicit even while P2.1 keeps polling in force.
pub(crate) enum EventSupport {
    /// P2.1 observed only connected/heartbeat for changes made by an
    /// independent TUI process. Polling stays explicit until that changes.
    PollingOnly,
}

pub(crate) trait SessionService: Send + Sync {
    fn health(&self) -> Result<ServiceHealth, String>;
    fn list(&self, directory: &Path) -> Result<Vec<SessionSummary>, String>;
    #[allow(dead_code)] // Consumed by the live drawer after this boundary lands.
    fn messages(&self, session: &SessionRef) -> Result<Vec<SessionMessage>, String>;

    /// Read compact cumulative accounting from the owning process. This is
    /// intentionally independent of transcript/message export.
    fn usage_snapshot(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
    ) -> Result<corpus_core::UsageSnapshot, String> {
        Err("live usage requires the owning OpenCode TUI endpoint".into())
    }

    /// State of the exact launch turn in its owning TUI process. The durable
    /// user message proves that the launch prompt exists; only an assistant
    /// step with a non-tool continuation finish ends the whole loop.
    fn session_turn_state(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        launched_at_ms: u64,
    ) -> Result<SessionTurnState, String>;

    /// Resume the exact session with one idempotent prompt when its owning
    /// TUI process is idle. Implementations must bind the id to
    /// `session.directory` before writing.
    fn queue_prompt(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        message_id: &str,
        prompt: &str,
    ) -> Result<(), String>;

    /// State of one persisted delivery intent in the exact owning process.
    /// A legacy user message proves admission; only its terminal assistant
    /// response proves acknowledgement.
    fn prompt_delivery_state(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        message_id: &str,
    ) -> Result<PromptDeliveryState, String>;

    #[allow(dead_code)] // P2.1 deliberately selected PollingOnly.
    fn event_support(&self) -> EventSupport {
        EventSupport::PollingOnly
    }

    #[allow(dead_code)] // Abort remains refused until OpenCode proves semantics.
    fn abort(&self, _session: &SessionRef) -> Result<(), String> {
        Err("OpenCode 1.18.18 has no verified cross-process abort contract".into())
    }

    /// Return a one-shot operator-facing diagnostic, if this service had to
    /// degrade. The app calls this from its background maintenance job.
    fn take_warning(&self) -> Option<String> {
        None
    }

    fn find_for_launch(
        &self,
        directory: &Path,
        launched_at_ms: u64,
        claimed: &BTreeSet<String>,
    ) -> Result<String, String> {
        select_launch_session(self.list(directory)?, directory, launched_at_ms, claimed)
    }
}

pub(crate) struct CliSessionService {
    opencode: String,
}

impl Default for CliSessionService {
    fn default() -> Self {
        Self {
            opencode: std::env::var("CORPUS_OPENCODE_BIN").unwrap_or_else(|_| "opencode".into()),
        }
    }
}

impl CliSessionService {
    fn output(&self, directory: &Path, args: &[&str]) -> Result<std::process::Output, String> {
        Command::new(&self.opencode)
            .args(args)
            .current_dir(directory)
            .output()
            .map_err(|error| format!("{} failed: {error}", args.join(" ")))
            .and_then(|output| {
                if output.status.success() {
                    Ok(output)
                } else {
                    Err(format!("{} reported an error", args.join(" ")))
                }
            })
    }
}

impl SessionService for CliSessionService {
    fn health(&self) -> Result<ServiceHealth, String> {
        let output = Command::new(&self.opencode)
            .arg("--version")
            .output()
            .map_err(|error| format!("opencode --version failed: {error}"))?;
        if !output.status.success() {
            return Err("opencode --version reported an error".into());
        }
        let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
        Ok(ServiceHealth {
            backend: SessionBackend::Cli,
            compatible: is_compatible_opencode_version(&version),
            version,
        })
    }

    fn list(&self, directory: &Path) -> Result<Vec<SessionSummary>, String> {
        let output = self.output(
            directory,
            &["session", "list", "--format", "json", "-n", "50"],
        )?;
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("opencode session list gave bad JSON: {error}"))?;
        parse_session_list(&value, false)
    }

    fn messages(&self, session: &SessionRef) -> Result<Vec<SessionMessage>, String> {
        require_bound_session(self.list(&session.directory)?, session)?;
        let output = self.output(&session.directory, &["export", &session.id])?;
        let value: Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("opencode export gave bad JSON: {error}"))?;
        parse_messages(value.get("messages").unwrap_or(&Value::Null))
    }

    fn queue_prompt(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
        _prompt: &str,
    ) -> Result<(), String> {
        Err("durable queued input requires the owning OpenCode TUI endpoint".into())
    }

    fn session_turn_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        Err("exact active-run state requires the owning OpenCode TUI endpoint".into())
    }

    fn prompt_delivery_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        Err("durable prompt acknowledgement requires the owning OpenCode TUI endpoint".into())
    }
}

struct HttpSessionService {
    base: String,
    password: String,
    client: Client,
}

impl HttpSessionService {
    fn new(base: &str, password: String) -> Result<Self, String> {
        let url = reqwest::Url::parse(base)
            .map_err(|error| format!("invalid OpenCode server URL: {error}"))?;
        if url.scheme() != "http" || url.host_str() != Some("127.0.0.1") || url.port().is_none() {
            return Err(
                "OpenCode session server must be an explicit http://127.0.0.1:<port> URL".into(),
            );
        }
        if !url.username().is_empty()
            || url.password().is_some()
            || !url.path().is_empty() && url.path() != "/"
        {
            return Err(
                "OpenCode session server URL must not contain credentials or a path".into(),
            );
        }
        if password.is_empty() {
            return Err("OpenCode session server password is empty".into());
        }
        let client = Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|error| format!("cannot build OpenCode HTTP client: {error}"))?;
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            password,
            client,
        })
    }

    fn get(&self, path: &str, directory: Option<&Path>) -> Result<Value, String> {
        let mut request = self
            .client
            .get(format!("{}{path}", self.base))
            .basic_auth("opencode", Some(&self.password));
        if let Some(directory) = directory {
            request = request.query(&[("directory", directory.to_string_lossy().as_ref())]);
        }
        let response = request
            .send()
            .map_err(|error| format!("OpenCode server request failed: {error}"))?;
        if !response.status().is_success() {
            return Err(format!(
                "OpenCode server returned HTTP {}",
                response.status()
            ));
        }
        response
            .json()
            .map_err(|error| format!("OpenCode server gave bad JSON: {error}"))
    }

    fn require_compatible(&self) -> Result<ServiceHealth, String> {
        let health = self.health()?;
        if !health.compatible {
            return Err(format!(
                "OpenCode server version {} is unsupported (expected {} or a newer 1.18.x patch)",
                health.version, MINIMUM_OPENCODE_VERSION
            ));
        }
        Ok(health)
    }

    fn queue_prompt(
        &self,
        session: &SessionRef,
        message_id: &str,
        prompt: &str,
    ) -> Result<(), String> {
        self.require_compatible()?;
        let record = self.get(
            &format!("/session/{}", session.id),
            Some(&session.directory),
        )?;
        require_bound_session(
            parse_session_list(&Value::Array(vec![record]), true)?,
            session,
        )?;

        let messages = self.get(
            &format!("/session/{}/message", session.id),
            Some(&session.directory),
        )?;
        if has_user_message(&messages, message_id) {
            return Ok(());
        }
        // Do not interrupt a curator that is still working. Persisting the
        // deterministic message id leaves a durable delivery intent; the app
        // retries it once this exact owning server reports the session idle.
        if self.session_is_active(session)? {
            return Ok(());
        }

        let response = self
            .client
            .post(format!("{}/session/{}/prompt_async", self.base, session.id))
            .basic_auth("opencode", Some(&self.password))
            .query(&[("directory", session.directory.to_string_lossy().as_ref())])
            .json(&legacy_prompt_body(message_id, prompt))
            .send()
            .map_err(|error| format!("OpenCode queue request failed: {error}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().unwrap_or_default();
            return Err(format!(
                "OpenCode prompt endpoint returned HTTP {status}{}",
                if detail.is_empty() {
                    String::new()
                } else {
                    format!(": {detail}")
                }
            ));
        }
        Ok(())
    }

    fn session_turn_state(
        &self,
        session: &SessionRef,
        launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        self.require_bound_session(session)?;
        // TUI-launched turns still use OpenCode's legacy message projection
        // in 1.18.19; `session.next.step.started` exists only for the V2 loop.
        // A user message after this exact launch is therefore the portable
        // durable start proof. Intermediate `tool-calls` assistant messages
        // are loop steps, not completed turns.
        let messages = self.get(
            &format!("/session/{}/message", session.id),
            Some(&session.directory),
        )?;
        Ok(legacy_turn_state(&messages, launched_at_ms))
    }

    fn prompt_delivery_state(
        &self,
        session: &SessionRef,
        message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        self.require_bound_session(session)?;
        let messages = self.get(
            &format!("/session/{}/message", session.id),
            Some(&session.directory),
        )?;
        if !has_user_message(&messages, message_id) {
            return Ok(PromptDeliveryState::Pending);
        }
        Ok(match legacy_prompt_terminal(&messages, message_id) {
            Some(Ok(())) => PromptDeliveryState::Acknowledged,
            Some(Err(error)) => PromptDeliveryState::Failed {
                error,
                retry_ready: false,
            },
            None if self.session_is_active(session)? => PromptDeliveryState::Active,
            None if has_assistant_message(&messages, message_id) => PromptDeliveryState::Failed {
                error:
                    "OpenCode parked without producing a response to the admitted completion prompt"
                        .into(),
                retry_ready: false,
            },
            // prompt_async persists the user message before its spawned loop
            // necessarily becomes visible in /session/status. Treat that
            // narrow boundary as pending, not as a permanent failure.
            None => PromptDeliveryState::Pending,
        })
    }

    fn require_bound_session(&self, session: &SessionRef) -> Result<(), String> {
        self.require_compatible()?;
        let record = self.get(
            &format!("/session/{}", session.id),
            Some(&session.directory),
        )?;
        require_bound_session(
            parse_session_list(&Value::Array(vec![record]), true)?,
            session,
        )
    }

    fn usage_snapshot(&self, session: &SessionRef) -> Result<corpus_core::UsageSnapshot, String> {
        self.require_compatible()?;
        let record = self.get(
            &format!("/session/{}", session.id),
            Some(&session.directory),
        )?;
        require_bound_session(
            parse_session_list(&Value::Array(vec![record.clone()]), true)?,
            session,
        )?;
        usage_snapshot_from_record(&record, &session.id)
    }

    fn session_is_active(&self, session: &SessionRef) -> Result<bool, String> {
        let statuses = self.get("/session/status", Some(&session.directory))?;
        let statuses = statuses
            .as_object()
            .ok_or_else(|| "OpenCode session status response was not an object".to_string())?;
        Ok(statuses.contains_key(&session.id))
    }
}

fn message_failure_message(message: &Value) -> String {
    message
        .pointer("/info/error/data/message")
        .and_then(Value::as_str)
        .or_else(|| {
            message
                .pointer("/info/error/message")
                .and_then(Value::as_str)
        })
        .map(str::to_string)
        .unwrap_or_else(|| "OpenCode failed while handling the completion prompt".into())
}

fn legacy_turn_state(messages: &Value, launched_at_ms: u64) -> SessionTurnState {
    let Some(messages) = messages.as_array() else {
        return SessionTurnState::Pending;
    };
    let Some(started_at) = messages
        .iter()
        .filter(|message| message.pointer("/info/role").and_then(Value::as_str) == Some("user"))
        .filter_map(|message| {
            message
                .pointer("/info/time/created")
                .and_then(Value::as_u64)
        })
        .filter(|created| *created >= launched_at_ms)
        .min()
    else {
        return SessionTurnState::Pending;
    };
    let latest = messages
        .iter()
        .filter(|message| {
            message.pointer("/info/role").and_then(Value::as_str) == Some("assistant")
        })
        .filter_map(|message| {
            message
                .pointer("/info/time/created")
                .and_then(Value::as_u64)
                .filter(|created| *created >= started_at)
                .map(|created| (created, message))
        })
        .max_by_key(|(created, _)| *created)
        .map(|(_, message)| message);
    if latest.is_some_and(|message| {
        message
            .pointer("/info/time/completed")
            .and_then(Value::as_u64)
            .is_some()
            && message.pointer("/info/finish").and_then(Value::as_str) != Some("tool-calls")
    }) {
        SessionTurnState::Completed
    } else {
        SessionTurnState::Active
    }
}

fn has_user_message(messages: &Value, message_id: &str) -> bool {
    messages.as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message.pointer("/info/id").and_then(Value::as_str) == Some(message_id)
                && message.pointer("/info/role").and_then(Value::as_str) == Some("user")
        })
    })
}

fn has_assistant_message(messages: &Value, message_id: &str) -> bool {
    messages.as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message.pointer("/info/role").and_then(Value::as_str) == Some("assistant")
                && message.pointer("/info/parentID").and_then(Value::as_str) == Some(message_id)
        })
    })
}

fn legacy_prompt_terminal(messages: &Value, message_id: &str) -> Option<Result<(), String>> {
    let latest = messages
        .as_array()?
        .iter()
        .filter(|message| {
            message.pointer("/info/role").and_then(Value::as_str) == Some("assistant")
                && message.pointer("/info/parentID").and_then(Value::as_str) == Some(message_id)
        })
        .max_by_key(|message| {
            message
                .pointer("/info/time/created")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        })?;
    if latest.pointer("/info/error").is_some() {
        return Some(Err(message_failure_message(latest)));
    }
    (latest
        .pointer("/info/time/completed")
        .and_then(Value::as_u64)
        .is_some()
        && latest.pointer("/info/finish").and_then(Value::as_str) != Some("tool-calls"))
    .then_some(Ok(()))
}

fn legacy_prompt_body(message_id: &str, prompt: &str) -> Value {
    serde_json::json!({
        "messageID": message_id,
        "parts": [{"type": "text", "text": prompt}],
    })
}

impl SessionService for HttpSessionService {
    fn health(&self) -> Result<ServiceHealth, String> {
        let value = self.get("/global/health", None)?;
        let version = value
            .get("version")
            .and_then(Value::as_str)
            .ok_or_else(|| "OpenCode health response omitted version".to_string())?
            .to_string();
        Ok(ServiceHealth {
            backend: SessionBackend::Http,
            compatible: is_compatible_opencode_version(&version),
            version,
        })
    }

    fn list(&self, directory: &Path) -> Result<Vec<SessionSummary>, String> {
        self.require_compatible()?;
        parse_session_list(&self.get("/session", Some(directory))?, true)
    }

    fn messages(&self, session: &SessionRef) -> Result<Vec<SessionMessage>, String> {
        self.require_compatible()?;
        // P2.1 proved that `?directory=` does not constrain an id lookup.
        // Bind the returned record ourselves before reading any content.
        let record = self.get(
            &format!("/session/{}", session.id),
            Some(&session.directory),
        )?;
        require_bound_session(
            parse_session_list(&Value::Array(vec![record]), true)?,
            session,
        )?;
        parse_messages(&self.get(
            &format!("/session/{}/message", session.id),
            Some(&session.directory),
        )?)
    }

    fn usage_snapshot(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
    ) -> Result<corpus_core::UsageSnapshot, String> {
        HttpSessionService::usage_snapshot(self, session)
    }

    fn queue_prompt(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
        message_id: &str,
        prompt: &str,
    ) -> Result<(), String> {
        HttpSessionService::queue_prompt(self, session, message_id, prompt)
    }

    fn session_turn_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
        launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        HttpSessionService::session_turn_state(self, session, launched_at_ms)
    }

    fn prompt_delivery_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
        message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        HttpSessionService::prompt_delivery_state(self, session, message_id)
    }
}

/// Chooses the HTTP adapter only when explicitly configured. We do not
/// auto-start the ~366 MiB server observed in P2.1. Any HTTP failure falls
/// back to the off-thread CLI adapter and records one visible warning.
pub(crate) struct ConfiguredSessionService {
    http: Option<HttpSessionService>,
    configuration_error: Option<String>,
    cli: CliSessionService,
    warning: Mutex<Option<String>>,
}

impl ConfiguredSessionService {
    pub(crate) fn from_env() -> Self {
        let base = std::env::var("CORPUS_OPENCODE_SERVER_URL").ok();
        let password = std::env::var("CORPUS_OPENCODE_SERVER_PASSWORD").ok();
        let (http, configuration_error) = match (base, password) {
            (None, None) => (None, None),
            (Some(base), Some(password)) => match HttpSessionService::new(&base, password) {
                Ok(http) => (Some(http), None),
                Err(error) => (None, Some(error)),
            },
            _ => (
                None,
                Some("both CORPUS_OPENCODE_SERVER_URL and CORPUS_OPENCODE_SERVER_PASSWORD are required".into()),
            ),
        };
        Self {
            http,
            configuration_error,
            cli: CliSessionService::default(),
            warning: Mutex::new(None),
        }
    }

    fn with_fallback<T>(
        &self,
        operation: impl FnOnce(&HttpSessionService) -> Result<T, String>,
        fallback: impl FnOnce(&CliSessionService) -> Result<T, String>,
    ) -> Result<T, String> {
        if let Some(error) = &self.configuration_error {
            self.warn(format!(
                "OpenCode HTTP session adapter is misconfigured ({error}); using CLI fallback"
            ));
        } else if let Some(http) = &self.http {
            match operation(http) {
                Ok(value) => return Ok(value),
                Err(error) => self.warn(format!(
                    "OpenCode HTTP session adapter failed ({error}); using CLI fallback"
                )),
            }
        }
        fallback(&self.cli)
    }

    fn warn(&self, warning: String) {
        if let Ok(mut slot) = self.warning.lock() {
            if slot.is_none() {
                *slot = Some(warning);
            }
        }
    }
}

impl SessionService for ConfiguredSessionService {
    fn health(&self) -> Result<ServiceHealth, String> {
        self.with_fallback(|http| http.require_compatible(), SessionService::health)
    }

    fn list(&self, directory: &Path) -> Result<Vec<SessionSummary>, String> {
        self.with_fallback(|http| http.list(directory), |cli| cli.list(directory))
    }

    fn messages(&self, session: &SessionRef) -> Result<Vec<SessionMessage>, String> {
        self.with_fallback(|http| http.messages(session), |cli| cli.messages(session))
    }

    fn usage_snapshot(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
    ) -> Result<corpus_core::UsageSnapshot, String> {
        let http = HttpSessionService::new(
            &format!("http://127.0.0.1:{}", control.port),
            password.to_string(),
        )?;
        http.usage_snapshot(session)
    }

    fn queue_prompt(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        message_id: &str,
        prompt: &str,
    ) -> Result<(), String> {
        let http = HttpSessionService::new(
            &format!("http://127.0.0.1:{}", control.port),
            password.to_string(),
        )?;
        http.queue_prompt(session, message_id, prompt)
    }

    fn session_turn_state(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        let http = HttpSessionService::new(
            &format!("http://127.0.0.1:{}", control.port),
            password.to_string(),
        )?;
        http.session_turn_state(session, launched_at_ms)
    }

    fn prompt_delivery_state(
        &self,
        control: &corpus_core::MissionControl,
        password: &str,
        session: &SessionRef,
        message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        let http = HttpSessionService::new(
            &format!("http://127.0.0.1:{}", control.port),
            password.to_string(),
        )?;
        http.prompt_delivery_state(session, message_id)
    }

    fn take_warning(&self) -> Option<String> {
        self.warning.lock().ok()?.take()
    }
}

/// Deterministic no-process adapter for AppState and contract tests.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakeSessionService;

#[cfg(test)]
impl SessionService for FakeSessionService {
    fn health(&self) -> Result<ServiceHealth, String> {
        Ok(ServiceHealth {
            backend: SessionBackend::Cli,
            version: MINIMUM_OPENCODE_VERSION.into(),
            compatible: true,
        })
    }

    fn list(&self, _directory: &Path) -> Result<Vec<SessionSummary>, String> {
        Ok(Vec::new())
    }

    fn messages(&self, _session: &SessionRef) -> Result<Vec<SessionMessage>, String> {
        Ok(Vec::new())
    }

    fn usage_snapshot(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
    ) -> Result<corpus_core::UsageSnapshot, String> {
        Ok(corpus_core::UsageSnapshot {
            version: corpus_core::USAGE_SNAPSHOT_VERSION,
            session_id: session.id.clone(),
            captured_at: 1,
            source: "test".into(),
            rows: Vec::new(),
        })
    }

    fn queue_prompt(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
        _prompt: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    fn session_turn_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        Ok(SessionTurnState::Pending)
    }

    fn prompt_delivery_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        Ok(PromptDeliveryState::Pending)
    }

    fn find_for_launch(
        &self,
        _directory: &Path,
        _launched_at_ms: u64,
        _claimed: &BTreeSet<String>,
    ) -> Result<String, String> {
        Ok("fake-conversation".into())
    }
}

pub(crate) fn launch_stamp_ms(session: &str) -> Option<u64> {
    let stem = session.strip_prefix("corpus-")?;
    let (agent, timestamp) = stem.rsplit_once('-')?;
    if agent.is_empty() {
        return None;
    }
    Some(timestamp.parse::<u64>().ok()?.saturating_mul(1000))
}

fn select_launch_session(
    sessions: Vec<SessionSummary>,
    directory: &Path,
    launched_at_ms: u64,
    claimed: &BTreeSet<String>,
) -> Result<String, String> {
    sessions
        .into_iter()
        .filter(|session| session.directory == directory)
        .filter(|session| session.created_ms >= launched_at_ms)
        .filter(|session| !claimed.contains(&session.id))
        .min_by_key(|session| session.created_ms)
        .map(|session| session.id)
        .ok_or_else(|| "no OpenCode session found for this launch".into())
}

fn require_bound_session(
    sessions: Vec<SessionSummary>,
    expected: &SessionRef,
) -> Result<(), String> {
    let session = sessions
        .into_iter()
        .find(|session| session.id == expected.id)
        .ok_or_else(|| format!("OpenCode session {} was not found", expected.id))?;
    if session.directory != expected.directory {
        return Err(format!(
            "OpenCode session {} belongs to {}, not {}",
            expected.id,
            session.directory.display(),
            expected.directory.display()
        ));
    }
    Ok(())
}

fn parse_session_list(value: &Value, served: bool) -> Result<Vec<SessionSummary>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| "OpenCode session list was not an array".to_string())?;
    entries
        .iter()
        .map(|entry| {
            let id = string_field(entry, "id")?;
            let directory = PathBuf::from(string_field(entry, "directory")?);
            let created_ms = if served {
                entry.pointer("/time/created").and_then(Value::as_u64)
            } else {
                entry.get("created").and_then(Value::as_u64)
            }
            .ok_or_else(|| format!("OpenCode session {id} omitted its creation time"))?;
            Ok(SessionSummary {
                id,
                directory,
                created_ms,
                title: entry
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn usage_snapshot_from_record(
    record: &Value,
    session_id: &str,
) -> Result<corpus_core::UsageSnapshot, String> {
    let number = |pointer: &str, flat: &str| {
        record
            .pointer(pointer)
            .and_then(Value::as_u64)
            .or_else(|| record.get(flat).and_then(Value::as_u64))
            .unwrap_or(0)
    };
    let cost = record
        .get("cost")
        .and_then(Value::as_f64)
        .ok_or_else(|| format!("OpenCode session {session_id} omitted cumulative cost"))?;
    let model_id = record
        .pointer("/model/id")
        .and_then(Value::as_str)
        .or_else(|| record.get("modelID").and_then(Value::as_str))
        .unwrap_or("session-total");
    let provider = record
        .pointer("/model/providerID")
        .and_then(Value::as_str)
        .or_else(|| record.get("providerID").and_then(Value::as_str))
        .unwrap_or("aggregate");
    let captured_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Ok(corpus_core::UsageSnapshot {
        version: corpus_core::USAGE_SNAPSHOT_VERSION,
        session_id: session_id.to_string(),
        captured_at,
        source: "opencode-session-aggregate".into(),
        rows: vec![corpus_core::CostRow {
            provider: provider.to_string(),
            model: model_id.rsplit('/').next().unwrap_or(model_id).to_string(),
            tokens_input: number("/tokens/input", "tokens_input"),
            tokens_output: number("/tokens/output", "tokens_output"),
            tokens_reasoning: number("/tokens/reasoning", "tokens_reasoning"),
            cache_read: number("/tokens/cache/read", "tokens_cache_read"),
            cache_write: number("/tokens/cache/write", "tokens_cache_write"),
            cost,
            ..corpus_core::CostRow::default()
        }],
    })
}

fn parse_messages(value: &Value) -> Result<Vec<SessionMessage>, String> {
    let entries = value
        .as_array()
        .ok_or_else(|| "OpenCode messages response was not an array".to_string())?;
    entries
        .iter()
        .map(|entry| {
            let info = entry
                .get("info")
                .ok_or_else(|| "OpenCode message omitted info".to_string())?;
            let text = entry
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
                .collect();
            Ok(SessionMessage {
                id: string_field(info, "id")?,
                role: string_field(info, "role")?,
                text,
                input_tokens: info.pointer("/tokens/input").and_then(Value::as_u64),
                output_tokens: info.pointer("/tokens/output").and_then(Value::as_u64),
                reasoning_tokens: info.pointer("/tokens/reasoning").and_then(Value::as_u64),
                cost: info.get("cost").and_then(Value::as_f64),
            })
        })
        .collect()
}

fn string_field(value: &Value, field: &str) -> Result<String, String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("OpenCode response omitted {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parsers_cover_cli_and_served_shapes() {
        let cli = parse_session_list(
            &json!([{
                "id": "ses_cli", "directory": "/run/p", "created": 12, "title": "CLI"
            }]),
            false,
        )
        .unwrap();
        let http = parse_session_list(
            &json!([{
                "id": "ses_http", "directory": "/run/p", "time": {"created": 13}
            }]),
            true,
        )
        .unwrap();
        assert_eq!(cli[0].created_ms, 12);
        assert_eq!(http[0].created_ms, 13);
    }

    #[test]
    fn launch_selection_is_oldest_eligible_unclaimed_session() {
        let directory = Path::new("/run/p");
        let sessions = vec![
            SessionSummary {
                id: "before".into(),
                directory: directory.into(),
                created_ms: 99,
                title: None,
            },
            SessionSummary {
                id: "claimed".into(),
                directory: directory.into(),
                created_ms: 101,
                title: None,
            },
            SessionSummary {
                id: "ours".into(),
                directory: directory.into(),
                created_ms: 102,
                title: None,
            },
            SessionSummary {
                id: "later".into(),
                directory: directory.into(),
                created_ms: 103,
                title: None,
            },
        ];
        assert_eq!(
            select_launch_session(
                sessions,
                directory,
                100,
                &BTreeSet::from(["claimed".into()])
            )
            .unwrap(),
            "ours"
        );
    }

    #[test]
    fn id_read_rejects_a_record_from_another_directory() {
        let expected = SessionRef {
            id: "ses_x".into(),
            directory: "/run/ours".into(),
        };
        let actual = SessionSummary {
            id: "ses_x".into(),
            directory: "/run/theirs".into(),
            created_ms: 1,
            title: None,
        };
        assert!(require_bound_session(vec![actual], &expected)
            .unwrap_err()
            .contains("/run/theirs"));
    }

    #[test]
    fn messages_include_text_and_usage() {
        let parsed = parse_messages(&json!([{
            "info": {"id":"msg_1", "role":"assistant", "tokens":{"input":2,"output":3,"reasoning":1}, "cost":0.25},
            "parts": [{"type":"text", "text":"done"}, {"type":"tool"}]
        }])).unwrap();
        assert_eq!(parsed[0].text, ["done"]);
        assert_eq!(parsed[0].output_tokens, Some(3));
        assert_eq!(parsed[0].cost, Some(0.25));
    }

    #[test]
    fn session_aggregate_becomes_compact_usage_without_messages() {
        let snapshot = usage_snapshot_from_record(
            &json!({
                "id": "ses_cost",
                "cost": 4.4480775,
                "tokens": {
                    "input": 970080,
                    "output": 28512,
                    "reasoning": 3948,
                    "cache": {"read": 3503125, "write": 0}
                },
                "model": {"id": "moonshotai/kimi-k3", "providerID": "openrouter"}
            }),
            "ses_cost",
        )
        .unwrap();
        assert_eq!(snapshot.session_id, "ses_cost");
        assert_eq!(snapshot.rows[0].model, "kimi-k3");
        assert_eq!(snapshot.rows[0].tokens_input, 970080);
        assert!((snapshot.rows[0].cost - 4.4480775).abs() < f64::EPSILON);
    }

    #[test]
    fn launch_stamp_uses_tail_after_hyphenated_agent() {
        assert_eq!(launch_stamp_ms("corpus-red-team-42"), Some(42_000));
        assert_eq!(launch_stamp_ms("not-corpus-red-team-42"), None);
    }

    #[test]
    fn http_adapter_refuses_non_loopback_and_implicit_ports() {
        assert!(HttpSessionService::new("http://example.com:4096", "secret".into()).is_err());
        assert!(HttpSessionService::new("http://127.0.0.1", "secret".into()).is_err());
        assert!(HttpSessionService::new("http://127.0.0.1:4096", String::new()).is_err());
        assert!(HttpSessionService::new("http://127.0.0.1:4096", "secret".into()).is_ok());
    }

    #[test]
    fn queued_input_payload_uses_the_supported_async_prompt_contract() {
        assert_eq!(
            legacy_prompt_body("msg_corpusabc123", "children finished"),
            json!({
                "messageID": "msg_corpusabc123",
                "parts": [{"type": "text", "text": "children finished"}],
            })
        );
    }

    #[test]
    fn compatibility_accepts_newer_patches_on_the_measured_api_line() {
        assert!(is_compatible_opencode_version("1.18.18"));
        assert!(is_compatible_opencode_version("1.18.19"));
        assert!(is_compatible_opencode_version("1.18.20-beta.1"));
        assert!(!is_compatible_opencode_version("1.18.17"));
        assert!(!is_compatible_opencode_version("1.19.0"));
        assert!(!is_compatible_opencode_version("garbage"));
    }

    #[test]
    fn turn_start_evidence_is_durable_and_scoped_after_the_exact_launch() {
        let messages = json!([{
            "info": {"role": "user", "time": {"created": 1_100}}
        }]);
        assert_eq!(
            legacy_turn_state(&messages, 1_000),
            SessionTurnState::Active
        );
        assert_eq!(
            legacy_turn_state(&messages, 1_300),
            SessionTurnState::Pending
        );

        // Completed assistant fragments without a user turn after the exact
        // launch remain irrelevant.
        let fragments = json!([{
            "info": {"role": "assistant", "time": {"completed": 1_250}}
        }]);
        assert_eq!(
            legacy_turn_state(&fragments, 1_000),
            SessionTurnState::Pending
        );
    }

    #[test]
    fn tool_call_steps_do_not_complete_the_whole_agent_loop() {
        let mut messages = json!([{
            "info": {"role":"user", "time":{"created":1_100}}
        }, {
            "info": {
                "role":"assistant", "finish":"tool-calls",
                "time":{"created":1_101, "completed":1_200}
            }
        }, {
            "info": {"role":"assistant", "time":{"created":1_201}}
        }]);
        assert_eq!(
            legacy_turn_state(&messages, 1_000),
            SessionTurnState::Active
        );
        messages.as_array_mut().unwrap()[2] = json!({
            "info": {
                "role":"assistant", "finish":"stop",
                "time":{"created":1_201, "completed":1_300}
            }
        });
        assert_eq!(
            legacy_turn_state(&messages, 1_000),
            SessionTurnState::Completed
        );
    }

    #[test]
    fn delivery_terminal_is_scoped_to_the_exact_legacy_user_message() {
        let messages = json!([{
            "info":{"id":"ours", "role":"user"}
        }, {
            "info":{"id":"step-1", "role":"assistant", "parentID":"ours", "finish":"tool-calls", "time":{"created":1, "completed":2}}
        }, {
            "info":{"id":"step-2", "role":"assistant", "parentID":"ours", "finish":"stop", "time":{"created":3, "completed":4}}
        }, {
            "info":{"id":"other-step", "role":"assistant", "parentID":"other", "error":{"data":{"message":"unrelated"}}, "time":{"created":5, "completed":6}}
        }]);
        assert!(has_user_message(&messages, "ours"));
        assert!(!has_user_message(&messages, "missing"));
        assert!(has_assistant_message(&messages, "ours"));
        assert!(!has_assistant_message(&messages, "missing"));
        assert_eq!(legacy_prompt_terminal(&messages, "ours"), Some(Ok(())));

        let failed = json!([{
            "info":{"id":"ours", "role":"user"}
        }, {
            "info":{"id":"step", "role":"assistant", "parentID":"ours", "error":{"data":{"message":"Model unavailable"}}, "time":{"created":1, "completed":2}}
        }]);
        assert_eq!(
            legacy_prompt_terminal(&failed, "ours"),
            Some(Err("Model unavailable".into()))
        );
    }
}
