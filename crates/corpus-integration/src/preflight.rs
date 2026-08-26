//! Live-run preflight performed while holding the model lease.

use std::io;

use serde::Serialize;

use crate::ollama::{self, OllamaModel};

#[derive(Debug, Serialize)]
pub struct LivePreflight {
    pub model: OllamaModel,
    pub rustc: String,
    pub git_commit: String,
    pub dirty: bool,
}

pub fn live_qwen38() -> io::Result<LivePreflight> {
    let model = ollama::require_qwen38()?;
    let rustc = command_text("rustc", &["--version"])?;
    let git_commit = command_text("git", &["rev-parse", "HEAD"])?;
    let dirty = !command_text("git", &["status", "--porcelain"])?.is_empty();
    Ok(LivePreflight {
        model,
        rustc,
        git_commit,
        dirty,
    })
}

fn command_text(program: &str, args: &[&str]) -> io::Result<String> {
    let output = std::process::Command::new(program).args(args).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
