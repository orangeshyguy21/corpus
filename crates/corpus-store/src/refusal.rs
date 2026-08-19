//! The refusal log: an append-only record of tool calls that did not happen.
//!
//! A refused call used to leave no trace. The agent was told why, in prose,
//! and that prose ended up in a raw PTY capture of a TUI — so reconstructing
//! what the gate did meant stripping ANSI from a multi-megabyte terminal
//! dump and reading the model's own narration of its denials.
//!
//! The diagnostic counterpart to [`crate::audit`], with deliberately
//! opposite contracts. `audit` covers mutating management tools and REFUSES
//! an act it cannot record, because the case for trusting a curator rests
//! on the record. This covers every tool and every role, and is best-effort
//! by construction — [`record`] returns `()` and there is no fallible
//! public write path, because failing to write a diagnostic must never
//! change what the caller was told.
//!
//! Placement follows `audit`: `<var>/refusals/<project>.jsonl`, outside the
//! project subtree, so the agent it describes can neither read nor edit it.
//! Calls refused BECAUSE no project could be resolved land in [`UNSCOPED`]
//! rather than being dropped for want of a filename.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::Store;

/// The log for calls refused before a project could be established.
pub const UNSCOPED: &str = "_unscoped";

/// Which gate turned the call away — the same fact as the caller's prose,
/// as a value an operator can filter on. [`Gate::Unknown`] is the one that
/// means the corpus server never had an opinion, and therefore the only one
/// that says "whatever refused this, it was not us".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Gate {
    /// No agent role could be resolved, so no ceiling could be applied.
    Identity,
    /// A role resolved and does not hold this tool.
    Role,
    /// No project scope could be established.
    Scope,
    /// The environment probe reports the harness is not ready.
    Probe,
    /// The tool was reached and its arguments were rejected.
    Args,
    /// The name is not a tool this server has.
    Unknown,
    /// The plugin, the sandbox, or an external command failed.
    Harness,
}

impl Gate {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Role => "role",
            Self::Scope => "scope",
            Self::Probe => "probe",
            Self::Args => "args",
            Self::Unknown => "unknown",
            Self::Harness => "harness",
        }
    }
}

/// One line of the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefusalRecord {
    /// Epoch seconds.
    pub ts: u64,
    /// The basename of this run's transcript in the project corpus `runs/`:
    /// the join key that turns a line here into a position in the capture.
    pub run_log: Option<String>,
    /// Who was asking — the same identity that stamps the sidecars.
    pub actor: String,
    /// The role in force, or `None` when resolving it is what failed.
    pub role: Option<String>,
    /// The tool, as the caller named it.
    pub tool: String,
    pub gate: Gate,
    /// The arguments: project stripped, truncated.
    pub args: String,
    /// What the caller was told, verbatim. Recording anything else would
    /// make the log describe a call that did not occur.
    pub detail: String,
}

impl RefusalRecord {
    /// The three fields every refusal has. The rest describe the run and
    /// are set by the caller that knows them.
    pub fn new(tool: impl Into<String>, gate: Gate, detail: impl Into<String>) -> Self {
        Self {
            ts: crate::store::now_epoch(),
            run_log: None,
            actor: "unknown".to_string(),
            role: None,
            tool: tool.into(),
            gate,
            args: String::new(),
            detail: detail.into(),
        }
    }
}

/// The log file for a project.
///
/// The project name reaches here from the environment, and a refusal is
/// exactly the situation in which it may be malformed — the scope gate
/// refusing a nonsense `CORPUS_PROJECT` is a call we want logged, not one
/// we want writing through a `../`. So the slug is sanitized, not trusted.
pub fn log_path(store: &Store, project: &str) -> PathBuf {
    store
        .var_dir()
        .join("refusals")
        .join(format!("{}.jsonl", safe_slug(project)))
}

/// Reduce an arbitrary string to one path component. Anything outside
/// `[A-Za-z0-9._-]` becomes `_`; results that would still name a directory
/// rather than a file fall back to [`UNSCOPED`].
fn safe_slug(project: &str) -> String {
    let mapped: String = project
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    match mapped.as_str() {
        "" | "." | ".." => UNSCOPED.to_string(),
        _ => mapped,
    }
}

/// Record a refusal. Best-effort and infallible BY SIGNATURE.
///
/// `project` is `None` when the refusal is the reason there is no project.
pub fn record(store: &Store, project: Option<&str>, entry: &RefusalRecord) {
    let _ = append(store, project.unwrap_or(UNSCOPED), entry);
}

/// Private on purpose: a caller holding a `Result` here would eventually be
/// tempted to act on it, and acting on it is what this log must not do.
fn append(store: &Store, project: &str, entry: &RefusalRecord) -> Result<()> {
    let path = log_path(store, project);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(entry)
        .map_err(|e| Error::Store(format!("cannot serialize refusal record: {e}")))?;
    line.push('\n');
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// The last `n` records, oldest first. Unparseable lines are skipped — a
/// torn final write must not hide the history before it.
pub fn tail(store: &Store, project: &str, n: usize) -> Result<Vec<RefusalRecord>> {
    let path = log_path(store, project);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    let mut records: Vec<RefusalRecord> = raw
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if records.len() > n {
        records.drain(..records.len() - n);
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let world =
            std::env::temp_dir().join(format!("corpus-refusal-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    /// The point: a refused call is reconstructable from the log alone,
    /// without reading the transcript.
    #[test]
    fn a_refusal_is_reconstructable_from_the_log_alone() {
        let store = tmp_store("round-trip");
        let mut entry = RefusalRecord::new(
            "sandbox_exec",
            Gate::Role,
            "refusing sandbox_exec: agent role \"researcher\" does not grant it.",
        );
        entry.actor = "curator:discover".to_string();
        entry.role = Some("researcher".to_string());
        entry.args = r#"{"command":"ls /opt/src"}"#.to_string();
        entry.run_log = Some("1786938993-7c493193.raw".to_string());
        record(&store, Some("deepseek"), &entry);

        let log = tail(&store, "deepseek", 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].tool, "sandbox_exec");
        assert_eq!(log[0].gate, Gate::Role);
        assert_eq!(log[0].role.as_deref(), Some("researcher"));
        assert_eq!(log[0].actor, "curator:discover");
        assert!(log[0].args.contains("/opt/src"));
        assert_eq!(
            log[0].run_log.as_deref(),
            Some("1786938993-7c493193.raw"),
            "without the run_log key a line cannot be placed in the transcript"
        );
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// Oldest first and append-only — the same reading contract as the
    /// audit log, so `--tail` means the same thing in both.
    #[test]
    fn the_log_only_ever_grows() {
        let store = tmp_store("append");
        for i in 0..3 {
            record(
                &store,
                Some("p"),
                &RefusalRecord::new(format!("tool{i}"), Gate::Probe, "harness not ready"),
            );
        }
        let all = tail(&store, "p", 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].tool, "tool0", "oldest first");
        assert_eq!(all[2].tool, "tool2");
        assert_eq!(tail(&store, "p", 2).unwrap()[0].tool, "tool1");
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// An agent that could edit the record of its own refusals would make
    /// the log worthless exactly when it is interesting.
    #[test]
    fn the_log_lives_outside_the_project_it_describes() {
        let store = tmp_store("outside");
        let path = log_path(&store, "p");
        assert!(!path.starts_with(store.project_dir("p")));
        assert!(path.starts_with(store.var_dir()));
        assert!(tail(&store, "never-touched", 10).unwrap().is_empty());
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// The refusals worth reading most are the ones where the project could
    /// not be resolved, so they must land somewhere.
    #[test]
    fn a_refusal_with_no_project_still_lands() {
        let store = tmp_store("unscoped");
        record(
            &store,
            None,
            &RefusalRecord::new("target_info", Gate::Scope, "no usable project scope"),
        );
        let log = tail(&store, UNSCOPED, 10).unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].gate, Gate::Scope);
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// Reachable: the scope gate refuses whatever `CORPUS_PROJECT` says,
    /// and logging that refusal is the point.
    #[test]
    fn a_malformed_project_cannot_escape_the_refusal_dir() {
        let store = tmp_store("traversal");
        let dir = store.var_dir().join("refusals");
        for hostile in ["../../etc/passwd", "..", ".", "", "a/b", "x\0y"] {
            let path = log_path(&store, hostile);
            assert_eq!(
                path.parent(),
                Some(dir.as_path()),
                "{hostile:?} escaped the refusals dir"
            );
            record(
                &store,
                Some(hostile),
                &RefusalRecord::new("t", Gate::Scope, "bad project"),
            );
            assert!(path.is_file(), "{hostile:?} was dropped instead of logged");
        }
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// The contract that separates this from the audit log. Proven by
    /// making the write impossible: the parent of the refusals dir is a
    /// FILE, so `create_dir_all` cannot succeed.
    #[test]
    fn an_unwritable_log_is_survivable() {
        let store = tmp_store("unwritable");
        let var = store.var_dir();
        fs::create_dir_all(&var).unwrap();
        fs::write(var.join("refusals"), b"not a directory").unwrap();
        // The assertion is that this returns at all.
        record(
            &store,
            Some("p"),
            &RefusalRecord::new("sandbox_exec", Gate::Role, "denied"),
        );
        assert!(tail(&store, "p", 10).unwrap().is_empty());
        let _ = fs::remove_dir_all(&var);
    }
}
