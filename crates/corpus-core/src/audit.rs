//! The audit log: an append-only record of who changed a project.
//!
//! It exists because of the curator role. An agent that can create agents,
//! set roles — including its own — and reorganise a corpus is trusted on
//! the strength of one claim: that the operator can see what it did
//! afterwards. Nothing in the store recorded that before this module, so
//! the claim was not true.
//!
//! Two properties do the work:
//!
//! - **Append-only.** Records are appended as JSONL and never rewritten, so
//!   a later act cannot tidy away an earlier one.
//! - **Out of reach.** The log lives at `<var>/audit/<project>.jsonl`, a
//!   sibling of the run and chat dirs and OUTSIDE the project subtree. A
//!   rendered agent's write rules are `'*': deny` with only its own corpus
//!   re-allowed, and `entry_delete` is rooted at `corpus/` and
//!   category-gated, so the subject of the log cannot edit it.
//!
//! Intent is recorded BEFORE the call and the outcome after, so a mutation
//! that panics, hangs or half-succeeds still leaves a trace of having been
//! attempted. The caller treats a failure to record as a refusal: if an act
//! cannot be accounted for, it does not happen.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::Store;

/// Where a record sits in the lifecycle of one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// About to be attempted. Written before the call, so an act that never
    /// returns is still visible.
    Intent,
    /// Completed.
    Ok,
    /// Refused, or failed. `detail` carries the reason.
    Refused,
}

/// One line of the log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    /// Epoch seconds.
    pub ts: u64,
    /// Who acted: `curator:<agent-slug>`, or `operator` for a human at the
    /// CLI or the management chat.
    pub actor: String,
    /// The tool name.
    pub op: String,
    /// What it acted on, in the project's own terms
    /// (`agents/<slug>`, `missions/<slug>`, `corpus/<rel>`).
    pub target: String,
    pub outcome: Outcome,
    /// The dry-run summary, the result, or the error.
    pub detail: String,
}

impl AuditRecord {
    pub fn new(
        actor: impl Into<String>,
        op: impl Into<String>,
        target: impl Into<String>,
        outcome: Outcome,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            ts: crate::store::now_epoch(),
            actor: actor.into(),
            op: op.into(),
            target: target.into(),
            outcome,
            detail: detail.into(),
        }
    }
}

/// The log file for a project.
pub fn log_path(store: &Store, project: &str) -> PathBuf {
    store.var_dir().join("audit").join(format!("{project}.jsonl"))
}

/// Append one record. Creates the file and its directory on first use.
///
/// Errors are returned rather than swallowed: the caller's contract is that
/// an unrecordable act is refused, and a silent failure here would turn the
/// whole mechanism into decoration exactly when it matters.
pub fn append(store: &Store, project: &str, record: &AuditRecord) -> Result<()> {
    let path = log_path(store, project);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut line = serde_json::to_string(record)
        .map_err(|e| Error::Store(format!("cannot serialize audit record: {e}")))?;
    line.push('\n');
    let mut file = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}

/// The last `n` records, oldest first. Unparseable lines are skipped rather
/// than failing the read — a truncated tail must not hide the rest.
pub fn tail(store: &Store, project: &str, n: usize) -> Result<Vec<AuditRecord>> {
    let path = log_path(store, project);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(&path)?;
    let mut records: Vec<AuditRecord> = raw
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
        let world = std::env::temp_dir().join(format!("corpus-audit-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    /// The log accumulates and never rewrites: a second act cannot erase
    /// the record of the first.
    #[test]
    fn the_log_only_ever_grows() {
        let store = tmp_store("append");
        for i in 0..3 {
            append(
                &store,
                "p",
                &AuditRecord::new(
                    "curator:keeper",
                    "agent_set_role",
                    format!("agents/a{i}"),
                    Outcome::Ok,
                    "set to tester",
                ),
            )
            .unwrap();
        }
        let all = tail(&store, "p", 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].target, "agents/a0", "oldest first");
        assert_eq!(all[2].target, "agents/a2");
        assert_eq!(all[0].actor, "curator:keeper");

        let last_two = tail(&store, "p", 2).unwrap();
        assert_eq!(last_two.len(), 2);
        assert_eq!(last_two[0].target, "agents/a1");
        let _ = fs::remove_dir_all(store.var_dir());
    }

    /// It sits OUTSIDE the project subtree — that is what puts it beyond
    /// the reach of the agent it describes.
    #[test]
    fn the_log_lives_outside_the_project_it_describes() {
        let store = tmp_store("outside");
        let path = log_path(&store, "p");
        assert!(
            !path.starts_with(store.project_dir("p")),
            "an agent can write its own project; the log must not be in it"
        );
        assert!(path.starts_with(store.var_dir()));
        // And a project with no log yet reads as empty rather than failing.
        assert!(tail(&store, "never-touched", 10).unwrap().is_empty());
    }
}
