//! Runtime session state and backend-shaped observation.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::process::{Child, ExitStatus};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::process::{kill_tree_checked, stopped_exit_status, successful_exit_status};
use super::tmux::{
    external_attach_argv, kill_tmux_session_checked, session_live, tui_attach_command,
};
use super::transcript::{export_record, find_opencode_session, tail_line};
use crate::error::{Error, Result};

/// One transcript line. Piped streams retain stderr identity; TUI lines come
/// from the raw pane capture and therefore always use `stderr: false`.
#[derive(Debug, Clone)]
pub struct RunLine {
    pub stderr: bool,
    pub text: String,
}

/// Stop attempts every cleanup step and preserves the durable transcript path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopOutcome {
    pub transcript: PathBuf,
    pub export_error: Option<String>,
    pub cleanup_errors: Vec<String>,
}

/// The concrete backend state is visible only to the parent launch facade.
pub(super) struct TuiBackend {
    pub(super) session: String,
    pub(super) workspace_id: String,
    pub(super) control_port: Option<u16>,
    pub(super) tui_session_id: Option<String>,
    pub(super) launched_at_ms: u64,
    pub(super) stopped: bool,
    pub(super) exported: bool,
    pub(super) export_json: PathBuf,
    pub(super) raw: PathBuf,
    pub(super) script: PathBuf,
    pub(super) file_pos: u64,
    pub(super) pending: String,
    pub(super) liveness: (Instant, bool),
    pub(super) discovery: Instant,
    pub(super) repo: PathBuf,
}

pub(super) struct PipedBackend {
    pub(super) child: Child,
    pub(super) rx: mpsc::Receiver<RunLine>,
}

pub(super) enum Backend {
    Tui(Box<TuiBackend>),
    Piped(PipedBackend),
}

/// A running OpenCode mission on one project scope.
pub struct RunSession {
    /// The exported JSON record for a TUI run or durable log for a piped run.
    pub transcript: PathBuf,
    pub(super) backend: Backend,
}

impl RunSession {
    pub fn launch_identity(&self) -> Option<String> {
        match &self.backend {
            Backend::Tui(tui) => Some(tui.session.clone()),
            Backend::Piped(_) => self
                .transcript
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        }
    }

    pub fn control_port(&self) -> Option<u16> {
        match &self.backend {
            Backend::Tui(tui) => tui.control_port,
            Backend::Piped(_) => None,
        }
    }

    /// Relocatable identity of the exact source-view workspace that owns the
    /// OpenCode conversation. Piped runs have no resumable TUI conversation.
    pub fn workspace_id(&self) -> Option<String> {
        match &self.backend {
            Backend::Tui(tui) => Some(tui.workspace_id.clone()),
            Backend::Piped(_) => None,
        }
    }

    /// Discover and cache the OpenCode conversation backing a TUI run.
    pub fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String> {
        let Backend::Tui(tui) = &mut self.backend else {
            return None;
        };
        if let Some(id) = &tui.tui_session_id {
            return Some(id.clone());
        }
        if tui.discovery.elapsed() < Duration::from_secs(1) {
            return None;
        }
        tui.discovery = Instant::now();
        let found = find_opencode_session(&tui.repo, tui.launched_at_ms, claimed)
            .ok()
            .flatten()?;
        tui.tui_session_id = Some(found.clone());
        Some(found)
    }

    /// Return one buffered output line without blocking.
    pub fn poll_line(&mut self) -> Option<RunLine> {
        match &mut self.backend {
            Backend::Piped(piped) => piped.rx.try_recv().ok(),
            Backend::Tui(tui) => tail_line(&tui.raw, &mut tui.file_pos, &mut tui.pending),
        }
    }

    pub fn poll_line_timeout(&mut self, timeout: Duration) -> Option<RunLine> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(line) = self.poll_line() {
                return Some(line);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn try_exit(&mut self) -> Option<ExitStatus> {
        match &mut self.backend {
            Backend::Piped(piped) => piped.child.try_wait().ok().flatten(),
            Backend::Tui(tui) => {
                if tui.stopped {
                    Some(stopped_exit_status())
                } else if !session_live(&tui.session, &mut tui.liveness) {
                    Some(successful_exit_status())
                } else {
                    None
                }
            }
        }
    }

    /// External terminal attachment retained for CLI callers.
    pub fn attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped(_) => None,
            Backend::Tui(tui) => external_attach_argv(&tui.session),
        }
    }

    /// Plain tmux attachment for the application's embedded PTY.
    pub fn pty_attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped(_) => None,
            Backend::Tui(tui) => tui_attach_command(&tui.session),
        }
    }

    /// Stop the owned backend, then export or retain its durable fallback.
    pub fn stop_detailed(&mut self) -> StopOutcome {
        match &mut self.backend {
            Backend::Piped(piped) => StopOutcome {
                transcript: self.transcript.clone(),
                export_error: None,
                cleanup_errors: kill_tree_checked(&mut piped.child),
            },
            Backend::Tui(tui) => {
                let fallback = tui.raw.clone();
                // Stop the writer before export so a successful command cannot
                // publish a truncated document while OpenCode is still writing.
                let cleanup_errors = self.close_tui_checked();
                let (transcript, export_error) = match self.export_transcript() {
                    Ok(path) => (path, None),
                    Err(error) => (fallback, Some(format!("transcript export failed: {error}"))),
                };
                if let Backend::Tui(tui) = &mut self.backend {
                    tui.stopped = true;
                }
                StopOutcome {
                    transcript,
                    export_error,
                    cleanup_errors,
                }
            }
        }
    }

    pub fn stop(&mut self) -> PathBuf {
        let outcome = self.stop_detailed();
        if let Some(error) = &outcome.export_error {
            eprintln!("corpus: stop cleanup failed: {error}");
        }
        for error in &outcome.cleanup_errors {
            eprintln!("corpus: stop cleanup failed: {error}");
        }
        outcome.transcript
    }

    fn export_transcript(&mut self) -> Result<PathBuf> {
        let Backend::Tui(tui) = &mut self.backend else {
            return Ok(self.transcript.clone());
        };
        if tui.exported {
            return Ok(tui.export_json.clone());
        }
        let id = match tui.tui_session_id.clone() {
            Some(id) => id,
            None => {
                let Some(found) =
                    find_opencode_session(&tui.repo, tui.launched_at_ms, &BTreeSet::new())?
                else {
                    return Ok(tui.raw.clone());
                };
                tui.tui_session_id = Some(found.clone());
                found
            }
        };
        let runs = tui
            .export_json
            .parent()
            .ok_or_else(|| Error::Store("export path has no runs dir".into()))?;
        let output = export_record(&tui.repo, runs, &id)?;
        tui.export_json = output.clone();
        tui.exported = true;
        Ok(output)
    }

    fn close_tui_checked(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let Backend::Tui(tui) = &self.backend else {
            return errors;
        };
        if let Err(error) = kill_tmux_session_checked(&tui.session) {
            errors.push(error.to_string());
        }
        if let Err(error) = fs::remove_file(&tui.script) {
            if error.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!(
                    "remove run script {}: {error}",
                    tui.script.display()
                ));
            }
        }
        errors
    }
}
