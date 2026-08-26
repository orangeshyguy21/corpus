//! Bounded, operator-only mission change diagnostic.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use corpus_observe::{MissionActivity, MissionRunState};
use corpus_store::{Mission, MissionRunRef, Store};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::common::status_label;
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

const AWAIT_POLL: Duration = Duration::from_secs(2);
const AWAIT_DEFAULT_SECS: u64 = 45;
const AWAIT_CAP_SECS: u64 = 90;

const READ_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Read,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::None,
    refresh: RefreshPolicy::None,
};

pub(crate) static AWAIT: ToolDefinition = ToolDefinition {
    name: "mission_await",
    description: "Operator diagnostic: block once (up to timeout_secs, default 45, max 90) until a launched mission changes, then return what changed. Do NOT call this repeatedly in one model turn; agent roles do not receive this tool because Corpus owns background supervision. Omit 'mission' to observe any mission on the project, or name one. While blocked, this MCP session cannot service another call.",
    input_schema: input_schema::<MissionAwaitArgs>,
    handler: mission_await,
    policy: READ_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionAwaitArgs {
    project: String,
    /// Optional — one mission to observe; omitted means every project mission.
    mission: Option<String>,
    /// Optional bounded wait in seconds; defaults to 45 and clamps to 1..=90.
    timeout_secs: Option<u64>,
}

#[derive(Debug, PartialEq, Eq)]
enum WatchScope {
    All,
    One(String),
}

impl From<Option<String>> for WatchScope {
    fn from(mission: Option<String>) -> Self {
        mission.map_or(Self::All, Self::One)
    }
}

fn bounded_timeout(timeout_secs: Option<u64>) -> u64 {
    timeout_secs
        .unwrap_or(AWAIT_DEFAULT_SECS)
        .clamp(1, AWAIT_CAP_SECS)
}

fn watched_missions(
    store: &Store,
    project: &str,
    scope: &WatchScope,
) -> Result<Vec<(String, Mission)>> {
    match scope {
        WatchScope::One(slug) => {
            let mission = store
                .load_mission(project, slug)
                .map_err(|error| Error::Args(error.to_string()))?;
            Ok(vec![(slug.clone(), mission)])
        }
        WatchScope::All => store
            .list_missions(project)
            .map_err(|error| Error::Args(error.to_string())),
    }
}

fn state_snapshot(
    store: &Store,
    project: &str,
    missions: &[(String, Mission)],
    live: &[String],
) -> BTreeMap<String, MissionRunState> {
    missions
        .iter()
        .map(|(slug, mission)| {
            (
                slug.clone(),
                corpus_observe::mission_run_state(store, project, mission, live),
            )
        })
        .collect()
}

fn corpus_entry_set(store: &Store, project: &str) -> BTreeSet<String> {
    let root = store.project_corpus_dir(project);
    let mut entries = BTreeSet::new();
    let mut stack = vec![root.clone()];
    while let Some(directory) = stack.pop() {
        let Ok(children) = std::fs::read_dir(&directory) else {
            continue;
        };
        for child in children.flatten() {
            let path = child.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(&root) {
                entries.insert(relative.to_string_lossy().into_owned());
            }
        }
    }
    entries
}

fn activity_word(activity: MissionActivity) -> &'static str {
    match activity {
        MissionActivity::Working => "running",
        MissionActivity::Waiting => "waiting",
        MissionActivity::Idle => "idle",
    }
}

fn await_report(
    before: &BTreeMap<String, MissionRunState>,
    now: &BTreeMap<String, MissionRunState>,
    new_entries: &[String],
) -> Option<String> {
    let mut lines = Vec::new();
    for (slug, state) in now {
        let previous = before.get(slug).map(|snapshot| snapshot.activity);
        if previous != Some(state.activity) {
            let was = previous.map(activity_word).unwrap_or("new");
            lines.push(format!("{slug}: {was} → {}", status_label(state)));
        }
    }
    if lines.is_empty() && new_entries.is_empty() {
        return None;
    }
    if !new_entries.is_empty() {
        lines.push(format!("new corpus output: {}", new_entries.join(", ")));
    }
    Some(lines.join("\n"))
}

fn mission_await(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionAwaitArgs = parse_args(AWAIT.name, value)?;
    let timeout_secs = bounded_timeout(args.timeout_secs);
    let scope = WatchScope::from(args.mission);
    let missions = watched_missions(ctx.store, &args.project, &scope)?;
    if missions.is_empty() {
        return Ok(format!("no missions on {}", args.project));
    }
    let before = state_snapshot(
        ctx.store,
        &args.project,
        &missions,
        &corpus_observe::live_tui_sessions(),
    );
    let entries_before = corpus_entry_set(ctx.store, &args.project);

    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if Instant::now() >= deadline {
            let live = corpus_observe::live_tui_sessions();
            let status = state_snapshot(ctx.store, &args.project, &missions, &live)
                .iter()
                .map(|(slug, state)| format!("{slug}: {}", status_label(state)))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(format!(
                "no change in {timeout_secs}s — bounded diagnostic wait ended.\n{status}"
            ));
        }
        std::thread::sleep(AWAIT_POLL);
        let missions_now =
            watched_missions(ctx.store, &args.project, &scope).unwrap_or_else(|_| missions.clone());
        let live = corpus_observe::live_tui_sessions();
        let now = state_snapshot(ctx.store, &args.project, &missions_now, &live);
        let new_entries: Vec<String> = corpus_entry_set(ctx.store, &args.project)
            .difference(&entries_before)
            .cloned()
            .collect();
        if let Some(report) = await_report(&before, &now, &new_entries) {
            return Ok(report);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn state(activity: MissionActivity, idle_secs: Option<u64>) -> MissionRunState {
        MissionRunState {
            activity,
            idle_secs,
        }
    }

    fn states(pairs: &[(&str, MissionRunState)]) -> BTreeMap<String, MissionRunState> {
        pairs
            .iter()
            .map(|(slug, state)| (slug.to_string(), *state))
            .collect()
    }

    #[test]
    fn generated_contract_types_timeout_and_exact_watch_selection() {
        let schema = AWAIT.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project"]));
        assert!(schema["properties"].get("timeout_secs").is_some());

        let all: MissionAwaitArgs = parse_args(AWAIT.name, &json!({"project": "p"})).unwrap();
        assert_eq!(WatchScope::from(all.mission), WatchScope::All);
        let one: MissionAwaitArgs = parse_args(
            AWAIT.name,
            &json!({"project": "p", "mission": "recon", "timeout_secs": 3}),
        )
        .unwrap();
        assert_eq!(
            WatchScope::from(one.mission),
            WatchScope::One("recon".into())
        );
        assert!(parse_args::<MissionAwaitArgs>(
            AWAIT.name,
            &json!({"project": "p", "timeout_secs": "fast"})
        )
        .is_err());
    }

    #[test]
    fn timeout_is_defaulted_and_clamped_without_waiting() {
        assert_eq!(bounded_timeout(None), 45);
        assert_eq!(bounded_timeout(Some(0)), 1);
        assert_eq!(bounded_timeout(Some(7)), 7);
        assert_eq!(bounded_timeout(Some(91)), 90);
    }

    #[test]
    fn mission_await_is_an_operator_diagnostic_read() {
        assert_eq!(AWAIT.policy.kind, ToolKind::Read);
        assert_eq!(AWAIT.policy.confirmation, ConfirmationPolicy::None);
        assert_eq!(AWAIT.policy.audit, AuditPolicy::None);
        assert_eq!(AWAIT.policy.refresh, RefreshPolicy::None);
    }

    #[test]
    fn no_change_and_no_output_is_none() {
        let before = states(&[("recon", state(MissionActivity::Working, Some(1)))]);
        let now = states(&[("recon", state(MissionActivity::Working, Some(2)))]);
        assert!(await_report(&before, &now, &[]).is_none());
    }

    #[test]
    fn a_finished_turn_is_reported() {
        let before = states(&[("recon", state(MissionActivity::Working, Some(1)))]);
        let now = states(&[("recon", state(MissionActivity::Waiting, Some(9)))]);
        let report = await_report(&before, &now, &[]).expect("a flip is a wake");
        assert!(report.contains("recon: running → waiting"), "{report}");
    }

    #[test]
    fn new_output_alone_is_reported() {
        let before = states(&[("recon", state(MissionActivity::Working, Some(1)))]);
        let now = states(&[("recon", state(MissionActivity::Working, Some(2)))]);
        let report = await_report(&before, &now, &["techniques/c2.md".to_string()])
            .expect("new output is a wake even with no state flip");
        assert!(
            report.contains("new corpus output: techniques/c2.md"),
            "{report}"
        );
    }
}
