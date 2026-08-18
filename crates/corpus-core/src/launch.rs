//! Run-launch seam: materialize an agent to `.opencode/agent/`, then launch
//! an opencode mission — shared by the CLI (`corpus run`, headless) and the
//! app.
//!
//! Naming scheme: bare names (no team prefix — teams are gone). The spawned
//! opencode inherits CORPUS_PROJECT / CORPUS_STORE so the MCP server routes
//! writes into the project corpus.
//!
//! Supervisor + full-TUI decision (2026-08-11, corrected same-day):
//! an interactive launch spawns the REAL opencode TUI in a DETACHED
//! tmux session (`opencode --agent <a> --model <m> --prompt "<mission>"`)
//! — a detached session IS the headless mode, so attach/detach/close/
//! re-attach never kill the run, and attaching shows a steerable TUI,
//! not a one-shot `[exited]` dump. The TUI has no stdout, so:
//!   - tail           = `tmux pipe-pane` raw capture (ANSI-stripped for
//!                      the app), written into the project corpus runs/
//!                      as `<epoch>-<agent>.raw` from the first output —
//!                      the durable run log, not the record;
//!   - record         = `opencode export <id>` (the newest session in the
//!                      project dir) -> `<epoch>-<agent>.json` in the
//!                      project corpus runs/ on Stop (best-effort — the
//!                      .raw log is the durable fallback);
//!   - completion     = operator-driven: a TUI session doesn't exit, a
//!                      run stays live until Stop (best-effort export +
//!                      `tmux kill-session`) or opencode itself exits.
//! The app NEVER inherits opencode's ambient default model: the model
//! is resolved primary-agent-model -> launch arg -> registry tool-use
//! default, and a launch with none fails loudly instead of spawning.
//!
//! Headless `opencode run` stays for automation (`corpus run`,
//! scripted missions): the piped spawn behind the same handle. It is
//! also the no-tmux fallback for the app (attach greys).

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::models::ModelRegistry;
use crate::store::{Store, AGENT_ENV, HANDLE_ENV, PROJECT_ENV, RUN_LOG_ENV, SOURCE_PINS_ENV, STORE_ENV};

/// One transcript line. In the piped backend the two child streams are
/// kept apart; in the TUI backend lines come from the raw capture, so
/// `stderr` is always false there.
#[derive(Debug, Clone)]
pub struct RunLine {
    pub stderr: bool,
    pub text: String,
}

/// Where the run actually executes. The app/CLI never branch on this —
/// only `attach_command()` / `stop()` are backend-shaped.
enum Backend {
    /// The full opencode TUI in a detached tmux session.
    Tui {
        session: String,
        /// Discovered opencode session id (newest in the project dir).
        tui_session_id: Option<String>,
        /// Epoch-millis when we spawned; the discovery window anchors here.
        launched_at_ms: u64,
        stopped: bool,
        exported: bool,
        /// `<epoch>-<agent>.json`: the exported transcript of record.
        export_json: PathBuf,
        /// `tmux pipe-pane` raw capture: the live tail source AND the
        /// durable run log (`<epoch>-<agent>.raw` in the project corpus
        /// runs/ — never deleted, survives app death and missing exports).
        raw: PathBuf,
        /// The tiny run script (temp file) the pane executed.
        script: PathBuf,
        file_pos: u64,
        pending: String,
        /// Throttled `tmux has-session` verdict (a subprocess spawn —
        /// re-checked at most once a second by `try_exit`).
        liveness: (std::time::Instant, bool),
        /// Last `opencode session list` lookup by `opencode_session_id`
        /// (also a subprocess — the app asks every poll until it lands).
        discovery: std::time::Instant,
        /// The project run dir the TUI runs in: the cwd opencode keys its
        /// sessions by, needed to find/export this run's session.
        repo: PathBuf,
    },
    /// No tmux / headless automation: `opencode run` piped directly.
    Piped {
        child: Child,
        rx: mpsc::Receiver<RunLine>,
    },
}

/// A running opencode mission on a project scope.
pub struct RunSession {
    /// The record file: the exported JSON for a TUI run, the .log for
    /// a piped one.
    pub transcript: PathBuf,
    backend: Backend,
}

impl RunSession {
    /// APP launch: resolve the model (primary-model -> arg -> registry
    /// tool-use default, fail loudly if none), then run the FULL TUI in
    /// a detached tmux session, or the piped headless fallback when tmux
    /// is absent. `source_pins_json` is the RESOLVED `repo -> sha` map
    /// (from `registry::prepare_source_pins`, trees already fetched) —
    /// exported as CORPUS_SOURCE_PINS so the sandbox mounts exactly the
    /// revs the mission recorded; None = the plugin's default pins.
    pub fn spawn(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let model = resolve_launch_model(&store, project, agent, model)?;
        if tmux_available().is_some() {
            Self::start_tui(&store, project, agent, &model, mission, source_pins_json, None)
        } else {
            Self::start_piped(&store, project, agent, Some(&model), mission, None, source_pins_json)
        }
    }

    /// Re-open an EXISTING opencode session in a fresh TUI
    /// (`opencode --session <id>`): the conversation comes back with its
    /// history, so a mission whose tmux session is long dead is steerable
    /// again instead of being a dead record.
    ///
    /// The session id is known up front, so it seeds `tui_session_id` —
    /// a resumed run can export from its first moment (the launch-window
    /// search that a fresh spawn needs would never match an old session).
    /// TUI only: resuming is an interactive act, and the piped backend
    /// has no session to return to.
    pub fn resume(
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
    ) -> Result<Self> {
        if tmux_available().is_none() {
            return Err(Error::Store(
                "resume needs tmux — the piped backend has no session to re-open".into(),
            ));
        }
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        let model = resolve_launch_model(&store, project, agent, model)?;
        Self::start_tui(
            &store,
            project,
            agent,
            &model,
            "",
            source_pins_json,
            Some(opencode_session_id),
        )
    }

    /// The opencode session id backing this run, discovered on demand and
    /// cached: the app persists it on the mission record so the session
    /// can be exported or RESUMED later. `None` until opencode has
    /// actually created the session (a TUI takes a moment to boot), and
    /// always `None` for the piped backend.
    ///
    /// Throttled to one lookup a second — each call is an `opencode
    /// session list` subprocess, and the app asks on its poll beat.
    pub fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String> {
        let Backend::Tui { tui_session_id, launched_at_ms, repo, discovery, .. } =
            &mut self.backend
        else {
            return None;
        };
        if let Some(id) = tui_session_id {
            return Some(id.clone());
        }
        if discovery.elapsed() < Duration::from_secs(1) {
            return None;
        }
        *discovery = std::time::Instant::now();
        let found = find_opencode_session(repo.as_path(), *launched_at_ms, claimed).ok()?;
        *tui_session_id = Some(found.clone());
        Some(found)
    }

    /// CLI automation: always the headless `opencode run` piped path.
    /// No model resolution — `-m` stays optional (scripted missions may
    /// lean on opencode's own default-resolver).
    pub fn spawn_headless(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        Self::start_piped(&store, project, agent, model, mission, None, None)
    }

    /// CLI automation APPENDING to an existing transcript (the
    /// researcher follow-up pass).
    pub fn spawn_headless_append(
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: &Path,
    ) -> Result<Self> {
        let store = Store::from_env();
        let runs_dir = store.project_corpus_dir(project).join("runs");
        let _ = fs::create_dir_all(&runs_dir);
        Self::start_piped(
            &store,
            project,
            agent,
            model,
            mission,
            Some(append_to),
            None,
        )
    }

    /// The full opencode TUI in a detached tmux session. `resume` carries
    /// an existing opencode session id to re-open (`--session`) instead of
    /// starting a fresh conversation.
    fn start_tui(
        store: &Store,
        project: &str,
        agent: &str,
        model: &str,
        mission: &str,
        source_pins: Option<&str>,
        resume: Option<&str>,
    ) -> Result<Self> {
        let opencode = resolve_opencode()?;
        let tmux = resolve_tmux().ok_or_else(|| Error::Store("tmux vanished".into()))?;
        let ts = now_secs();
        // Two identifiers, on purpose: `agent_stem` is the dir slug (the
        // run's identity, resolved to a role server-side); `handle` is the
        // name opencode shows and loads `--agent` by. They coincide for an
        // unnamed agent.
        let agent_stem = crate::store::slugify(agent);
        let handle = opencode_agent_handle(store, project, agent);
        // The session name stays keyed by the dir slug: `session_raw_log`
        // pairs `corpus-<slug>-<ts>` with `runs/<ts>-<slug>.raw`, and the
        // raw capture is written by slug. Only opencode's `--agent` (the
        // HANDLE_ENV below) uses the friendly handle.
        let session = format!("corpus-{agent_stem}-{ts}");
        let export_json = Self::runs_for(store, project, agent, ts, "json");
        let temp = std::env::temp_dir();
        // The raw capture is a CORPUS ARTIFACT, not a temp file: pipe-pane
        // appends to it from the first output, so the run leaves a durable
        // log in the project corpus runs/ even if the app dies, the export
        // never happens, or the session is never stopped.
        let raw = Self::runs_for(store, project, agent, ts, "raw");
        let script = temp.join(format!("{session}.sh"));

        let repo = store.provision_run_dir(project)?; // the run's cwd
        let prompt = if mission.trim().is_empty() {
            None
        } else {
            Some(mission)
        };
        let opencode_bin = opencode.display().to_string();
        let store_root = store.root().to_string_lossy().into_owned();
        let raw_log = raw
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut env: Vec<(&str, &str)> = vec![
            ("CORPUS_OPENCODE_BIN", &opencode_bin),
            (AGENT_ENV, &agent_stem),
            (HANDLE_ENV, &handle),
            ("CORPUS_OPENCODE_MODEL", model),
            (PROJECT_ENV, project),
            (STORE_ENV, &store_root),
            (RUN_LOG_ENV, &raw_log),
        ];
        if let Some(pins) = source_pins {
            env.push((SOURCE_PINS_ENV, pins));
        }
        write_tui_script(&script, &env, prompt, resume)?;
        // Stamped BEFORE the spawn: session discovery keys off "created
        // after this moment", and a stamp taken afterwards could in
        // principle sit past the session opencode went on to create.
        let launched_at_ms = now_millis();
        let mut command = Command::new(&tmux);
        command.args(["new-session", "-d", "-s", &session]);
        command.arg("-c").arg(&repo);
        command.arg(&script);
        let status = command.status().map_err(|e| Error::Store(format!("failed to spawn tmux: {e}")))?;
        if !status.success() {
            let _ = fs::remove_file(&script);
            return Err(Error::Store(format!("tmux refused the session: {status}")));
        }
        // Put the run's identity in the SESSION environment too. The script
        // exports it into the one pane it execs, which leaves a new window
        // or split inside a live `corpus-*` session with no identity at
        // all — the server there fails closed, but the operator just sees
        // every tool refused. This makes a second pane behave like the
        // first. Session-scoped, so it dies with the session; it is a
        // convenience, not a control (the ceiling still comes from the
        // agent's sidecar, server-side).
        for (key, value) in &env {
            let _ = Command::new(&tmux)
                .args(["set-environment", "-t", &session, key, value])
                .status();
        }
        // Session-scoped mouse mode: the embedded pane forwards wheel as
        // SGR mouse reports, and tmux answers them with copy-mode scrollback
        // (without this there is NO way to scroll a run's history in-app).
        // Scoped to the session — the operator's global tmux config is
        // untouched.
        let _ = Command::new(&tmux)
            .args(["set-option", "-t", &session, "mouse", "on"])
            .status();
        // Raw pane capture FIRST (so early output is never missed).
        let _ = Command::new(&tmux)
            .args(["pipe-pane", "-t", &session])
            .arg(format!("cat >> {}", raw.display()))
            .status();
        Ok(Self {
            transcript: export_json.clone(),
            backend: Backend::Tui {
                session,
                // A resume already knows its session; a fresh spawn
                // discovers one once opencode has created it.
                tui_session_id: resume.map(str::to_string),
                launched_at_ms,
                stopped: false,
                exported: false,
                export_json,
                raw,
                script,
                file_pos: 0,
                pending: String::new(),
                liveness: (std::time::Instant::now(), true),
                discovery: std::time::Instant::now(),
                repo,
            },
        })
    }

    /// The piped headless backend: `opencode run` with streams pumped
    /// into the transcript and a line channel.
    fn start_piped(
        store: &Store,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: Option<&Path>,
        source_pins: Option<&str>,
    ) -> Result<Self> {
        let opencode = resolve_opencode()?;
        let runs = store.project_corpus_dir(project).join("runs");
        fs::create_dir_all(&runs)?;
        let (transcript, header) = match append_to {
            Some(path) => (path.to_path_buf(), None),
            None => {
                let (path, ts) = fresh_transcript_path(&runs, agent, mission);
                (path, Some(header_for(agent, model, mission, ts)))
            }
        };
        if let Some(header) = header {
            let mut log = fs::File::create(&transcript)?;
            log.write_all(header.as_bytes())?;
        }
        let mut command = opencode_command(&opencode, project, agent, model, mission)?;
        if let Some(pins) = source_pins {
            command.env(SOURCE_PINS_ENV, pins);
        }
        if let Some(name) = transcript.file_name() {
            command.env(RUN_LOG_ENV, name);
        }
        let mut child = command.spawn().map_err(|e| {
            Error::Store(format!("failed to spawn opencode (on PATH?): {e}"))
        })?;
        let (tx, rx) = mpsc::channel();
        let log = fs::OpenOptions::new().append(true).open(&transcript)?;
        let log = Arc::new(Mutex::new(log));
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Store("no stdout from opencode".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| Error::Store("no stderr from opencode".into()))?;
        pump(stdout, false, tx.clone(), Arc::clone(&log));
        pump(stderr, true, tx, Arc::clone(&log));
        Ok(Self {
            transcript,
            backend: Backend::Piped { child, rx },
        })
    }

    /// A transcript line already buffered, or None. TUI runs tail the
    /// raw capture; piped runs read the channel.
    pub fn poll_line(&mut self) -> Option<RunLine> {
        match &mut self.backend {
            Backend::Piped { rx, .. } => rx.try_recv().ok(),
            Backend::Tui {
                raw, file_pos, pending, ..
            } => poll_file(raw, file_pos, pending),
        }
    }

    /// A line within a deadline.
    pub fn poll_line_timeout(&mut self, timeout: Duration) -> Option<RunLine> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Some(line) = self.poll_line() {
                return Some(line);
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Non-blocking exit check. A TUI run exits when the operator stops
    /// it OR its tmux session dies (the operator quit opencode — the run
    /// is over even though nobody told the app); the piped run surfaces
    /// its real status.
    pub fn try_exit(&mut self) -> Option<ExitStatus> {
        match &mut self.backend {
            Backend::Piped { child, .. } => child.try_wait().ok().flatten(),
            Backend::Tui {
                stopped,
                session,
                liveness,
                ..
            } => {
                if *stopped {
                    Some(stop_exit_status())
                } else if !tui_session_live(session, liveness) {
                    // opencode exited on its own: a clean exit.
                    Some(ExitStatus::from_raw(0))
                } else {
                    None
                }
            }
        }
    }

    /// The command to attach a terminal to this run: `(program, args)`
    /// an external terminal shells out to. None when the run has no
    /// attach backend (the piped fallback). Retained for CLI use; the
    /// app embeds the terminal and uses [`RunSession::pty_attach_command`]
    /// instead.
    pub fn attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => attach_argv(session),
        }
    }

    /// The argv to attach an EMBEDDED PTY to this run (app chunk 7):
    /// plain `tmux attach -t <session>` — no terminal-app shell-out.
    /// None for the piped fallback (the pane then shows the tail).
    pub fn pty_attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => tui_attach_command(session),
        }
    }

    /// Stop: the ONE run teardown verb. Best-effort transcript-of-record
    /// export, then kill the run. Always succeeds (stopping is the
    /// operator's final word) and always returns the durable transcript
    /// path — the exported JSON when the export lands, else the raw
    /// capture (TUI) or .log (piped), both durable by design.
    pub fn stop(&mut self) -> PathBuf {
        match &mut self.backend {
            Backend::Piped { child, .. } => {
                kill_tree(child);
                self.transcript.clone()
            }
            Backend::Tui { .. } => {
                let fallback = match &self.backend {
                    Backend::Tui { raw, .. } => raw.clone(),
                    Backend::Piped { .. } => self.transcript.clone(),
                };
                let path = match self.export_transcript() {
                    Ok(path) => path,
                    Err(error) => {
                        // Best-effort, but not silent: a broken export
                        // must be diagnosable, not just an empty Cost panel.
                        eprintln!("corpus: transcript export on stop failed: {error}");
                        fallback
                    }
                };
                self.close_tui();
                if let Backend::Tui { stopped, .. } = &mut self.backend {
                    *stopped = true;
                }
                path
            }
        }
    }

    /// `opencode export <session-id>`: the newest session opened in the
    /// project dir since launch, written as clean JSON to
    /// `runs/<session-id>.json`. Keyed by session id (not launch epoch) so
    /// a re-export overwrites in place — usage is captured every turn, and
    /// one file per session is what keeps the cost aggregation from double
    /// counting. Returns the written path.
    fn export_transcript(&mut self) -> Result<PathBuf> {
        let Backend::Tui {
            export_json,
            exported,
            tui_session_id,
            launched_at_ms,
            repo,
            ..
        } = &mut self.backend
        else {
            return Ok(self.transcript.clone());
        };
        if *exported {
            return Ok(export_json.clone());
        }
        let id = match tui_session_id.clone() {
            Some(id) => id,
            None => {
                let found = find_opencode_session(repo.as_path(), *launched_at_ms, &BTreeSet::new())?;
                *tui_session_id = Some(found.clone());
                found
            }
        };
        let json = export_opencode_json(repo.as_path(), &id)?;
        // Key by opencode SESSION id, not the launch epoch: the turn-boundary
        // re-export must overwrite this file in place, never accumulate a
        // second double-counted copy. The runs/ dir is export_json's parent.
        let runs = export_json
            .parent()
            .ok_or_else(|| Error::Store("export path has no runs dir".into()))?
            .to_path_buf();
        fs::create_dir_all(&runs)?;
        let out = runs.join(format!("{id}.json"));
        fs::write(&out, json)?;
        *export_json = out.clone();
        *exported = true;
        Ok(out)
    }

    /// Kill the tmux session (the whole TUI process tree) and drop the
    /// temp script. The raw capture in the project corpus runs/ is KEPT:
    /// it is the durable run log.
    fn close_tui(&mut self) {
        let Backend::Tui {
            session, script, ..
        } = &self.backend
        else {
            return;
        };
        if let Some(tmux) = resolve_tmux() {
            let _ = Command::new(tmux)
                .args(["kill-session", "-t", session])
                .status();
        }
        let _ = fs::remove_file(script);
    }

    fn runs_for(store: &Store, project: &str, agent: &str, ts: u64, ext: &str) -> PathBuf {
        store.project_corpus_dir(project).join("runs")
            .join(format!("{ts}-{agent}.{ext}"))
    }
}


/// Resolve the effective launch model:
/// primary-agent model -> launch arg -> registry tool-use default -> refuse.
/// OpenCode's ambient default is never inherited.
fn resolve_launch_model(
    store: &Store,
    project: &str,
    agent: &str,
    launch_model: Option<&str>,
) -> Result<String> {
    let config = store
        .load_agent(project, agent)
        .map_err(|e| Error::Store(format!("agent {project}/{agent}: {e}")))?;
    let primary_model = primary_agent_model(&config.doc);
    let model = pick_model(primary_model.as_deref(), launch_model)
        .or_else(|| registry_default())
        .ok_or_else(|| {
            Error::Store(format!(
                "no model configured for agent {agent} on {project} — set one on \
                 the primary agent entry, pass an explicit model, or register a \
                 tool-use model in benchmarks/models.yaml; opencode's ambient \
                 default is never inherited"
            ))
        })?;
    Ok(model)
}

/// The model a launch would pre-fill from the agent config (primary -> registry
/// tool-use default); None when neither is set.
pub fn agent_default_model(store: &Store, project: &str, agent: &str) -> Option<String> {
    let config = store.load_agent(project, agent).ok()?;
    primary_agent_model(&config.doc)
        .or_else(registry_default)
}

/// The model declared on the primary agent's entry in the `agent` map.
fn primary_agent_model(doc: &serde_json::Value) -> Option<String> {
    let agents = doc.get("agent")?.as_object()?;
    for (_name, cfg) in agents {
        let mode = cfg.get("mode").and_then(|v| v.as_str()).unwrap_or("primary");
        if mode == "primary" {
            return cfg.get("model").and_then(|v| v.as_str()).map(str::to_string);
        }
    }
    None
}

/// The registry's tool-use default (the first tool-use entry, or the first
/// model). This IS an explicit model id; it replaces the old template
/// default (templates are gone).
fn registry_default() -> Option<String> {
    let path = std::env::var("CORPUS_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("benchmarks/models.yaml"));
    ModelRegistry::load(&path)
        .ok()?
        .launch_default()
}

/// First non-empty of two ordered options (primary -> arg).
fn pick_model(primary: Option<&str>, arg: Option<&str>) -> Option<String> {
    [primary, arg]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|m| !m.is_empty())
        .map(str::to_string)
}

/// Write the tiny run script the tmux pane executes. The script CARRIES
/// its own command AND its environment: every dynamic value is embedded
/// as a shell-escaped export (single-quoted, with `'` escaping), so no
/// mission text can break out, and there is no dependency on tmux `-e`
/// per-session env (which a freshly-started server may drop).
/// An EMPTY `prompt` spawns a bare opencode TUI (no `--prompt`), so the
/// operator types the mission into opencode's own input.
fn write_tui_script(
    script: &Path,
    params: &[(&str, &str)],
    prompt: Option<&str>,
    resume: Option<&str>,
) -> Result<()> {
    let mut out = String::from("#!/bin/sh\n");
    for (key, value) in params {
        out.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    let mut exec = make_exec_vars();
    // `--session <id>` re-opens an existing conversation with its history.
    if let Some(id) = resume {
        exec.push_str(&format!(" --session {}", shell_quote(id)));
    }
    if let Some(prompt) = prompt {
        exec.push_str(&format!(" --prompt {}", shell_quote(prompt)));
    }
    out.push_str(&format!("exec {exec}\n"));
    fs::write(script, out)?;
    let mut perms = fs::metadata(script)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(script, perms)?;
    Ok(())
}

/// The `--agent/--model` prefix shared by every spawn: the agent and model
/// are always explicit (opencode's ambient default is never inherited).
/// `--agent` is the display HANDLE (a name), not the dir slug the server
/// identifies the run by (`$CORPUS_OPENCODE_AGENT`).
fn make_exec_vars() -> String {
    "\"$CORPUS_OPENCODE_BIN\" --agent \"$CORPUS_OPENCODE_HANDLE\" --model \"$CORPUS_OPENCODE_MODEL\"".to_string()
}

/// Single-quote a dynamic value so it is inert inside the run script.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Find the newest opencode session opened in `cwd` since `launched_at`
/// (ms) — our TUI's session id. The TUI command has no `--title`, so
/// the record is located by project dir + launch window instead.
fn find_opencode_session(
    cwd: &Path,
    launched_at_ms: u64,
    claimed: &BTreeSet<String>,
) -> Result<String> {
    let opencode = resolve_opencode()?;
    let output = Command::new(&opencode)
        .args(["session", "list", "--format", "json", "-n", "50"])
        .current_dir(cwd)
        .output()
        .map_err(|e| Error::Store(format!("opencode session list failed: {e}")))?;
    if !output.status.success() {
        return Err(Error::Store(
            "opencode session list reported an error".into(),
        ));
    }
    let list: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Store(format!("opencode session list gave bad JSON: {e}")))?;
    let dir = cwd.to_string_lossy();
    let mut best: Option<(u64, String)> = None;
    for entry in list.as_array().unwrap_or(&Vec::new()) {
        let in_dir = entry
            .get("directory")
            .and_then(|d| d.as_str())
            .map(|d| d == dir)
            .unwrap_or(false);
        if !in_dir {
            continue;
        }
        let (Some(created), Some(id)) = (
            entry.get("created").and_then(|c| c.as_u64()),
            entry.get("id").and_then(|i| i.as_str()),
        ) else {
            continue;
        };
        // Sessions this run cannot own: created before it started, or
        // already bound to another mission. Both matter now that runs
        // overlap — they share one cwd, so the listing shows every
        // concurrent mission's conversation.
        if created < launched_at_ms || claimed.contains(id) {
            continue;
        }
        // OLDEST, not newest. The first session to appear after a launch
        // is that launch's own; picking the newest handed a slow-booting
        // run the conversation of a mission started after it.
        if best.as_ref().map(|(c, _)| created < *c).unwrap_or(true) {
            best = Some((created, id.to_string()));
        }
    }
    best.map(|(_, id)| id)
        .ok_or_else(|| Error::Store("no opencode session found for this launch".into()))
}

/// `opencode export <id>` -> the pretty JSON transcript. The JSON is on
/// stdout; the "Exporting…" chatter is on stderr.
fn export_opencode_json(cwd: &Path, session_id: &str) -> Result<String> {
    let opencode = resolve_opencode()?;
    let output = Command::new(&opencode)
        .arg("export")
        .arg(session_id)
        .current_dir(cwd)
        .output()
        .map_err(|e| Error::Store(format!("opencode export failed: {e}")))?;
    if !output.status.success() {
        return Err(Error::Store("opencode export reported an error".into()));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| Error::Store(format!("opencode export gave bad JSON: {e}")))?;
    serde_json::to_string_pretty(&value)
        .map_err(|e| Error::Store(format!("cannot serialize export: {e}")))
}

/// The attach argv for a terminal: focus the app, then run the attach
/// in a fresh window. macOS uses osascript so the window lands on top.
fn attach_argv(session: &str) -> Option<Vec<String>> {
    let tmux = resolve_tmux()?.display().to_string();
    #[cfg(target_os = "macos")]
    {
        let app = std::env::var("CORPUS_TERMINAL").unwrap_or_else(|_| {
            match std::env::var("TERM_PROGRAM").as_deref().ok() {
                Some("iTerm2") => "iTerm".to_string(),
                Some("WezTerm") => "WezTerm".to_string(),
                _ => "Terminal".to_string(),
            }
        });
        let command = format!("{tmux} attach -t {session}");
        Some(vec![
            "osascript".to_string(),
            "-e".to_string(),
            format!("tell application \"{app}\" to activate"),
            "-e".to_string(),
            format!("tell application \"{app}\" to do script \"{command}\""),
        ])
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(vec![tmux, "attach".to_string(), "-t".to_string(), session.to_string()])
    }
}

/// The plain `tmux attach -t <session>` argv for an embedded PTY (app
/// chunk 7): the embedded terminal never spawns opencode — tmux stays
/// the supervisor, the pane is just another client.
pub fn tui_attach_command(session: &str) -> Option<Vec<String>> {
    let tmux = resolve_tmux()?.display().to_string();
    Some(vec![
        tmux,
        "attach".to_string(),
        "-t".to_string(),
        session.to_string(),
    ])
}

/// Live corpus run sessions on the tmux server (the `corpus-` prefix) —
/// the app's re-attach list after a relaunch: a run outlives the app
/// by design, so a reopened app offers these for in-pane attach.
/// Empty on any failure (no tmux, no server running).
pub fn live_tui_sessions() -> Vec<String> {
    let Some(tmux) = resolve_tmux() else {
        return Vec::new();
    };
    let Ok(output) = Command::new(tmux)
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new(); // no server up — no live runs
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|name| name.starts_with("corpus-"))
        .map(str::to_string)
        .collect()
}

/// The opencode conversation a LIVE tmux session is hosting, discovered
/// without a `RunSession` handle.
///
/// Needed because a handle is not the same thing as a live run. A run this
/// process re-attached to after a restart never had one, and a run
/// displaced by a later launch stops having one — yet both are still up,
/// and a mission holding neither a handle nor a conversation id is an
/// orphan: live in tmux, but impossible to export or resume.
///
/// The launch stamp comes from the session name, which `start_tui` builds
/// from the same instant it records as `launched_at_ms`.
pub fn session_conversation(
    store: &Store,
    project: &str,
    session: &str,
    claimed: &BTreeSet<String>,
) -> Option<String> {
    let ts = session_stamp(session)?;
    let cwd = store.project_run_dir(project);
    find_opencode_session(&cwd, ts.saturating_mul(1000), claimed).ok()
}

/// The launch stamp in `corpus-<agent>-<ts>`. `<agent>` may itself contain
/// `-`, so the stamp splits off the TAIL.
fn session_stamp(session: &str) -> Option<u64> {
    let stem = session.strip_prefix("corpus-")?;
    let (agent, ts) = stem.rsplit_once('-')?;
    if agent.is_empty() {
        return None;
    }
    ts.parse::<u64>().ok()
}

/// The raw capture a TUI session appends to, derived from the session
/// name: `start_tui` builds both from the same launch stamp, so
/// `corpus-<agent>-<ts>` pairs with `runs/<ts>-<agent>.raw`. Lets the app
/// find the log of a run it does NOT own (re-attached after a relaunch)
/// without a handle. None when the name isn't ours or carries no stamp.
pub fn session_raw_log(store: &Store, project: &str, session: &str) -> Option<PathBuf> {
    let ts = session_stamp(session)?;
    let (agent, _) = session.strip_prefix("corpus-")?.rsplit_once('-')?;
    Some(
        store
            .project_corpus_dir(project)
            .join(crate::store::RUNS)
            .join(format!("{ts}-{agent}.raw")),
    )
}

/// How long a run's TUI has been producing NOTHING, in seconds — the
/// "is the agent actually working" signal.
///
/// A live opencode TUI is not the same as a busy one: a session sits at
/// its prompt indefinitely waiting on the operator. But everything the
/// TUI paints (streamed tokens, the working spinner, tool output) flows
/// through `tmux pipe-pane` into the run's raw capture, and an idle TUI
/// paints nothing at all — so the capture's mtime IS the last moment the
/// agent did something. Costs one `stat`, no subprocess.
///
/// None when there is no capture yet (a just-launched run that hasn't
/// printed) or the mtime is unreadable — callers treat that as "not
/// working" rather than guessing.
pub fn run_idle_secs(log: &Path) -> Option<u64> {
    let modified = fs::metadata(log).ok()?.modified().ok()?;
    // A capture written a hair in the future (clock skew) reads as 0.
    Some(modified.elapsed().map(|d| d.as_secs()).unwrap_or(0))
}

/// How recently a run's TUI must have painted for the agent to count as
/// WORKING. opencode animates while a turn is in flight (spinner, token
/// stream, tool output), so a live-but-quiet capture for this long means
/// the turn is over and it's waiting. Long enough to ride out a slow frame,
/// short enough that the state settles as soon as the answer lands.
pub const WORKING_WINDOW_SECS: u64 = 3;

/// What a mission is actually doing right now — the signal behind the app's
/// sidebar dots, and what the curator polls to pace a team. Derived live
/// from tmux + the raw capture; never persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionActivity {
    /// No live session — the mission is not up.
    Idle,
    /// The session is live but quiet: the turn is finished and it is
    /// waiting on the operator (or on a reply). Not work.
    Waiting,
    /// The agent is producing right now — streaming, spinning, running
    /// tools.
    Working,
}

/// The pure activity rule, split out so it is testable and SHARED by the
/// app (which feeds an aged in-memory reading) and the one-shot
/// `mission_run_state` (which stats the capture fresh): a live session is
/// only `Working` when something was painted within `WORKING_WINDOW_SECS`.
/// No reading at all (`None`) is NOT evidence of work — it falls through to
/// `Waiting`, never `Working` (the case that used to pulse forever).
pub fn activity_from_idle(live: bool, idle_secs: Option<u64>) -> MissionActivity {
    if !live {
        return MissionActivity::Idle;
    }
    match idle_secs {
        Some(secs) if secs < WORKING_WINDOW_SECS => MissionActivity::Working,
        _ => MissionActivity::Waiting,
    }
}

/// A mission's live run state: its activity plus how long since its TUI
/// last painted (`None` = no live capture to read).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissionRunState {
    pub activity: MissionActivity,
    pub idle_secs: Option<u64>,
}

/// Compute a mission's live run state ONE-SHOT — the same answer the app's
/// dots show, from any process that has the store + tmux (the curator's MCP
/// server). `live` is the current tmux listing (`live_tui_sessions()`),
/// passed in so a caller reporting many missions shells out once. A mission
/// whose recorded `session` is not live — or that has none (a piped
/// headless run, which only the app can see) — reads as `Idle`.
pub fn mission_run_state(
    store: &Store,
    project: &str,
    mission: &crate::store::Mission,
    live: &[String],
) -> MissionRunState {
    let is_live = mission
        .session
        .as_deref()
        .is_some_and(|s| live.iter().any(|l| l == s));
    let idle_secs = mission
        .session
        .as_deref()
        .filter(|_| is_live)
        .and_then(|s| session_raw_log(store, project, s))
        .and_then(|log| run_idle_secs(&log));
    MissionRunState {
        activity: activity_from_idle(is_live, idle_secs),
        idle_secs,
    }
}

/// Kill a corpus tmux session (Stop for a re-attached run). No-op when
/// tmux is unavailable; the session may already be dead.
pub fn kill_tmux_session(session: &str) {
    if let Some(tmux) = resolve_tmux() {
        let _ = Command::new(tmux)
            .args(["kill-session", "-t", session])
            .status();
    }
}

/// Is this tmux session still alive? Throttled: the check is a
/// subprocess spawn, so `try_exit` polls at most once a second and
/// otherwise trusts the cached verdict. tmux itself unresolvable is
/// treated as LIVE (never declare a run dead on a tooling failure).
fn tui_session_live(session: &str, cache: &mut (std::time::Instant, bool)) -> bool {
    if cache.0.elapsed() < Duration::from_secs(1) {
        return cache.1;
    }
    let live = match resolve_tmux() {
        Some(tmux) => Command::new(tmux)
            .args(["has-session", "-t", session])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(true),
        None => true,
    };
    *cache = (std::time::Instant::now(), live);
    live
}

/// Export an opencode session's transcript of record to the project
/// corpus `runs/<session-id>.json`. Used both on turn completion (the app
/// re-exports a live conversation every time it settles into `Waiting`) and
/// on Stop for a re-attached run with no app-owned handle. Keyed by session
/// id so repeated exports overwrite in place — one file per session keeps
/// the cost aggregation from double counting. Reuses the pretty-export
/// internals of the TUI backend.
pub fn export_session(project: &str, opencode_session_id: &str) -> Result<PathBuf> {
    let store = Store::from_env();
    let repo = store.provision_run_dir(project)?;
    let json = export_opencode_json(&repo, opencode_session_id)?;
    let path = store
        .project_corpus_dir(project)
        .join("runs")
        .join(format!("{opencode_session_id}.json"));
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, json)?;
    Ok(path)
}

/// Signal-shaped exit for a stopped TUI run (the transcript documents
/// the rest).
fn stop_exit_status() -> ExitStatus {
    ExitStatus::from_raw(130 << 8)
}

/// Tail a file from the last consumed offset, emitting one complete
/// line per call (the app's live tail over pipe-pane raw capture).
fn poll_file(
    path: &Path,
    file_pos: &mut u64,
    pending: &mut String,
) -> Option<RunLine> {
    loop {
        let Ok(meta) = fs::metadata(path) else {
            return None;
        };
        let len = meta.len();
        if len < *file_pos {
            *file_pos = 0;
            pending.clear();
        }
        if len <= *file_pos {
            return None;
        }
        let mut buf = vec![0u8; (len - *file_pos) as usize];
        let Ok(mut file) = fs::File::open(path) else {
            return None;
        };
        if file.seek(SeekFrom::Start(*file_pos)).is_err() || file.read_exact(&mut buf).is_err() {
            return None;
        }
        *file_pos = len;
        pending.push_str(&String::from_utf8_lossy(&buf));
        if let Some(end) = pending.find(['\n', '\r']) {
            let line = pending[..end].to_string();
            let consumed = if pending[end..].starts_with("\r\n") { end + 2 } else { end + 1 };
            pending.drain(..consumed);
            return Some(RunLine {
                stderr: false,
                text: line,
            });
        }
        if pending.len() > 16 * 1024 {
            let line = std::mem::take(pending);
            return Some(RunLine { stderr: false, text: line });
        }
    }
}

/// Resolve the opencode binary WITHOUT assuming PATH (GUI-launched
/// apps get a minimal PATH on macOS). Tried: PATH, `~/.opencode/bin`,
/// the repo-local `node_modules/.bin`.
pub(crate) fn resolve_opencode() -> Result<PathBuf> {
    if let Some(found) = on_path("opencode") {
        return Ok(found);
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(format!("{home}/.opencode/bin/opencode"));
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    if let Some(resources) = crate::paths::resource_root_opt() {
        let candidate = resources
            .join(".opencode")
            .join("node_modules")
            .join(".bin")
            .join("opencode");
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    Err(Error::Store(
        "opencode binary not found — tried PATH, ~/.opencode/bin/opencode, \
         .opencode/node_modules/.bin/opencode. Install it or put it on PATH."
            .into(),
    ))
}

/// Whether tmux exists AND is new enough to take `-e` on new-session
/// (3.2a+). `CORPUS_NO_TMUX=1` forces the piped fallback.
fn tmux_available() -> Option<()> {
    if std::env::var("CORPUS_NO_TMUX").as_deref() == Ok("1") {
        return None;
    }
    let tmux = resolve_tmux()?;
    let output = Command::new(tmux).arg("-V").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let field = text.split_whitespace().nth(1)?.trim_start_matches("next-");
    let major: u32 = field.split('.').next()?.parse().ok()?;
    let minor: u32 = field
        .split('.')
        .nth(1)
        .and_then(|m| {
            m.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse()
                .ok()
        })
        .unwrap_or(0);
    if (major, minor) < (3, 2) {
        return None;
    }
    Some(())
}

/// Resolve the tmux binary WITHOUT assuming PATH: PATH first, then the
/// homebrew/ports/system locations. Cached per process.
fn resolve_tmux() -> Option<PathBuf> {
    static TMUX: OnceLock<Option<PathBuf>> = OnceLock::new();
    TMUX.get_or_init(|| {
        if let Some(found) = on_path("tmux") {
            return Some(found);
        }
        ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"]
            .iter()
            .map(|dir| PathBuf::from(dir).join("tmux"))
            .find(|candidate| is_executable(candidate))
    })
    .clone()
}

/// An executable-ish file named `name` on PATH (no execution probe —
/// the binary may be a long-running agent stub).
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    path.is_file()
        && fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

/// The piped `opencode run` command.
fn opencode_command(
    opencode: &Path,
    project: &str,
    agent: &str,
    model: Option<&str>,
    mission: &str,
) -> Result<Command> {
    let mut command = Command::new(opencode);
    let agent_stem = crate::store::slugify(agent);
    let store = Store::from_env();
    // opencode loads `--agent` by the rendered handle (a name); the server
    // still identifies the run by the dir slug via `AGENT_ENV` below.
    let handle = opencode_agent_handle(&store, project, agent);
    command.args(["run", "--agent", &handle]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(model) = model {
        command.args(["-m", model]);
    }
    // An EMPTY mission launches a bare `opencode run` TUI (no prompt).
    if !mission.trim().is_empty() {
        command.arg(mission);
    }
    // The run's cwd is the PROJECT's run dir (own .opencode/agent set, own
    // opencode session pool, exactly one project's store subtree visible).
    // A provisioning failure REFUSES the launch: it used to fall back to
    // the repo root, where opencode discovers the repo's own agent set and
    // the whole store — precisely the boundary this cwd exists to enforce.
    let cwd = store.provision_run_dir(project)?;
    command.current_dir(cwd);
    command
        .env(PROJECT_ENV, project)
        .env(STORE_ENV, store.root())
        // The agent identity must reach the MCP server, not just opencode:
        // corpus-mcp inherits this environment and resolves the run's agent
        // (and its role) from it. The TUI path exports the same var via its
        // launch script; without it here, every headless run is anonymous
        // to the server.
        .env(AGENT_ENV, &agent_stem);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    Ok(command)
}

/// The materialized agent file stem: bare (slugified) — no team prefix.
pub fn agent_file_stem(agent: &str) -> String {
    crate::store::slugify(agent)
}

/// The opencode `--agent` handle for a launched agent: its project-unique,
/// name-derived identifier (see [`crate::primary_handles`]). This is what
/// opencode shows and resolves the rendered `.opencode/agent/<handle>.md`
/// by, so it must match what the renderer wrote. Falls back to the dir slug
/// when the project can't be listed — the same value the renderer uses for
/// an unnamed agent.
pub fn opencode_agent_handle(store: &Store, project: &str, slug: &str) -> String {
    store
        .list_agents(project)
        .ok()
        .and_then(|agents| crate::primary_handles(&agents).remove(slug))
        .unwrap_or_else(|| crate::store::slugify(slug))
}

/// A fresh transcript path + the epoch for headless runs.
fn fresh_transcript_path(runs: &Path, agent: &str, mission: &str) -> (PathBuf, u64) {
    let ts = now_secs();
    let slug: String = mission
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");
    (runs.join(format!("{ts}-{agent}-{slug}.log")), ts)
}

fn header_for(agent: &str, model: Option<&str>, mission: &str, ts: u64) -> String {
    format!(
        "# corpus run\n# agent: {agent}\n# model: {}\n# started: {ts}\n# mission: {mission}\n\n",
        model.unwrap_or("(default)")
    )
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Pump one child stream into the transcript file AND the line channel.
fn pump<R>(
    stream: R,
    stderr: bool,
    tx: mpsc::Sender<RunLine>,
    log: Arc<Mutex<fs::File>>,
) -> std::thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    std::thread::spawn(move || {
        for line in BufReader::new(stream).lines() {
            let Ok(line) = line else { break };
            if let Ok(mut log) = log.lock() {
                let _ = writeln!(log, "{line}");
            }
            let _ = tx.send(RunLine {
                stderr,
                text: line,
            });
        }
    })
}

/// Kill a child and its whole process group (unix).
fn kill_tree(child: &mut Child) {
    #[cfg(unix)]
    {
        let pgid = child.id().to_string();
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pgid}")])
            .status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::AgentRole;
    use std::sync::MutexGuard;

    /// The env- and process-mutating launch tests are inherently global
    /// (CORPUS_STORE/PATH, tmux sessions, stray processes), so they run
    /// under one shared lock instead of racing the parallel test pool.
    static ENV_LOCK: OnceLock<std::sync::Mutex<()>> = OnceLock::new();
    pub fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .get_or_init(|| std::sync::Mutex::new(()))
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A store in its OWN world: the run dir is a sibling of the store
    /// (`<store parent>/var/run/<project>`), so temp stores that shared a
    /// parent — every one of them, when the parent was `/tmp` — collided
    /// on the run dir of any project with the same slug.
    fn tmp_store(tag: &str) -> (Store, PathBuf) {
        let world =
            std::env::temp_dir().join(format!("corpus-launch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        let dir = world.join("store");
        (Store::new(dir.clone()), dir)
    }

    /// A project with the two agents the launch tests exercise. Projects
    /// no longer come with agents — they are created from a role.
    fn core_project(store: &Store) {
        store.create_project("default", "Default", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("default", "operator", AgentRole::Tester)
            .unwrap();
        store
            .create_agent_with_role("default", "researcher", AgentRole::Researcher)
            .unwrap();
    }

    #[test]
    fn agent_names_are_bare() {
        assert_eq!(agent_file_stem("operator"), "operator");
        assert_eq!(agent_file_stem("Flow Agent"), "flow-agent");
        assert_eq!(agent_file_stem("My Auditor"), "my-auditor");
    }

    #[test]
    fn launch_model_precedence_and_loud_failure() {
        // primary wins, then arg.
        assert_eq!(
            pick_model(Some("inst"), Some("arg")).as_deref(),
            Some("inst")
        );
        assert_eq!(
            pick_model(None, Some("arg")).as_deref(),
            Some("arg")
        );
        // empty values are skipped, whitespace trimmed
        assert_eq!(
            pick_model(None, Some("  ")),
            None
        );
    }

    /// Integration test (env-locked): the spawn/stop machinery
    /// runs against a temp store seeded with the core agent pair,
    /// exercising the v2 teamless paths.
    #[test]
    fn spawn_stop_and_piped_headless() {
        let _guard = env_lock();
        let _ = Command::new("pkill").args(["-f", "sleep 90127"]).status();
        let bin = std::env::temp_dir().join(format!("corpus-fake-bin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        fs::write(&fake, "#!/bin/sh\nsleep 90127\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = std::env::var("PATH").unwrap_or_default();
        path = format!("{}:{}", bin.display(), path);
        std::env::set_var("PATH", &path);

        let (store, store_dir) = tmp_store("stop-v2");
        std::env::set_var("CORPUS_STORE", &store_dir);
        core_project(&store);

        let mut session = RunSession::spawn_headless("default", "operator", None, "probe")
            .expect("piped headless spawn");
        assert!(session.transcript.is_file(), "transcript starts at spawn");
        std::thread::sleep(Duration::from_millis(800));

        let started = std::time::Instant::now();
        let stopped_at = session.stop();
        assert_eq!(stopped_at, session.transcript, "stop returns the transcript");
        let mut exited = false;
        while started.elapsed() < Duration::from_secs(5) {
            if session.try_exit().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "stop reaps the run within 5s");
        let alive = Command::new("pgrep")
            .args(["-f", "sleep 90127"])
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        assert!(!alive, "no orphaned grandchildren");

        // Transcript is in the project corpus runs/.
        let runs_dir = store.project_corpus_dir("default").join("runs");
        assert!(runs_dir.join(session.transcript.file_name().unwrap()).exists(),
            "transcript in project corpus");

        std::env::remove_var("CORPUS_STORE");
        std::env::remove_var("PATH");
        let _ = fs::remove_dir_all(&bin);
        let _ = fs::remove_dir_all(&store_dir);
    }

    /// TUI backend: the pipe-pane raw capture is a durable corpus
    /// artifact — it lands in the project corpus runs/ (never /tmp) and
    /// survives stop/close.
    #[test]
    fn tui_raw_capture_is_durable_in_project_corpus() {
        let _guard = env_lock();
        if tmux_available().is_none() {
            return; // no tmux on this host — nothing to exercise
        }
        let _ = Command::new("pkill").args(["-f", "sleep 90128"]).status();
        let bin = std::env::temp_dir().join(format!("corpus-fake-tui-bin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        fs::write(&fake, "#!/bin/sh\nsleep 90128\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = std::env::var("PATH").unwrap_or_default();
        path = format!("{}:{}", bin.display(), path);
        std::env::set_var("PATH", &path);

        let (store, store_dir) = tmp_store("tui-raw");
        std::env::set_var("CORPUS_STORE", &store_dir);
        core_project(&store);

        let mut session =
            RunSession::spawn("default", "operator", Some("test/model"), "probe", None)
                .expect("tui spawn");
        let raw = match &session.backend {
            Backend::Tui { raw, .. } => raw.clone(),
            _ => panic!("expected the TUI backend (tmux is available)"),
        };
        let runs_dir = store.project_corpus_dir("default").join("runs");
        assert_eq!(
            raw.parent(),
            Some(runs_dir.as_path()),
            "raw capture lives in the project corpus runs/, not /tmp"
        );
        assert_eq!(raw.extension().and_then(|e| e.to_str()), Some("raw"));

        // Simulate pane output, then stop: the run log must survive.
        fs::write(&raw, "pane output\n").unwrap();
        session.stop();
        assert!(raw.exists(), "stop keeps the durable run log");

        std::env::remove_var("CORPUS_STORE");
        std::env::remove_var("PATH");
        let _ = fs::remove_dir_all(&bin);
        let _ = fs::remove_dir_all(&store_dir);
    }

    /// BOTH launch paths must carry the run's agent identity in the
    /// ENVIRONMENT, not just as a CLI arg: corpus-mcp inherits the env and
    /// resolves the agent's role from it, so a path that omits it leaves
    /// the server unable to tell a researcher from an operator. The piped
    /// path used to pass `--agent` only, which is invisible to the server.
    #[test]
    fn both_launch_paths_export_the_agent_identity() {
        let _guard = env_lock();
        let (store, store_dir) = tmp_store("agent-env");
        std::env::set_var("CORPUS_STORE", &store_dir);
        // A run dir belongs to a project, and provisioning now refuses to
        // invent one.
        store.create_project("default", "D", "cdk-regtest").unwrap();

        // Piped path: the identity rides the child's environment.
        let command = opencode_command(
            Path::new("/bin/echo"),
            "default",
            "discover",
            Some("test/model"),
            "probe",
        )
        .expect("provision the run dir");
        let exported: Vec<(String, String)> = command
            .get_envs()
            .filter_map(|(k, v)| {
                Some((k.to_string_lossy().into_owned(), v?.to_string_lossy().into_owned()))
            })
            .collect();
        let agent_env = exported.iter().find(|(k, _)| k == AGENT_ENV);
        assert_eq!(
            agent_env.map(|(_, v)| v.as_str()),
            Some("discover"),
            "the piped path must export {AGENT_ENV}; exported: {exported:?}"
        );

        // TUI path: the identity is exported by the launch script.
        let script = std::env::temp_dir().join(format!("corpus-idscript-{}.sh", std::process::id()));
        write_tui_script(&script, &[(AGENT_ENV, "discover")], None, None).unwrap();
        let body = fs::read_to_string(&script).unwrap();
        assert!(
            body.contains(&format!("export {AGENT_ENV}='discover'")),
            "the TUI path must export {AGENT_ENV}: {body}"
        );
        let _ = fs::remove_file(&script);

        std::env::remove_var("CORPUS_STORE");
        let _ = fs::remove_dir_all(&store_dir);
    }

    /// The run script's exec line: agent+model always explicit, and
    /// `--session` only when resuming an existing conversation.
    #[test]
    fn tui_script_carries_session_and_prompt_flags() {
        let dir = std::env::temp_dir().join(format!("corpus-script-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.sh");
        let read = |script: &Path| fs::read_to_string(script).unwrap();

        // Bare launch: no --session, no --prompt.
        write_tui_script(&script, &[], None, None).unwrap();
        let bare = read(&script);
        assert!(bare.contains("--agent \"$CORPUS_OPENCODE_HANDLE\""), "{bare}");
        assert!(!bare.contains("--session"), "a fresh launch resumes nothing: {bare}");
        assert!(!bare.contains("--prompt"), "{bare}");

        // Resume: the recorded id re-opens that conversation.
        write_tui_script(&script, &[], None, Some("ses_ff783a74dffeyTn76osPWIUX3L")).unwrap();
        let resumed = read(&script);
        assert!(
            resumed.contains("--session 'ses_ff783a74dffeyTn76osPWIUX3L'"),
            "{resumed}"
        );

        // A session id is quoted like every other dynamic value, so a
        // hostile one cannot break out of the exec line.
        write_tui_script(&script, &[], Some("go"), Some("x'; rm -rf /tmp/nope; #")).unwrap();
        let quoted = read(&script);
        assert!(quoted.contains(r"'x'\''; rm -rf /tmp/nope; #'"), "{quoted}");
        assert!(quoted.contains("--prompt 'go'"), "{quoted}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// The session name round-trips to the raw capture `start_tui`
    /// writes — the app finds the log of a run it never owned.
    #[test]
    fn session_raw_log_pairs_with_the_launch_stamp() {
        let (store, dir) = tmp_store("raw-pair");
        let runs = store.project_corpus_dir("p").join("runs");
        assert_eq!(
            session_raw_log(&store, "p", "corpus-discover-1786911614"),
            Some(runs.join("1786911614-discover.raw"))
        );
        // An agent stem with its own dashes keeps them: only the trailing
        // stamp is split off.
        assert_eq!(
            session_raw_log(&store, "p", "corpus-web-scanner-1786911614"),
            Some(runs.join("1786911614-web-scanner.raw"))
        );
        // Not ours / no stamp / no agent: no guessing.
        assert_eq!(session_raw_log(&store, "p", "my-editor"), None);
        assert_eq!(session_raw_log(&store, "p", "corpus-discover-later"), None);
        assert_eq!(session_raw_log(&store, "p", "corpus-1786911614"), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn activity_rule_maps_liveness_and_idle_to_state() {
        use MissionActivity::*;
        // Not live is always Idle, whatever the reading.
        assert_eq!(activity_from_idle(false, Some(0)), Idle);
        assert_eq!(activity_from_idle(false, None), Idle);
        // Live + painted within the window = Working.
        assert_eq!(activity_from_idle(true, Some(0)), Working);
        assert_eq!(activity_from_idle(true, Some(WORKING_WINDOW_SECS - 1)), Working);
        // Live but quiet past the window = Waiting (not Working).
        assert_eq!(activity_from_idle(true, Some(WORKING_WINDOW_SECS)), Waiting);
        assert_eq!(activity_from_idle(true, Some(600)), Waiting);
        // Live with no reading is absence of evidence, not work.
        assert_eq!(activity_from_idle(true, None), Waiting);
    }

    #[test]
    fn mission_run_state_is_idle_without_a_live_session() {
        let (store, dir) = tmp_store("run-state");
        let mut mission = crate::store::Mission {
            agent: "recon".to_string(),
            pins: Default::default(),
            budget: None,
            created: 0,
            name: None,
            session: None,
            opencode_session: None,
            launch_requested: None,
        };
        // No session at all → Idle, no reading.
        let s = mission_run_state(&store, "p", &mission, &[]);
        assert_eq!(s.activity, MissionActivity::Idle);
        assert_eq!(s.idle_secs, None);
        // A recorded session that is NOT in the live listing → still Idle.
        mission.session = Some("corpus-recon-1786911614".to_string());
        let s = mission_run_state(&store, "p", &mission, &["corpus-other-1".to_string()]);
        assert_eq!(s.activity, MissionActivity::Idle);
        let _ = fs::remove_dir_all(&dir);
    }

    /// `run_idle_secs` reads the capture's age — the busy signal. A
    /// missing capture is None (no evidence), a fresh write is ~0s.
    #[test]
    fn run_idle_secs_reads_capture_age() {
        let dir = std::env::temp_dir().join(format!("corpus-idle-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("run.raw");
        assert_eq!(run_idle_secs(&log), None, "no capture yet is not activity");
        fs::write(&log, "painting\n").unwrap();
        assert!(run_idle_secs(&log).unwrap() < 2, "a just-written capture reads as fresh");
        let _ = fs::remove_dir_all(&dir);
    }

    /// materialize_agent renders the launched agent's files into
    /// `.opencode/agent/` with bare names.
    #[test]
    fn materialize_agent_renders_agent_files() {
        let (store, dir) = tmp_store("mat-v2");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", AgentRole::Tester)
            .unwrap();
        let written = store.render_agent("p", "operator").unwrap();
        assert!(!written.is_empty());
        let dest = &written[0];
        assert!(dest.ends_with("operator.md"), "{dest:?}");
        let text = fs::read_to_string(dest).unwrap();
        assert!(text.contains("mode: primary"), "{text}");
        assert!(text.contains("corpus TESTER"), "the role's prompt: {text}");
        let _ = fs::remove_dir_all(&dir);
    }
}