use corpus_store::{Mission, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::common::{live_label, status_label};
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

const READ_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Read,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::None,
    refresh: RefreshPolicy::None,
};

pub(crate) static LIST: ToolDefinition = ToolDefinition {
    name: "mission_list",
    description: "List a project's missions — per row: slug, agent, budget, and `live`. `live` only reports whether a launch session for the mission currently exists (yes) or not (no); a finished agent parked at its prompt STILL reads live=yes, so this is not a working/done signal. To tell whether an agent is actually running, waiting, or idle, use mission_status. For a mission's brief and pins, use mission_get.",
    input_schema: input_schema::<MissionListArgs>,
    handler: mission_list,
    policy: READ_POLICY,
};

pub(crate) static GET: ToolDefinition = ToolDefinition {
    name: "mission_get",
    description: "Read one mission in full: its agent, budget, source pins, and brief body. The `live` line means only that a launch session exists — not that the agent is working. For run state (running / waiting / idle), use mission_status.",
    input_schema: input_schema::<MissionGetArgs>,
    handler: mission_get,
    policy: READ_POLICY,
};

pub(crate) static STATUS: ToolDefinition = ToolDefinition {
    name: "mission_status",
    description: "Read ONE immediate snapshot of mission run state: 'running' (the agent is producing now), 'waiting · last active <dur>' (session live but parked), or 'idle' (nothing up). This — not the `live` flag from mission_list/mission_get — distinguishes work from a parked session. Omit 'mission' for every project mission, or name one. Do not call this in a polling loop. Agent roles should finish their turn after dispatch; `mission_await` remains only as a one-shot operator diagnostic.",
    input_schema: input_schema::<MissionStatusArgs>,
    handler: mission_status,
    policy: READ_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionListArgs {
    project: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionGetArgs {
    project: String,
    mission: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionStatusArgs {
    project: String,
    /// Optional — one mission's status; omitted means all project missions.
    mission: Option<String>,
}

fn mission_list(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionListArgs = parse_args(LIST.name, value)?;
    let missions = ctx
        .store
        .list_missions(&args.project)
        .map_err(|error| Error::Args(error.to_string()))?;
    let live = corpus_observe::live_tui_sessions();
    Ok(missions
        .iter()
        .map(|(slug, mission)| {
            format!(
                "{:<20} agent={} budget={} live={}",
                slug,
                mission.agent,
                mission.budget.as_deref().unwrap_or("-"),
                live_label(mission, &live)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn mission_get(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionGetArgs = parse_args(GET.name, value)?;
    let mission = ctx
        .store
        .load_mission(&args.project, &args.mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    let brief = ctx
        .store
        .mission_brief(&args.project, &args.mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    let live = corpus_observe::live_tui_sessions();
    Ok(format!(
        "--- mission {}/{} ---\nagent: {}\nbudget: {}\nlive: {}\npins: {:?}\n\n{}",
        args.project,
        args.mission,
        mission.agent,
        mission.budget.as_deref().unwrap_or("-"),
        live_label(&mission, &live),
        mission.pins,
        brief
    ))
}

fn mission_status(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionStatusArgs = parse_args(STATUS.name, value)?;
    let live = corpus_observe::live_tui_sessions();
    let rows: Vec<(String, Mission)> = match args.mission {
        Some(slug) => {
            let mission = ctx
                .store
                .load_mission(&args.project, &slug)
                .map_err(|error| Error::Args(error.to_string()))?;
            vec![(slug, mission)]
        }
        None => ctx
            .store
            .list_missions(&args.project)
            .map_err(|error| Error::Args(error.to_string()))?,
    };
    if rows.is_empty() {
        return Ok(format!("no missions on {}", args.project));
    }
    Ok(rows
        .iter()
        .map(|(slug, mission)| {
            let state = corpus_observe::mission_run_state(ctx.store, &args.project, mission, &live);
            format!("{:<24} {}", slug, status_label(&state))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_schemas_and_deserializers_share_mission_read_contracts() {
        assert_eq!(
            LIST.catalog_entry()["inputSchema"]["required"],
            json!(["project"])
        );
        assert_eq!(
            GET.catalog_entry()["inputSchema"]["required"],
            json!(["project", "mission"])
        );
        assert_eq!(
            STATUS.catalog_entry()["inputSchema"]["required"],
            json!(["project"])
        );
        assert!(parse_args::<MissionListArgs>(LIST.name, &json!({"project": 42})).is_err());
        assert!(parse_args::<MissionGetArgs>(GET.name, &json!({"project": "p"})).is_err());
        assert!(parse_args::<MissionStatusArgs>(
            STATUS.name,
            &json!({"project": "p", "mission": 42})
        )
        .is_err());
        assert!(parse_args::<MissionStatusArgs>(
            STATUS.name,
            &json!({"project": "p", "future_field": true})
        )
        .is_ok());
    }

    #[test]
    fn mission_reads_are_immediate_unconfirmed_and_side_effect_free() {
        for tool in [&LIST, &GET, &STATUS] {
            assert_eq!(tool.policy.kind, ToolKind::Read);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::None);
            assert_eq!(tool.policy.refresh, RefreshPolicy::None);
        }
    }
}
