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

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Error;
pub use corpus_observe::PluginManifest;

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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FaucetCall {
    /// Paid BOLT11 invoice (`pay`).
    pub invoice: Option<String>,
    /// Amount in sats (`invoice`).
    pub amount_sat: Option<u64>,
    /// Optional memo (`invoice`).
    pub memo: Option<String>,
}

/// Successful `hello` result for protocol v1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HelloResult {
    pub protocol: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetRecord {
    pub id: String,
    pub kind: String,
    pub url: String,
    pub source_id: String,
    pub source_sha: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolRecord {
    pub id: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EnvironmentDescription {
    #[serde(default)]
    pub targets: Vec<TargetRecord>,
    #[serde(default)]
    pub tools: Vec<ToolRecord>,
    #[serde(default)]
    pub limits: Value,
    #[serde(default)]
    pub provenance: Value,
}

/// A stable, machine-readable protocol v1 error.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

/// One non-terminal line from a long-running lifecycle call such as `setup`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LifecycleProgress {
    pub id: u64,
    pub event: String,
    pub phase: String,
    #[serde(default)]
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProtocolV1Reply {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Unknown,
    Running,
    Succeeded,
    Failed,
}

/// Recorded state for a mutating request. A caller checks this before retrying
/// after timeout or process loss, so successful work is not performed twice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationStatus {
    pub idempotency_key: String,
    pub state: OperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// Lifecycle stdout is NDJSON: zero or more progress lines followed by exactly
/// one terminal reply. Progress does not extend the caller's outer deadline.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum LifecycleLine {
    Progress(LifecycleProgress),
    Reply(ProtocolV1Reply),
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
                    Ok(0) => break, // EOF: plugin exited
                    Ok(_) => {
                        if tx.send(line).is_err() {
                            break; // receiver gone
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
        let id = self.send_request(method, params)?;

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

    /// Run a protocol-v1 lifecycle method with progress and one hard outer
    /// deadline. This is intentionally separate from the legacy one-reply
    /// client: setup may take minutes and must remain observable/cancellable.
    pub fn lifecycle_call<F>(
        &mut self,
        method: &str,
        params: Option<Value>,
        deadline: std::time::Duration,
        on_progress: F,
    ) -> Result<Value, Error>
    where
        F: FnMut(&LifecycleProgress),
    {
        self.lifecycle_call_cancellable(method, params, deadline, || false, on_progress)
    }

    /// Cancellable form used by typed app jobs. The cancellation predicate is
    /// checked at least every 100ms even when a plugin emits no progress.
    pub fn lifecycle_call_cancellable<C, F>(
        &mut self,
        method: &str,
        params: Option<Value>,
        deadline: std::time::Duration,
        mut is_cancelled: C,
        mut on_progress: F,
    ) -> Result<Value, Error>
    where
        C: FnMut() -> bool,
        F: FnMut(&LifecycleProgress),
    {
        if !matches!(method, "setup" | "doctor" | "status" | "stop") {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("{method} is not a lifecycle method"),
            });
        }
        let id = self.send_request(method, params)?;
        let started = std::time::Instant::now();
        loop {
            if is_cancelled() {
                self.kill_tree();
                return Err(Error::Plugin {
                    plugin: self.manifest.name.clone(),
                    message: format!("lifecycle call {method} cancelled — plugin process tree killed"),
                });
            }
            let Some(remaining) = deadline.checked_sub(started.elapsed()) else {
                self.kill_tree();
                return Err(Error::Plugin {
                    plugin: self.manifest.name.clone(),
                    message: format!(
                        "lifecycle call {method} timed out after {}s — plugin process tree killed",
                        deadline.as_secs()
                    ),
                });
            };
            let poll = remaining.min(std::time::Duration::from_millis(100));
            let line = match self.replies.recv_timeout(poll) {
                Ok(line) => line,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if poll < remaining {
                        continue;
                    }
                    self.kill_tree();
                    return Err(Error::Plugin {
                        plugin: self.manifest.name.clone(),
                        message: format!(
                            "lifecycle call {method} timed out after {}s — plugin process tree killed",
                            deadline.as_secs()
                        ),
                    });
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(Error::PluginClosed(self.manifest.name.clone()));
                }
            };
            let parsed: LifecycleLine =
                serde_json::from_str(line.trim()).map_err(|error| Error::Plugin {
                    plugin: self.manifest.name.clone(),
                    message: format!("malformed lifecycle reply {:?}: {error}", line.trim()),
                })?;
            match parsed {
                LifecycleLine::Progress(progress) => {
                    if progress.id != id || progress.event != "progress" {
                        return Err(Error::Plugin {
                            plugin: self.manifest.name.clone(),
                            message: format!(
                                "invalid lifecycle progress id/event: expected id {id} and event progress"
                            ),
                        });
                    }
                    on_progress(&progress);
                }
                LifecycleLine::Reply(reply) => {
                    if reply.id != id {
                        return Err(Error::Plugin {
                            plugin: self.manifest.name.clone(),
                            message: format!(
                                "reply id {} does not match request id {id}",
                                reply.id
                            ),
                        });
                    }
                    if reply.ok {
                        if reply.error.is_some() {
                            return Err(Error::Plugin {
                                plugin: self.manifest.name.clone(),
                                message: "successful lifecycle reply carried an error".to_string(),
                            });
                        }
                        return Ok(reply.result.unwrap_or(Value::Null));
                    }
                    if reply.result.is_some() {
                        return Err(Error::Plugin {
                            plugin: self.manifest.name.clone(),
                            message: "failed lifecycle reply carried a result".to_string(),
                        });
                    }
                    let error = reply.error.unwrap_or(ProtocolError {
                        code: "unknown".to_string(),
                        message: "unknown plugin error".to_string(),
                        retryable: false,
                        details: None,
                    });
                    return Err(Error::Plugin {
                        plugin: self.manifest.name.clone(),
                        message: format!(
                            "{}: {} (retryable: {})",
                            error.code, error.message, error.retryable
                        ),
                    });
                }
            }
        }
    }

    /// One-reply protocol-v1 call with structured errors.
    pub fn call_v1(&mut self, method: &str, params: Option<Value>) -> Result<Value, Error> {
        let id = self.send_request(method, params)?;
        let line = match self.replies.recv_timeout(self.call_timeout) {
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
        let reply: ProtocolV1Reply =
            serde_json::from_str(line.trim()).map_err(|error| Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("malformed v1 reply {:?}: {error}", line.trim()),
            })?;
        self.finish_v1_reply(id, reply)
    }

    /// Look up a mutating request before deciding whether it is safe to retry.
    pub fn operation_status(&mut self, idempotency_key: &str) -> Result<OperationStatus, Error> {
        self.operation_status_with_params(idempotency_key, serde_json::Map::new())
    }

    /// Context-bearing status lookup for independently installed plugins.
    /// Corpus supplies state/cache paths explicitly; the executable never has
    /// to infer `CORPUS_HOME` or its install layout.
    pub fn operation_status_with_params(
        &mut self,
        idempotency_key: &str,
        mut params: serde_json::Map<String, Value>,
    ) -> Result<OperationStatus, Error> {
        params.insert(
            "idempotency_key".to_string(),
            Value::String(idempotency_key.to_string()),
        );
        let value = self.call_v1(
            "operation_status",
            Some(Value::Object(params)),
        )?;
        Ok(serde_json::from_value(value)?)
    }

    /// Negotiate the executable protocol after manifest validation.
    pub fn hello(&mut self) -> Result<HelloResult, Error> {
        let value = self.call_v1("hello", None)?;
        let hello: HelloResult = serde_json::from_value(value)?;
        let expected_protocol = self.manifest.protocol.as_deref().ok_or_else(|| Error::Plugin {
            plugin: self.manifest.name.clone(),
            message: "hello requires a protocol-v1 manifest".to_string(),
        })?;
        if hello.protocol != expected_protocol {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!(
                    "hello protocol {:?} does not match manifest {:?}",
                    hello.protocol, expected_protocol
                ),
            });
        }
        let mut declared = self.manifest.capabilities.clone();
        let mut reported = hello.capabilities.clone();
        declared.sort();
        reported.sort();
        if reported != declared {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!(
                    "hello capabilities {reported:?} do not match manifest {declared:?}"
                ),
            });
        }
        Ok(hello)
    }

    pub fn describe_v1(&mut self, session_id: &str) -> Result<EnvironmentDescription, Error> {
        let value = self.call_v1("describe", Some(serde_json::json!({"session_id": session_id})))?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn session_probe_v1(&mut self, session_id: &str) -> Result<Value, Error> {
        self.call_v1("session_probe", Some(serde_json::json!({"session_id": session_id})))
    }

    pub fn sandbox_exec_v1(&mut self, session_id: &str, command: &str) -> Result<SandboxExecResult, Error> {
        let value = self.call_v1(
            "sandbox_exec",
            Some(serde_json::json!({"session_id": session_id, "command": command})),
        )?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn faucet_v1(&mut self, session_id: &str, op: &str, call: &FaucetCall) -> Result<FaucetResult, Error> {
        let value = self.call_v1(
            "faucet",
            Some(serde_json::json!({"session_id": session_id, "op": op, "call": call})),
        )?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn wallet_fund_v1(&mut self, session_id: &str, params: Value) -> Result<Value, Error> {
        let mut params = params.as_object().cloned().unwrap_or_default();
        params.insert("session_id".into(), Value::String(session_id.to_string()));
        self.call_v1("wallet_fund", Some(Value::Object(params)))
    }

    pub fn oracles_v1(&mut self, session_id: &str) -> Result<Vec<OracleInfo>, Error> {
        let value = self.call_v1("oracles", Some(serde_json::json!({"session_id": session_id})))?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn call_oracle_v1(&mut self, session_id: &str, name: &str) -> Result<OracleResult, Error> {
        let value = self.call_v1(
            "call_oracle",
            Some(serde_json::json!({"session_id": session_id, "name": name})),
        )?;
        Ok(serde_json::from_value(value)?)
    }

    fn finish_v1_reply(&self, id: u64, reply: ProtocolV1Reply) -> Result<Value, Error> {
        if reply.id != id {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("reply id {} does not match request id {id}", reply.id),
            });
        }
        if reply.ok {
            if reply.error.is_some() {
                return Err(Error::Plugin {
                    plugin: self.manifest.name.clone(),
                    message: "successful v1 reply carried an error".to_string(),
                });
            }
            return Ok(reply.result.unwrap_or(Value::Null));
        }
        if reply.result.is_some() {
            return Err(Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: "failed v1 reply carried a result".to_string(),
            });
        }
        let error = reply.error.unwrap_or(ProtocolError {
            code: "unknown".to_string(),
            message: "unknown plugin error".to_string(),
            retryable: false,
            details: None,
        });
        Err(Error::Plugin {
            plugin: self.manifest.name.clone(),
            message: format!(
                "{}: {} (retryable: {})",
                error.code, error.message, error.retryable
            ),
        })
    }

    fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<u64, Error> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = Request { id, method, params };
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| Error::Plugin {
                plugin: self.manifest.name.clone(),
                message: format!("write failed: {error}"),
            })?;
        Ok(id)
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

    pub fn wallet_fund_legacy(
        &mut self,
        mut params: serde_json::Map<String, Value>,
        sources: Option<&serde_json::Map<String, Value>>,
    ) -> Result<Value, Error> {
        if let Some(sources) = sources {
            params.insert("sources".into(), Value::Object(sources.clone()));
        }
        self.call("wallet_fund", Some(Value::Object(params)))
    }

    /// Kill the plugin and its whole process group (unix): scripts the
    /// plugin spawned — oracle runs, nix re-execs, docker execs — must
    /// not outlive a timed-out call.
    fn kill_tree(&mut self) {
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }
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
