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
pub(super) enum Backend {
    Tui {
        session: String,
        workspace_id: String,
        control_port: Option<u16>,
        tui_session_id: Option<String>,
        launched_at_ms: u64,
        stopped: bool,
        exported: bool,
        export_json: PathBuf,
        raw: PathBuf,
        script: PathBuf,
        file_pos: u64,
        pending: String,
        liveness: (Instant, bool),
        discovery: Instant,
        repo: PathBuf,
    },
    Piped {
        child: Child,
        rx: mpsc::Receiver<RunLine>,
    },
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
            Backend::Tui { session, .. } => Some(session.clone()),
            Backend::Piped { .. } => self
                .transcript
                .file_name()
                .map(|name| name.to_string_lossy().into_owned()),
        }
    }

    pub fn control_port(&self) -> Option<u16> {
        match &self.backend {
            Backend::Tui { control_port, .. } => *control_port,
            Backend::Piped { .. } => None,
        }
    }

    /// Relocatable identity of the exact source-view workspace that owns the
    /// OpenCode conversation. Piped runs have no resumable TUI conversation.
    pub fn workspace_id(&self) -> Option<String> {
        match &self.backend {
            Backend::Tui { workspace_id, .. } => Some(workspace_id.clone()),
            Backend::Piped { .. } => None,
        }
    }

    /// Discover and cache the OpenCode conversation backing a TUI run.
    pub fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String> {
        let Backend::Tui {
            tui_session_id,
            launched_at_ms,
            repo,
            discovery,
            ..
        } = &mut self.backend
        else {
            return None;
        };
        if let Some(id) = tui_session_id {
            return Some(id.clone());
        }
        if discovery.elapsed() < Duration::from_secs(1) {
            return None;
        }
        *discovery = Instant::now();
        let found = find_opencode_session(repo, *launched_at_ms, claimed)
            .ok()
            .flatten()?;
        *tui_session_id = Some(found.clone());
        Some(found)
    }

    /// Return one buffered output line without blocking.
    pub fn poll_line(&mut self) -> Option<RunLine> {
        match &mut self.backend {
            Backend::Piped { rx, .. } => rx.try_recv().ok(),
            Backend::Tui {
                raw,
                file_pos,
                pending,
                ..
            } => tail_line(raw, file_pos, pending),
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
            Backend::Piped { child, .. } => child.try_wait().ok().flatten(),
            Backend::Tui {
                stopped,
                session,
                liveness,
                ..
            } => {
                if *stopped {
                    Some(stopped_exit_status())
                } else if !session_live(session, liveness) {
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
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => external_attach_argv(session),
        }
    }

    /// Plain tmux attachment for the application's embedded PTY.
    pub fn pty_attach_command(&self) -> Option<Vec<String>> {
        match &self.backend {
            Backend::Piped { .. } => None,
            Backend::Tui { session, .. } => tui_attach_command(session),
        }
    }

    /// Stop the owned backend, then export or retain its durable fallback.
    pub fn stop_detailed(&mut self) -> StopOutcome {
        match &mut self.backend {
            Backend::Piped { child, .. } => StopOutcome {
                transcript: self.transcript.clone(),
                export_error: None,
                cleanup_errors: kill_tree_checked(child),
            },
            Backend::Tui { .. } => {
                let fallback = match &self.backend {
                    Backend::Tui { raw, .. } => raw.clone(),
                    Backend::Piped { .. } => self.transcript.clone(),
                };
                // Stop the writer before export so a successful command cannot
                // publish a truncated document while OpenCode is still writing.
                let cleanup_errors = self.close_tui_checked();
                let (transcript, export_error) = match self.export_transcript() {
                    Ok(path) => (path, None),
                    Err(error) => (fallback, Some(format!("transcript export failed: {error}"))),
                };
                if let Backend::Tui { stopped, .. } = &mut self.backend {
                    *stopped = true;
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
        let Backend::Tui {
            export_json,
            exported,
            tui_session_id,
            launched_at_ms,
            repo,
            raw,
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
                let Some(found) = find_opencode_session(repo, *launched_at_ms, &BTreeSet::new())?
                else {
                    return Ok(raw.clone());
                };
                *tui_session_id = Some(found.clone());
                found
            }
        };
        let runs = export_json
            .parent()
            .ok_or_else(|| Error::Store("export path has no runs dir".into()))?;
        let output = export_record(repo, runs, &id)?;
        *export_json = output.clone();
        *exported = true;
        Ok(output)
    }

    fn close_tui_checked(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let Backend::Tui {
            session, script, ..
        } = &self.backend
        else {
            return errors;
        };
        if let Err(error) = kill_tmux_session_checked(session) {
            errors.push(error.to_string());
        }
        if let Err(error) = fs::remove_file(script) {
            if error.kind() != std::io::ErrorKind::NotFound {
                errors.push(format!("remove run script {}: {error}", script.display()));
            }
        }
        errors
    }
}
