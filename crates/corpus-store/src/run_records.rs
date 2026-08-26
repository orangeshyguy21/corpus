//! Persisted run transcript records and their exact launch identity.

use std::collections::BTreeMap;
use std::fs;

use crate::error::Result;
use crate::store::Store;

/// The mission slug for this exact run. Paired with [`RUN_ID_ENV`] so a
/// project-management call can record which Curator mission dispatched work.
pub const MISSION_ENV: &str = "CORPUS_MISSION";

/// Exact launcher session identity. TUI runs use their persisted tmux session
/// name; the no-tmux fallback uses its unique transcript basename.
pub const RUN_ID_ENV: &str = "CORPUS_RUN_ID";

/// The basename of the current run's transcript file in the project corpus
/// `runs/` (for example, `1786891368-verify.raw`).
pub const RUN_LOG_ENV: &str = "CORPUS_RUN_LOG";

/// The project corpus category containing immutable run transcripts.
pub const RUNS: &str = "runs";

/// One transcript record in the project corpus `runs/` directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionLog {
    /// File name as it sits in `runs/` (e.g. `1786891368-verify.raw`) —
    /// the value [`RUN_LOG_ENV`] carries and findings cite.
    pub name: String,
    /// The agent slug parsed out of `<epoch>-<agent>.<ext>` or resolved from
    /// a mission whose OpenCode session owns a session-keyed JSON export.
    /// `None` for legacy/unlinked filenames that carry no agent identity.
    pub agent: Option<String>,
    /// Run-start epoch seconds from the name prefix (0 when absent).
    pub started: u64,
    pub bytes: u64,
    /// Extension: `raw` (piped transcript), `json` (OpenCode export).
    pub kind: String,
}

/// List a project's transcript records, newest first. Mission records are read
/// once to map session-keyed exports back to their agent; transcript contents
/// are never parsed. Only regular files directly under `runs/` count.
pub fn mission_logs(store: &Store, project: &str) -> Result<Vec<MissionLog>> {
    let runs = store.project_corpus_dir(project).join(RUNS);
    let mut logs = Vec::new();
    if !runs.is_dir() {
        return Ok(logs);
    }
    let session_agents: BTreeMap<String, String> = store
        .list_missions(project)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, mission)| {
            mission
                .opencode_session
                .map(|session| (session, mission.agent))
        })
        .collect();
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let bytes = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
        let kind = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_string();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or(name);
        // `<epoch>-<mission>`: split once, and only when the prefix really is
        // a number (a mission named `2fa-probe` must not lose its head).
        let (started, agent) = match stem.split_once('-') {
            Some((head, rest)) if !rest.is_empty() => match head.parse::<u64>() {
                Ok(epoch) => (epoch, Some(rest.to_string())),
                Err(_) => (0, session_agents.get(stem).cloned()),
            },
            _ => (0, session_agents.get(stem).cloned()),
        };
        logs.push(MissionLog {
            name: name.to_string(),
            agent,
            started,
            bytes,
            kind,
        });
    }
    logs.sort_by(|left, right| {
        right
            .started
            .cmp(&left.started)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(logs)
}

#[cfg(test)]
#[path = "run_records/tests.rs"]
mod tests;
