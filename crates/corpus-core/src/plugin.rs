//! The corpus plugin protocol: newline-delimited JSON over stdio.
//!
//! A plugin is any executable that reads request lines on stdin and writes
//! reply lines on stdout:
//!
//! ```text
//! → {"id":1,"method":"probe","params":null}
//! ← {"id":1,"ok":true,"result":{"ready":true,"notes":"regtest up"}}
//! ```
//!
//! Plugins control host infrastructure (docker, nix, Lightning nodes) and
//! are therefore trusted code; the protocol's job is not sandboxing but a
//! stable, language-agnostic contract — a plugin can be a bash script.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;

/// Plugin manifest (`plugin.toml` inside a plugin directory).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginManifest {
    /// Plugin name (unique within the registry).
    pub name: String,
    /// Plugin version.
    pub version: Option<String>,
    /// Human-readable description.
    pub description: Option<String>,
    /// Executable entry point, relative to the manifest's directory.
    pub exec: String,
    /// Environment variables passed to the plugin process.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

impl PluginManifest {
    /// Load a manifest from a plugin directory.
    pub fn load(dir: &Path) -> Result<Self, Error> {
        let path = dir.join("plugin.toml");
        let raw =
            fs::read_to_string(&path).map_err(|e| Error::Manifest(path.clone(), e.to_string()))?;
        let manifest: Self =
            toml::from_str(&raw).map_err(|e| Error::Manifest(path.clone(), e.to_string()))?;
        Ok(manifest)
    }
}

/// Result of a `probe` call: is the environment usable right now?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProbeResult {
    /// True if the environment is ready for campaigns.
    pub ready: bool,
    /// Human-readable detail (what is missing, versions, etc.).
    #[serde(default)]
    pub notes: String,
    /// The version the target is ACTUALLY running right now, discovered
    /// live by the probe (e.g. the mint's `/v1/info`). `None` when the
    /// target is unreachable or the plugin does not report one. Live-only:
    /// never persisted — a stored value would drift the moment the target
    /// restarts. `#[serde(default)]` so an older plugin that omits it still
    /// deserializes.
    #[serde(default)]
    pub running_version: Option<String>,
    /// The rev name the manifest EXPECTS to be running (the `sources.toml`
    /// tag), for a "runs X, pins Y" readout without re-reading the manifest.
    /// `None` when the plugin does not report one.
    #[serde(default)]
    pub expected_tag: Option<String>,
}

/// One oracle as advertised by `oracles`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleInfo {
    /// Oracle name (unique within the plugin).
    pub name: String,
    /// What invariant it checks.
    #[serde(default)]
    pub description: String,
}

/// Result of a `call_oracle`: the verdict and the verbatim oracle log.
/// Finding bodies need the log, so it is part of the protocol contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleResult {
    /// `hold` | `violated` | `inconclusive`.
    pub verdict: String,
    /// Oracle stdout/stderr captured during the run.
    #[serde(default)]
    pub log: String,
}

/// Result of a `sandbox_exec`: combined output and the process exit code.
/// The plugin owns the long-lived sandbox; corpus never learns its name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxExecResult {
    /// Combined stdout+stderr from the sandboxed command.
    pub output: String,
    /// Process exit code.
    pub exit_code: i32,
}

/// One pinned source tree mounted into the sandbox as research corpus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Repository name (`cdk`, `nuts`).
    pub name: String,
    /// Upstream repo (`owner/repo`).
    #[serde(default)]
    pub repo: String,
    /// Human-readable tag/branch the pin was taken from.
    #[serde(default)]
    pub tag: String,
    /// The pinned commit SHA (the actual pin).
    pub sha: String,
    /// Read-only mount point inside the sandbox.
    pub mount: String,
}

/// Result of a `faucet` op.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FaucetResult {
    /// Human-readable result text (invoice, balance, or refusal reason).
    pub text: String,
    /// Sats paid by this op, if it was a successful payment.
    #[serde(default)]
    pub paid_sats: Option<u64>,
}

/// Parameters for `faucet`: op is required, the rest are op-dependent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct FaucetCall {
    /// Paid BOLT11 invoice (`pay`).
    pub invoice: Option<String>,
    /// Amount in sats (`invoice`).
    pub amount_sat: Option<u64>,
    /// Optional memo (`invoice`).
    pub memo: Option<String>,
}

/// A running plugin process speaking the protocol.
#[derive(Debug)]
pub struct Plugin {
    manifest: PluginManifest,
    dir: PathBuf,
    child: Child,
    stdin: ChildStdin,
    /// Reply lines from the reader thread; a bounded `recv_timeout` in
    /// `call` is what keeps a wedged plugin (unbounded nix eval, hung
    /// docker exec) from freezing the whole MCP server.
    replies: std::sync::mpsc::Receiver<String>,
    next_id: AtomicU64,
    /// Maximum time to wait for one reply before killing the plugin tree.
    call_timeout: std::time::Duration,
}

#[derive(Debug, Serialize)]
struct Request<'a> {
    id: u64,
    method: &'a str,
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct Reply {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

impl Plugin {
    /// Spawn the plugin process from its directory.
    pub fn spawn(dir: &Path) -> Result<Self, Error> {
        // Canonicalize: with `current_dir` set, a relative exec path would
        // be resolved ambiguously (parent vs child working directory).
        let dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let manifest = PluginManifest::load(&dir)?;
        let exec = dir.join(&manifest.exec);
        let mut command = Command::new(&exec);
        command
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Plugins log to stderr; it never pollutes the protocol.
            .stderr(Stdio::inherit());
        // Own process group so a timed-out call can kill the plugin AND
        // anything it spawned (oracle scripts, nix re-execs, docker execs).
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        for (key, value) in &manifest.env {
            command.env(key, value);
        }
        let mut child = command.spawn().map_err(|e| Error::Plugin {
            plugin: manifest.name.clone(),
            message: format!("failed to spawn {}: {e}", exec.display()),
        })?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Plugin {
            plugin: manifest.name.clone(),
            message: "no stdin".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Plugin {
            plugin: manifest.name.clone(),
            message: "no stdout".to_string(),
        })?;
        // Reader thread → channel: `call` can then wait with a deadline
        // instead of blocking forever on a wedged plugin.
        let (tx, replies) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,                    // EOF: plugin exited
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break;                     // receiver gone
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let call_timeout = std::env::var("CORPUS_PLUGIN_CALL_TIMEOUT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_millis)
            .unwrap_or(std::time::Duration::from_secs(120));
        Ok(Self {
            manifest,
            dir: dir.to_path_buf(),
            child,
            stdin,
            replies,
            next_id: AtomicU64::new(1),
            call_timeout,
        })
    }

    /// The plugin's manifest.
    pub fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    /// The plugin's directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Call a protocol method; returns the raw `result` value.
    pub fn call(&mut self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = Request { id, method, params };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|e| Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("write failed: {e}"),
            })?;

        let reply_line = match self.replies.recv_timeout(self.call_timeout) {
            Ok(line) => line,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.kill_tree();
                return Err(Error::Plugin {
                    plugin: self.manifest.name.clone(),
                    message: format!(
                        "call {method} timed out after {}s — plugin process tree killed",
                        self.call_timeout.as_secs()
                    ),
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::PluginClosed(self.manifest.name.clone()));
            }
        };
        if reply_line.trim().is_empty() {
            return Err(Error::PluginClosed(self.manifest.name.clone()));
        }
        let reply: Reply = serde_json::from_str(reply_line.trim()).map_err(|e| Error::Plugin {
            plugin: self.manifest.name.clone(),
            message: format!("malformed reply {:?}: {e}", reply_line.trim()),
        })?;
        if reply.id != id {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("reply id {} does not match request id {id}", reply.id),
            });
        }
        if !reply.ok {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: reply
                    .error
                    .unwrap_or_else(|| "unknown plugin error".to_string()),
            });
        }
        Ok(reply.result.unwrap_or(Value::Null))
    }

    /// `probe`: is the environment usable right now?
    pub fn probe(&mut self) -> Result<ProbeResult, Error> {
        let value = self.call("probe", None)?;
        Ok(serde_json::from_value(value)?)
    }

    /// `up`: start the environment.
    pub fn up(&mut self) -> Result<(), Error> {
        self.call("up", None)?;
        Ok(())
    }

    /// `down`: stop the environment.
    pub fn down(&mut self) -> Result<(), Error> {
        self.call("down", None)?;
        Ok(())
    }

    /// `targets`: in-scope endpoints the agent may attack.
    pub fn targets(&mut self) -> Result<Vec<String>, Error> {
        let value = self.call("targets", None)?;
        Ok(serde_json::from_value(value)?)
    }

    /// `oracles`: available invariant checks.
    pub fn oracles(&mut self) -> Result<Vec<OracleInfo>, Error> {
        let value = self.call("oracles", None)?;
        Ok(serde_json::from_value(value)?)
    }

    /// `tools`: files mounted read-only into the sandbox (mount points).
    pub fn tools(&mut self) -> Result<Vec<String>, Error> {
        let value = self.call("tools", None)?;
        Ok(serde_json::from_value(value)?)
    }

    /// `sources`: the pinned source corpus mounted read-only into the
    /// sandbox at /opt/src/<name>.
    pub fn sources(&mut self) -> Result<Vec<SourceInfo>, Error> {
        self.sources_with_sources(None)
    }

    /// `sources` carrying the mission's resolved source pins (`repo ->
    /// sha`), same contract as [`Self::sandbox_exec_with_sources`]: the
    /// plugin answers with the mounts the mission's sandbox actually gets,
    /// not its default pins.
    pub fn sources_with_sources(
        &mut self,
        sources: Option<&serde_json::Map<String, Value>>,
    ) -> Result<Vec<SourceInfo>, Error> {
        let mut params = serde_json::json!({});
        if let Some(sources) = sources {
            params["sources"] = Value::Object(sources.clone());
        }
        let value = self.call("sources", Some(params))?;
        Ok(serde_json::from_value(value)?)
    }

    /// `sandbox_exec`: run a command inside the plugin-owned sandbox.
    /// Lazy-starts the sandbox if it is not already running.
    pub fn sandbox_exec(&mut self, command: &str) -> Result<SandboxExecResult, Error> {
        self.sandbox_exec_with_sources(command, None)
    }

    /// `sandbox_exec` carrying the mission's resolved source pins
    /// (`repo -> sha`): the plugin reconciles the sandbox's source mounts
    /// against them (restarting the long-lived container when its mounts
    /// don't match), so the agent reads exactly the revs the mission
    /// recorded. None = the plugin's default pins.
    pub fn sandbox_exec_with_sources(
        &mut self,
        command: &str,
        sources: Option<&serde_json::Map<String, Value>>,
    ) -> Result<SandboxExecResult, Error> {
        let mut params = serde_json::json!({ "command": command });
        if let Some(sources) = sources {
            params["sources"] = Value::Object(sources.clone());
        }
        let value = self.call("sandbox_exec", Some(params))?;
        Ok(serde_json::from_value(value)?)
    }

    /// `call_oracle`: run one invariant check; returns verdict and log.
    pub fn call_oracle(&mut self, name: &str) -> Result<OracleResult, Error> {
        let value = self.call("call_oracle", Some(serde_json::json!({ "name": name })))?;
        Ok(serde_json::from_value(value)?)
    }

    /// `faucet`: pay an invoice, create an invoice, or read the balance.
    /// The plugin enforces the per-payment cap and the regtest-only check;
    /// the caller (corpus) keeps the per-session budget.
    pub fn faucet(&mut self, op: &str, args: &FaucetCall) -> Result<FaucetResult, Error> {
        let mut params = serde_json::json!({ "op": op });
        if let Some(invoice) = &args.invoice {
            params["invoice"] = serde_json::Value::String(invoice.clone());
        }
        if let Some(amount_sat) = args.amount_sat {
            params["amount_sat"] = serde_json::Value::from(amount_sat);
        }
        if let Some(memo) = &args.memo {
            params["memo"] = serde_json::Value::String(memo.clone());
        }
        let value = self.call("faucet", Some(params))?;
        Ok(serde_json::from_value(value)?)
    }

    /// Kill the plugin and its whole process group (unix): scripts the
    /// plugin spawned — oracle runs, nix re-execs, docker execs — must
    /// not outlive a timed-out call.
    fn kill_tree(&mut self) {
        #[cfg(unix)]
        {
            let pgid = self.child.id().to_string();
            let _ = Command::new("kill")
                .args(["-TERM", &format!("-{pgid}")])
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Respawn the plugin in place after a fatal error (timeout, closed
    /// stream). Callers must NOT blindly retry the failed call — the work
    /// may have completed server-side (e.g. a faucet payment).
    pub fn restart(&mut self) -> Result<(), Error> {
        *self = Self::spawn(&self.dir)?;
        Ok(())
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        // Plugins must handle SIGTERM/SIGKILL by releasing the environment
        // only when they own it; see the plugin authoring guide.
        self.kill_tree();
    }
}
