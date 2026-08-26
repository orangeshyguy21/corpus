//! Tmux process adapter. Raw tmux argv and best-effort session setup live here
//! so launch orchestration deals in session intent rather than subprocesses.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::command::{shell_quote, LaunchEnvironment};
use super::executables::resolve_tmux;
use crate::error::{Error, Result};

pub(super) struct SessionSetup<'a> {
    pub(super) name: &'a str,
    pub(super) cwd: &'a Path,
    pub(super) script: &'a Path,
    pub(super) raw_capture: &'a Path,
    pub(super) environment: &'a LaunchEnvironment,
}

#[derive(Debug, Clone)]
struct Tmux {
    executable: PathBuf,
}

impl Tmux {
    fn resolve() -> Option<Self> {
        resolve_tmux().map(|executable| Self { executable })
    }

    fn command(&self) -> Command {
        Command::new(&self.executable)
    }
}

/// Start the detached pane, then project convenience state into the tmux
/// session. Environment, mouse, and capture setup remain best-effort exactly as
/// before: the owner script carries the authoritative child environment and
/// the detached session itself is already live.
pub(super) fn start_session(setup: SessionSetup<'_>) -> Result<()> {
    let tmux = Tmux::resolve().ok_or_else(|| Error::Store("tmux vanished".into()))?;
    let status = new_session_command(&tmux, setup.name, setup.cwd, setup.script)
        .status()
        .map_err(|error| Error::Store(format!("failed to spawn tmux: {error}")))?;
    if !status.success() {
        return Err(Error::Store(format!("tmux refused the session: {status}")));
    }

    for (key, value) in setup.environment.iter() {
        let _ = tmux
            .command()
            .args(["set-environment", "-t", setup.name, key, value])
            .status();
    }
    let _ = tmux
        .command()
        .args(["set-option", "-t", setup.name, "mouse", "on"])
        .status();
    let _ = tmux
        .command()
        .args(["pipe-pane", "-t", setup.name])
        .arg(capture_command(setup.raw_capture))
        .status();
    Ok(())
}

fn new_session_command(tmux: &Tmux, name: &str, cwd: &Path, script: &Path) -> Command {
    let mut command = tmux.command();
    command.args(["new-session", "-d", "-s", name]);
    command.arg("-c").arg(cwd);
    command.arg(script);
    command
}

fn capture_command(raw_capture: &Path) -> String {
    format!("cat >> {}", shell_quote(&raw_capture.to_string_lossy()))
}

/// The external-terminal attach command retained for CLI callers.
pub(super) fn external_attach_argv(session: &str) -> Option<Vec<String>> {
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
        Some(attach_argv(&tmux, session))
    }
}

/// The plain tmux attach argv used by the embedded PTY.
pub fn tui_attach_command(session: &str) -> Option<Vec<String>> {
    let tmux = resolve_tmux()?.display().to_string();
    Some(attach_argv(&tmux, session))
}

fn attach_argv(tmux: &str, session: &str) -> Vec<String> {
    vec![
        tmux.to_string(),
        "attach".to_string(),
        "-t".to_string(),
        session.to_string(),
    ]
}

/// Best-effort compatibility wrapper for callers that cannot retain teardown
/// errors. The checked form below preserves retryable cleanup state.
pub fn kill_tmux_session(session: &str) {
    if let Err(error) = kill_tmux_session_checked(session) {
        eprintln!("corpus: tmux cleanup failed: {error}");
    }
}

pub fn kill_tmux_session_checked(session: &str) -> Result<()> {
    let tmux = Tmux::resolve()
        .ok_or_else(|| Error::Store("cannot stop tmux session: tmux is unavailable".into()))?;
    let status = tmux
        .command()
        .args(["kill-session", "-t", session])
        .status()?;
    if status.success() {
        return Ok(());
    }
    // A session may exit between listing and Stop. A failed kill is successful
    // cleanup only when the same resolved tmux proves the session is gone.
    if !has_session(&tmux, session, false)? {
        return Ok(());
    }
    Err(Error::Store(format!(
        "tmux kill-session failed for {session} with {status}; session is still alive"
    )))
}

/// Throttled liveness probe. Resolution or execution failure remains "live"
/// so a transient tooling problem can never declare an owned run complete.
pub(super) fn session_live(session: &str, cache: &mut (Instant, bool)) -> bool {
    if cache.0.elapsed() < Duration::from_secs(1) {
        return cache.1;
    }
    let live = Tmux::resolve()
        .and_then(|tmux| has_session(&tmux, session, true).ok())
        .unwrap_or(true);
    *cache = (Instant::now(), live);
    live
}

fn has_session(tmux: &Tmux, session: &str, quiet: bool) -> std::io::Result<bool> {
    let mut command = tmux.command();
    command.args(["has-session", "-t", session]);
    if quiet {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    command.status().map(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_session_command_keeps_identity_cwd_and_script_separate() {
        let tmux = Tmux {
            executable: PathBuf::from("/opt/tmux test/bin/tmux"),
        };
        let command = new_session_command(
            &tmux,
            "corpus-agent-42",
            Path::new("/tmp/project run"),
            Path::new("/tmp/run script.sh"),
        );
        assert_eq!(command.get_program(), "/opt/tmux test/bin/tmux");
        assert_eq!(
            command.get_args().collect::<Vec<_>>(),
            [
                "new-session",
                "-d",
                "-s",
                "corpus-agent-42",
                "-c",
                "/tmp/project run",
                "/tmp/run script.sh"
            ]
        );
    }

    #[test]
    fn pane_capture_shell_quotes_the_complete_path() {
        assert_eq!(
            capture_command(Path::new("/tmp/raw capture's.log")),
            "cat >> '/tmp/raw capture'\\''s.log'"
        );
    }

    #[test]
    fn embedded_attach_keeps_the_session_as_one_argument() {
        assert_eq!(
            attach_argv("/opt/tmux test/bin/tmux", "corpus-agent-42"),
            ["/opt/tmux test/bin/tmux", "attach", "-t", "corpus-agent-42"]
        );
    }
}
