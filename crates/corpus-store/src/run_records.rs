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
    /// The agent slug parsed out of a timestamped run artifact or resolved
    /// from a mission whose OpenCode session owns a session-keyed JSON export.
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
    let mut known_agents = store
        .list_agents(project)
        .unwrap_or_default()
        .into_iter()
        .map(|(slug, _)| slug)
        .collect::<Vec<_>>();
    // An agent slug can prefix another agent slug. Prefer the longest exact
    // storage prefix so `recon-deep-...` does not resolve to `recon`.
    known_agents.sort_by_key(|slug| std::cmp::Reverse(slug.len()));
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
        // Timestamped artifacts historically used `<epoch>-<agent>` and now
        // use `<epoch>-<agent>-<mission>-gN`. Resolve against known project
        // agents instead of presenting the entire storage stem as an agent.
        // Split only when the prefix really is a number (an unlinked file
        // named `2fa-probe` must not lose its head).
        let (started, agent) = match stem.split_once('-') {
            Some((head, rest)) if !rest.is_empty() => match head.parse::<u64>() {
                Ok(epoch) => (epoch, filename_agent(rest, &known_agents)),
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

fn filename_agent(stem: &str, known_agents: &[String]) -> Option<String> {
    if let Some(agent) = known_agents.iter().find(|agent| {
        stem == agent.as_str()
            || stem
                .strip_prefix(agent.as_str())
                .is_some_and(|tail| tail.starts_with('-'))
    }) {
        return Some(agent.clone());
    }

    // Preserve a deleted app-created agent's exact UUID so presentation can
    // replace it with a lifecycle label. Never leak the compound run stem.
    let uuid = stem.get(..36).filter(|candidate| {
        candidate.len() == 36
            && candidate.as_bytes().get(8) == Some(&b'-')
            && candidate.as_bytes().get(13) == Some(&b'-')
            && candidate.as_bytes().get(18) == Some(&b'-')
            && candidate.as_bytes().get(23) == Some(&b'-')
            && candidate
                .bytes()
                .enumerate()
                .all(|(index, byte)| matches!(index, 8 | 13 | 18 | 23) || byte.is_ascii_hexdigit())
            && stem.as_bytes().get(36).is_none_or(|byte| *byte == b'-')
    });
    if let Some(uuid) = uuid {
        return Some(uuid.to_string());
    }

    // Human handles from the legacy `<epoch>-<agent>` format remain useful
    // after deletion. A generation token identifies the newer compound form,
    // whose agent/mission boundary cannot be recovered once the agent is gone.
    if stem.split('-').any(|part| {
        part.strip_prefix('g')
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    }) {
        None
    } else {
        Some(stem.to_string())
    }
}

#[cfg(test)]
#[path = "run_records/tests.rs"]
mod tests;
