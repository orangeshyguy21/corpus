//! Run-launch seam (deck-flow chunk 5): materialize a team's agents to
//! `.opencode/agent/`, then launch an opencode mission on the team scope
//! — shared by the CLI (`corpus run`, headless) and the deck.
//!
//! Naming scheme: `<team>-<agent>.md`, except the default project/team
//! which keeps the bare agent names for backward compatibility; the
//! spawned opencode inherits CORPUS_PROJECT / CORPUS_TEAM so the MCP
//! server routes writes into the right corpus.
//!
//! Supervisor + full-TUI decision (2026-08-11, corrected same-day):
//! an interactive launch spawns the REAL opencode TUI in a DETACHED
//! tmux session (`opencode --agent <a> --model <m> --prompt "<mission>"`)
//! — a detached session IS the headless mode, so attach/detach/close/
//! re-attach never kill the run, and attaching shows a steerable TUI,
//! not a one-shot `[exited]` dump. The TUI has no stdout, so:
//!   - tail           = `tmux pipe-pane` raw capture (ANSI-stripped for
//!                      the deck); the fallback transcript, not the
//!                      record;
//!   - record         = `opencode export <id>` (the newest session in the
//!                      project dir) -> `<epoch>-<agent>.json` in the
//!                      team corpus runs/ on Dismiss/abort;
//!   - completion     = operator-driven: a TUI session doesn't exit, a
//!                      run stays live until Dismiss (export + close) or
//!                      Abort (best-effort export + `tmux kill-session`).
//! The deck NEVER inherits opencode's ambient default model: the model
//! is resolved agent instance -> agent template -> explicit launch arg,
//! and a launch with none fails loudly instead of spawning.
//!
//! Headless `opencode run` stays for automation (`corpus run`,
//! scripted missions): the piped spawn behind the same handle. It is
//! also the no-tmux fallback for the deck (attach greys).

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
use crate::store::{Store, TeamSpec, PROJECT_ENV, STORE_ENV, TEAM_ENV};

/// One transcript line. In the piped backend the two child streams are
/// kept apart; in the TUI backend lines come from the raw capture, so
/// `stderr` is always false there.
#[derive(Debug, Clone)]
pub struct RunLine {
    pub stderr: bool,
    pub text: String,
}

/// Where the run actually executes. The deck/CLI never branch on this —
/// only `attach_command()` / `abort()` / `dismiss()` are backend-shaped.
enum Backend {
    /// The full opencode TUI in a detached tmux session.
    Tui {
        session: String,
        /// Discovered opencode session id (newest in the project dir).
        tui_session_id: Option<String>,
        /// Epoch-millis when we spawned; the discovery window anchors here.
        launched_at_ms: u64,
        aborted: bool,
        exported: bool,
        /// `<epoch>-<agent>.json`: the exported transcript of record.
        export_json: PathBuf,
        /// `tmux pipe-pane` raw capture: the live tail source.
        raw: PathBuf,
        /// The tiny run script (temp file) the pane executed.
        script: PathBuf,
        file_pos: u64,
        pending: String,
    },
    /// No tmux / headless automation: `opencode run` piped directly.
    Piped {
        child: Child,
        rx: mpsc::Receiver<RunLine>,
    },
}

/// A running opencode mission on a team scope.
pub struct RunSession {
    /// The record file: the exported JSON for a TUI run, the .log for
    /// a piped one.
    pub transcript: PathBuf,
    backend: Backend,
}

impl RunSession {
    /// DECK launch: resolve the model (instance -> template -> arg,
    /// fail loudly if none), then run the FULL TUI in a detached tmux
    /// session, or the piped headless fallback when tmux is absent.
    pub fn spawn(
        project: &str,
        team: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
    ) -> Result<Self> {
        let store = Store::from_env();
        Self::ensure_runs_dir(&store, project, team);
        let model = resolve_launch_model(&store, project, team, agent, model)?;
        if tmux_available().is_some() {
            Self::start_tui(&store, project, team, agent, &model, mission)
        } else {
            Self::start_piped(&store, project, team, agent, Some(&model), mission, None)
        }
    }

    /// CLI automation: always the headless `opencode run` piped path.
    /// No model resolution — `-m` stays optional (scripted missions may
    /// lean on opencode's own default-resolver).
    pub fn spawn_headless(
        project: &str,
        team: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
    ) -> Result<Self> {
        let store = Store::from_env();
        Self::ensure_runs_dir(&store, project, team);
        Self::start_piped(&store, project, team, agent, model, mission, None)
    }

    /// CLI automation APPENDING to an existing transcript (the
    /// researcher follow-up pass).
    pub fn spawn_headless_append(
        project: &str,
        team: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: &Path,
    ) -> Result<Self> {
        let store = Store::from_env();
        Self::ensure_runs_dir(&store, project, team);
        Self::start_piped(
            &store,
            project,
            team,
            agent,
            model,
            mission,
            Some(append_to),
        )
    }

    fn ensure_runs_dir(store: &Store, project: &str, team: &str) {
        let scope = crate::store::Scope::new(project, team);
        let _ = fs::create_dir_all(scope.corpus_dir(store).join("runs"));
    }

    /// The full opencode TUI in a detached tmux session.
    fn start_tui(
        store: &Store,
        project: &str,
        team: &str,
        agent: &str,
        model: &str,
        mission: &str,
    ) -> Result<Self> {
        let opencode = resolve_opencode()?;
        let tmux = resolve_tmux().ok_or_else(|| Error::Store("tmux vanished".into()))?;
        let ts = now_secs();
        let session = format!("corpus-{}-{}-{ts}", team, slugify(agent));
        let export_json = Self::runs_for(store, project, team, agent, ts, "json");
        let temp = std::env::temp_dir();
        let raw = temp.join(format!("{session}.raw"));
        let script = temp.join(format!("{session}.sh"));

        let repo = repo_root(store);
        // The script carries the command AND its environment: explicit,
        // escaped exports — no `-e` races on a freshly-started server.
        write_tui_script(
            &script,
            &[
                ("CORPUS_OPENCODE_BIN", &opencode.display().to_string()),
                ("CORPUS_OPENCODE_AGENT", &agent_file_stem(project, team, agent)),
                ("CORPUS_OPENCODE_MODEL", model),
                ("CORPUS_OPENCODE_PROMPT", mission),
                (PROJECT_ENV, project),
                (TEAM_ENV, team),
                (STORE_ENV, &store.root().to_string_lossy().into_owned()),
            ],
        )?;
        let mut command = Command::new(&tmux);
        command.args(["new-session", "-d", "-s", &session]);
        command.arg("-c").arg(&repo);
        command.arg(&script);
        let status = command.status().map_err(|e| Error::Store(format!("failed to spawn tmux: {e}")))?;
        if !status.success() {
            let _ = fs::remove_file(&script);
            return Err(Error::Store(format!("tmux refused the session: {status}")));
        }
        // Raw pane capture FIRST (so early output is never missed).
        let _ = Command::new(&tmux)
            .args(["pipe-pane", "-t", &session])
            .arg(format!("cat >> {}", raw.display()))
            .status();
        Ok(Self {
            transcript: export_json.clone(),
            backend: Backend::Tui {
                session,
                tui_session_id: None,
                launched_at_ms: now_millis(),
                aborted: false,
                exported: false,
                export_json,
                raw,
                script,
                file_pos: 0,
                pending: String::new(),
            },
        })
    }

    /// The piped headless backend: `opencode run` with streams pumped
    /// into the transcript and a line channel.
    fn start_piped(
        store: &Store,
        project: &str,
        team: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        append_to: Option<&Path>,
    ) -> Result<Self> {
        let opencode = resolve_opencode()?;
        let runs = crate::store::Scope::new(project, team)
            .corpus_dir(store)
            .join("runs");
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
        let mut command = opencode_command(&opencode, project, team, agent, model, mission);
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

    /// Non-blocking exit check. A TUI run never exits on its own — this
    /// reports only an abort; the piped run surfaces its real status.
    pub fn try_exit(&mut self) -> Option<ExitStatus> {
        match &mut self.backend {
            Backend::Piped { child, .. } => child.try_wait().ok().flatten(),
            Backend::Tui { aborted, .. } => {
                if *aborted {
                    Some(abort_exit_status())
                } else {
                    None
                }
            }
        }
    }

    /// The command to attach a terminal to this run: `(program, args)`
    /// an external terminal shells out to. None when the run has no
    /// attach backend (the piped fallback). Retained for CLI use; the
    /// deck embeds the terminal (chunk 7) and uses
    /// [`RunSession::pty_attach_command`] instead.
    pub fn attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => attach_argv(session),
        }
    }

    /// The argv to attach an EMBEDDED PTY to this run (deck chunk 7):
    /// plain `tmux attach -t <session>` — no terminal-app shell-out.
    /// None for the piped fallback (the pane then shows the tail).
    pub fn pty_attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => tui_attach_command(session),
        }
    }

    /// Dismiss: export the transcript of record, then close the run.
    pub fn dismiss(&mut self) -> Result<PathBuf> {
        match &mut self.backend {
            Backend::Piped { child, .. } => {
                kill_tree(child);
                Ok(self.transcript.clone())
            }
            Backend::Tui { .. } => {
                let path = self.export_transcript()?;
                self.close_tui();
                Ok(path)
            }
        }
    }

    /// Abort: best-effort export (if the session is findable), then kill
    /// the whole session tree. Never fails the caller — the operator's
    /// abort is final regardless.
    pub fn abort(&mut self) {
        match &mut self.backend {
            Backend::Piped { child, .. } => kill_tree(child),
            Backend::Tui { .. } => {
                let _ = self.export_transcript();
                self.close_tui();
                if let Backend::Tui { aborted, .. } = &mut self.backend {
                    *aborted = true;
                }
            }
        }
    }

    /// `opencode export <session-id>`: the newest session opened in the
    /// project dir since launch, written as clean JSON to
    /// `<epoch>-<agent>.json`. Returns the written path.
    fn export_transcript(&mut self) -> Result<PathBuf> {
        let Backend::Tui {
            export_json,
            exported,
            tui_session_id,
            launched_at_ms,
            ..
        } = &mut self.backend
        else {
            return Ok(self.transcript.clone());
        };
        if *exported {
            return Ok(export_json.clone());
        }
        let store = Store::from_env();
        let repo = repo_root(&store);
        let id = match tui_session_id.clone() {
            Some(id) => id,
            None => {
                let found = find_opencode_session(&repo, *launched_at_ms)?;
                *tui_session_id = Some(found.clone());
                found
            }
        };
        let json = export_opencode_json(&repo, &id)?;
        if let Some(parent) = export_json.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&*export_json, json)?;
        *exported = true;
        Ok(export_json.clone())
    }

    /// Kill the tmux session (the whole TUI process tree) and drop the
    /// temp script/raw files.
    fn close_tui(&mut self) {
        let Backend::Tui {
            session, script, raw, ..
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
        let _ = fs::remove_file(raw);
    }

    fn runs_for(store: &Store, project: &str, team: &str, agent: &str, ts: u64, ext: &str) -> PathBuf {
        crate::store::Scope::new(project, team)
            .corpus_dir(store)
            .join("runs")
            .join(format!("{ts}-{agent}.{ext}"))
    }
}

/// The repo root our runs attach to: the parent of the store root.
fn repo_root(store: &Store) -> PathBuf {
    store
        .root()
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_default()
        .canonicalize()
        .unwrap_or_else(|_| store.root().to_path_buf())
}

/// Resolve the effective launch model: agent instance -> agent template
/// -> explicit launch arg. A launch with none must not inherit
/// opencode's ambient default — fail loudly with a pointer.
fn resolve_launch_model(
    store: &Store,
    project: &str,
    team: &str,
    agent: &str,
    launch_model: Option<&str>,
) -> Result<String> {
    let spec = TeamSpec::load(store, project, team)?;
    let instance = spec
        .agents
        .get(agent)
        .ok_or_else(|| Error::Store(format!("team {project}/{team} has no agent named {agent:?}")))?;
    let template_model = instance
        .template
        .as_str()
        .trim_end_matches(".md");
    let template = store.load_agent(project, template_model).ok();
    let model = pick_model(
        instance.model.as_deref(),
        template.and_then(|t| t.model).as_deref(),
        launch_model,
    );
    model.ok_or_else(|| {
        Error::Store(format!(
            "no model configured for agent {agent} on {project}/{team} — set one on \
             the agent instance, the agent template, or pass an explicit model; \
             opencode's ambient default is never inherited"
        ))
    })
}

/// First non-empty of: instance override, template default, launch arg.
fn pick_model(instance: Option<&str>, template: Option<&str>, arg: Option<&str>) -> Option<String> {
    [instance, template, arg]
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
fn write_tui_script(script: &Path, params: &[(&str, &str)]) -> Result<()> {
    let mut out = String::from("#!/bin/sh\n");
    for (key, value) in params {
        out.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    out.push_str(
        "exec \"$CORPUS_OPENCODE_BIN\" --agent \"$CORPUS_OPENCODE_AGENT\" --model \"$CORPUS_OPENCODE_MODEL\" --prompt \"$CORPUS_OPENCODE_PROMPT\"\n",
    );
    fs::write(script, out)?;
    let mut perms = fs::metadata(script)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(script, perms)?;
    Ok(())
}

/// Single-quote a dynamic value so it is inert inside the run script.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Find the newest opencode session opened in `cwd` since `launched_at`
/// (ms) — our TUI's session id. The TUI command has no `--title`, so
/// the record is located by project dir + launch window instead.
fn find_opencode_session(cwd: &Path, launched_at_ms: u64) -> Result<String> {
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
    let window = launched_at_ms.saturating_sub(5_000);
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
        if created < window || created > launched_at_ms + 60_000 {
            continue;
        }
        if best.as_ref().map(|(c, _)| created > *c).unwrap_or(true) {
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

/// The plain `tmux attach -t <session>` argv for an embedded PTY (deck
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
/// the deck's re-attach list after a relaunch: a run outlives the deck
/// by design, so a reopened deck offers these for in-pane attach.
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

/// Signal-shaped exit for an aborted TUI run (the transcript documents
/// the rest).
fn abort_exit_status() -> ExitStatus {
    ExitStatus::from_raw(130 << 8)
}

/// Tail a file from the last consumed offset, emitting one complete
/// line per call (the deck's live tail over pipe-pane raw capture).
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
        // A TUI redraws in place with CR separators and can go long
        // stretches without an LF — split on CR or LF, and flush a frame
        // that outgrew a sane line as one coarse line either way.
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
fn resolve_opencode() -> Result<PathBuf> {
    if let Some(found) = on_path("opencode") {
        return Ok(found);
    }
    if let Ok(home) = std::env::var("HOME") {
        let candidate = PathBuf::from(format!("{home}/.opencode/bin/opencode"));
        if is_executable(&candidate) {
            return Ok(candidate);
        }
    }
    if let Some(repo_root) = Store::from_env().root().parent() {
        let candidate = repo_root
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
    team: &str,
    agent: &str,
    model: Option<&str>,
    mission: &str,
) -> Command {
    let mut command = Command::new(opencode);
    command.args(["run", "--agent", &agent_file_stem(project, team, agent)]);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(model) = model {
        command.args(["-m", model]);
    }
    command.arg(mission);
    let store = Store::from_env();
    if let Some(repo_root) = store.root().parent() {
        command.current_dir(repo_root);
    }
    command
        .env(PROJECT_ENV, project)
        .env(TEAM_ENV, team)
        .env(STORE_ENV, store.root());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// The materialized agent file stem: `<team>-<agent>.md` for per-team
/// agents; the bare agent name on the default project/team. Agent names
/// are slugified (team slugs already are).
pub fn agent_file_stem(project: &str, team: &str, agent: &str) -> String {
    let stem = slugify(agent);
    if project == crate::store::DEFAULT_PROJECT_SLUG && team == crate::store::DEFAULT_TEAM_SLUG {
        stem
    } else {
        format!("{team}-{stem}")
    }
}

/// kebab-case a free-form agent name.
fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

impl Store {
    /// The directory materialized agents land in: `.opencode/agent/`
    /// next to the store root (the repo root).
    pub fn opencode_agent_dir(&self) -> PathBuf {
        self.root()
            .parent()
            .map(|p| p.join(".opencode").join("agent"))
            .unwrap_or_else(|| self.root().to_path_buf())
    }

    /// Render every agent in a team spec to `.opencode/agent/`.
    pub fn materialize_team_agents(&self, project: &str, team: &str) -> Result<Vec<PathBuf>> {
        let spec = TeamSpec::load(self, project, team)?;
        let local = self.project_templates(project);
        let core = self.core_templates();
        let out_dir = self.opencode_agent_dir();
        fs::create_dir_all(&out_dir)?;
        let mut written = Vec::new();
        for (name, instance) in &spec.agents {
            let template = self.load_agent(project, &instance.template).map_err(|e| {
                Error::Store(format!("team {project}/{team} agent {name}: {e}"))
            })?;
            let dest = out_dir.join(format!("{}.md", agent_file_stem(project, team, name)));
            template.render(&local, &core, instance.model.as_deref(), &dest)?;
            written.push(dest);
        }
        Ok(written)
    }
}

/// A fresh transcript path + the epoch for headless runs, byte-identical
/// to what `corpus run` produced.
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
    use crate::store::AgentInstance;
    use std::collections::BTreeMap;
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

    fn tmp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!("corpus-launch-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        (Store::new(dir.clone()), dir)
    }

    #[test]
    fn agent_names_materialize_per_team_naming_scheme() {
        assert_eq!(agent_file_stem("default", "default", "operator"), "operator");
        assert_eq!(agent_file_stem("default", "default", "Flow Agent"), "flow-agent");
        assert_eq!(agent_file_stem("p", "red", "operator"), "red-operator");
        assert_eq!(agent_file_stem("p", "red-team", "My Auditor"), "red-team-my-auditor");
    }

    #[test]
    fn mission_slugs_are_identical_to_corpus_run() {
        let (path, _) = fresh_transcript_path(
            std::path::Path::new("/tmp"),
            "operator",
            "Probe the environment & map surfaces!",
        );
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.ends_with("-probe-the-environment-map-surfaces.log"), "{name}");
        assert!(!name.starts_with("operator"));
        assert!(name.contains("-operator-"), "{name}");
    }

    #[test]
    fn launch_model_precedence_and_loud_failure() {
        // instance wins, then template, then the explicit arg.
        assert_eq!(
            pick_model(Some("inst"), Some("tpl"), Some("arg")).as_deref(),
            Some("inst")
        );
        assert_eq!(
            pick_model(None, Some("tpl"), Some("arg")).as_deref(),
            Some("tpl")
        );
        assert_eq!(
            pick_model(None, None, Some("arg")).as_deref(),
            Some("arg")
        );
        // empty values are skipped, whitespace trimmed
        assert_eq!(
            pick_model(None, Some("  "), Some(" arg ")).as_deref(),
            Some("arg")
        );
        // none at all -> loud failure (never the ambient default)
        assert_eq!(pick_model(None, None, None), None);
    }

    #[test]
    fn abort_kills_the_whole_headless_run_tree() {
        let _guard = env_lock();
        let _ = Command::new("pkill").args(["-f", "sleep 90127"]).status(); // clear debris
        let bin = std::env::temp_dir().join(format!("corpus-fake-bin-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        fs::write(&fake, "#!/bin/sh\nsleep 90127\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = std::env::var("PATH").unwrap_or_default();
        path = format!("{}:{}", bin.display(), path);
        std::env::set_var("PATH", &path);

        let (_, store_dir) = tmp_store("abort");
        std::env::set_var("CORPUS_STORE", &store_dir);
        let (store, _) = (
            Store::from_env(),
            store_dir.clone(),
        );
        store.create_project("default", "Default", "cdk-regtest").unwrap();

        let mut session = RunSession::spawn_headless("default", "default", "operator", None, "probe")
            .expect("piped headless spawn");
        assert!(session.transcript.is_file(), "transcript starts at spawn");
        std::thread::sleep(Duration::from_millis(800));

        let started = std::time::Instant::now();
        session.abort();
        let mut exited = false;
        while started.elapsed() < Duration::from_secs(5) {
            if session.try_exit().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "abort reaps the run within 5s");
        let alive = Command::new("pgrep")
            .args(["-f", "sleep 90127"])
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);
        assert!(!alive, "no orphaned grandchildren");

        std::env::remove_var("CORPUS_STORE");
        std::env::remove_var("PATH");
        let _ = fs::remove_dir_all(&bin);
        let _ = fs::remove_dir_all(&store_dir);
    }

    #[test]
    #[ignore = "races the parallel harness: fresh tmux server + pipe-pane under 19 threads; green solo (--test-threads=1). The real path is covered by e2e_tui_launch_export_and_abort."]
    fn tui_run_detaches_attaches_and_aborts() {
        let _guard = env_lock();
        let _ = Command::new("pkill").args(["-f", "sleep 90128"]).status(); // clear debris
        // A stale or client-wedged tmux server from an earlier run makes
        // new sessions unreachable — always start from a fresh server.
        let _ = Command::new("tmux").arg("kill-server").status();
        if tmux_available().is_none() {
            return; // only meaningful with tmux >= 3.2a
        }
        let bin = std::env::temp_dir().join(format!("corpus-fake-bin-tui-{}", std::process::id()));
        let _ = fs::remove_dir_all(&bin);
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("opencode");
        fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = \"session\" ]; then printf '[]\\n'; exit 0; fi\nsleep 1\necho READY\nsleep 90128\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut path = std::env::var("PATH").unwrap_or_default();
        path = format!("{}:{}", bin.display(), path);
        std::env::set_var("PATH", &path);

        let (store, store_dir) = tmp_store("tui");
        std::env::set_var("CORPUS_STORE", &store_dir);
        store.create_project("default", "Default", "cdk-regtest").unwrap();
        store
            .create_team(
                "default",
                "default",
                "Default",
                core_instances(),
                None,
                None,
            )
            .unwrap();

        let mut session = RunSession::spawn(
            "default",
            "default",
            "operator",
            Some("openrouter/x"),
            "probe",
        )
        .expect("TUI spawn");
        assert!(session.attach_command().is_some(), "TUI runs are attachable");

        // pipe-pane capture feeds the live tail (the first raw line
        // may be the pane shell's prompt — the fake's READY follows).
        // The pane runs the fake: READY appears once the session
        // command executes. Under heavy parallel load the tmux server
        // can be slow to serve, so give it a generous window and treat
        // ANY pane output (a rendered prompt counts) as proof the
        // pipe-pane capture reaches the tail.
        let mut saw_output = false;
        for _ in 0..1200 {
            if let Some(line) = session.poll_line() {
                if !line.text.trim().is_empty() {
                    saw_output = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !saw_output {
            let listed = Command::new("tmux")
                .args(["ls"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let raws = fs::read_dir(std::env::temp_dir())
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            e.file_name().to_string_lossy().contains("corpus-default-operator-")
                                && e.path().extension().map(|x| x == "raw").unwrap_or(false)
                        })
                        .map(|e| {
                            format!(
                                "{}:{}b",
                                e.file_name().to_string_lossy(),
                                fs::metadata(e.path()).map(|m| m.len()).unwrap_or(0)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("  ")
                })
                .unwrap_or_default();
            eprintln!("DIAG tmux: {listed}");
            eprintln!("DIAG raws: {raws}");
            let procs = Command::new("ps")
                .args(["-eo", "pid,ppid,etime,command"])
                .output()
                .map(|o| {
                    String::from_utf8_lossy(&o.stdout)
                        .lines()
                        .filter(|l| l.contains("corpus-fake-bin-tui") || l.contains("opencode --agent"))
                        .take(6)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            eprintln!("DIAG procs:\n{procs}");
        }
        assert!(saw_output, "pipe-pane raw capture reaches the deck tail");

        let listed = Command::new("tmux")
            .args(["ls"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(listed.contains("corpus-default-operator-"), "detached session: {listed}");

        session.abort();
        let started = std::time::Instant::now();
        let mut exited = false;
        while started.elapsed() < Duration::from_secs(5) {
            if session.try_exit().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "abort reaps the TUI run within 5s");
        let listed = Command::new("tmux")
            .args(["ls"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(!listed.contains("corpus-default-operator-"), "cleanup: {listed}");

        std::env::remove_var("CORPUS_STORE");
        std::env::remove_var("PATH");
        let _ = fs::remove_dir_all(&bin);
        let _ = fs::remove_dir_all(&store_dir);
    }

    fn core_instances() -> BTreeMap<String, AgentInstance> {
        let mut agents = BTreeMap::new();
        agents.insert(
            "operator".to_string(),
            AgentInstance {
                template: "operator".to_string(),
                model: None,
            },
        );
        agents
    }

    #[test]
    fn materialize_renders_team_agents_with_model_override() {
        let (store, dir) = tmp_store("mat");
        let core = store.core_templates();
        fs::create_dir_all(core.permissions.clone()).unwrap();
        fs::create_dir_all(core.prompts.clone()).unwrap();
        fs::write(
            core.permissions.join("role.md"),
            "---\nname: role\ndescription: d\npermission: |\n  bash: deny\n  edit: deny\n---\n",
        )
        .unwrap();
        fs::write(
            core.prompts.join("prompt.md"),
            "---\nname: prompt\ndescription: d\n---\n\nYou are a probe.\n",
        )
        .unwrap();
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .write_agent(
                "p",
                "probe",
                &crate::templates::AgentTemplate {
                    name: "probe".to_string(),
                    description: String::new(),
                    mode: "primary".to_string(),
                    permission_ref: "role".to_string(),
                    prompt_ref: "prompt".to_string(),
                    model: Some("openrouter/x".to_string()),
                },
            )
            .unwrap();
        let mut agents = BTreeMap::new();
        agents.insert(
            "Auditor".to_string(),
            AgentInstance {
                template: "probe".to_string(),
                model: Some("openrouter/y".to_string()),
            },
        );
        store.create_team("p", "red", "Red", agents, None, None).unwrap();

        let written = store.materialize_team_agents("p", "red").unwrap();
        assert_eq!(written.len(), 1);
        let dest = written[0].clone();
        assert!(dest.ends_with("red-auditor.md"), "{dest:?}");
        let text = fs::read_to_string(&dest).unwrap();
        assert!(text.contains("model: openrouter/y"), "{text}");
        assert!(text.contains("You are a probe."), "{text}");
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod e2e {
    //! MANUAL end-to-end drill, not part of the normal gate: launches the
    //! REAL opencode TUI (costs a local model turn), dismisses to export a
    //! transcript JSON, and aborts a second run.
    //!
    //! Run: cargo test -p corpus-core e2e_tui_launch_export_and_abort -- --ignored --nocapture

    use super::*;

    #[test]
    #[ignore = "manual drill: spawns a real opencode TUI session and runs a model turn"]
    fn e2e_tui_launch_export_and_abort() {
        let _guard = tests::env_lock();
        if tmux_available().is_none() {
            eprintln!("SKIP: no tmux >= 3.2a");
            return;
        }
        let store = Store::from_env(); // the real store: default/default
        store.materialize_team_agents("default", "default").unwrap();
        let model = std::env::var("CORPUS_E2E_MODEL")
            .unwrap_or_else(|_| "ollama/qwen3.6:35b".to_string());

        // Run 1: launch, watch the pane, dismiss -> exported JSON.
        let mut session =
            RunSession::spawn("default", "default", "operator", Some(&model), "Reply with exactly: PONG")
                .expect("TUI spawn");
        assert!(session.attach_command().is_some(), "attach offered");

        let mut grew = false;
        for _ in 0..1200 {
            if let Some(line) = session.poll_line() {
                if !line.text.trim().is_empty() {
                    grew = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(grew, "the TUI rendered into the raw capture");

        let listed = Command::new("tmux")
            .args(["ls"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(listed.contains("corpus-default-operator-"), "detached session: {listed}");

        let exported = session.dismiss().expect("dismiss exports the transcript");
        eprintln!("EXPORTED -> {}", exported.display());
        let text = fs::read_to_string(&exported).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).expect("export is JSON");
        let keys: Vec<String> = value
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default();
        let messages = value
            .get("messages")
            .and_then(|m| m.as_array())
            .map(|m| m.len())
            .unwrap_or(0);
        eprintln!("export keys: {keys:?}  messages: {messages}");
        assert!(messages > 0, "the exported transcript holds the session messages");

        let listed = Command::new("tmux")
            .args(["ls"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(!listed.contains("corpus-default-operator-"), "dismiss closed the session: {listed}");

        // Run 2: abort tears the whole session down.
        let mut second =
            RunSession::spawn("default", "default", "operator", Some(&model), "Reply with exactly: PONG")
                .expect("second TUI spawn");
        std::thread::sleep(Duration::from_millis(1200));
        second.abort();
        let started = std::time::Instant::now();
        let mut exited = false;
        while started.elapsed() < Duration::from_secs(5) {
            if second.try_exit().is_some() {
                exited = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(exited, "abort reaped the second run");
        let listed = Command::new("tmux")
            .args(["ls"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        assert!(!listed.contains("corpus-default-operator-"), "abort cleaned up: {listed}");
    }
}
