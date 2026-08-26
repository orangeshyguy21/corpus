//! Mission creation and non-destructive persisted settings.

use std::collections::BTreeMap;

use corpus_store::{Mission, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::common::now;
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

const WRITE_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Write,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::Category("missions"),
    refresh: RefreshPolicy::Area("missions"),
};

pub(crate) static NEW: ToolDefinition = ToolDefinition {
    name: "mission_new",
    description: "Create a mission for an existing agent on the project. 'slug' is the mission's id (kebab-case); 'name' is the human display label shown in the app's mission nav — set it so the operator sees a real name, not a placeholder (defaults to the slug when omitted).",
    input_schema: input_schema::<MissionNewArgs>,
    handler: mission_new,
    policy: WRITE_POLICY,
};

pub(crate) static SET_BUDGET: ToolDefinition = ToolDefinition {
    name: "mission_set_budget",
    description: "Set a mission's execution budget (per-MISSION, never per-agent).",
    input_schema: input_schema::<MissionSetBudgetArgs>,
    handler: mission_set_budget,
    policy: WRITE_POLICY,
};

pub(crate) static SET_PINS: ToolDefinition = ToolDefinition {
    name: "mission_set_pins",
    description: "Set a mission's source pins (repo -> rev map).",
    input_schema: input_schema::<MissionSetPinsArgs>,
    handler: mission_set_pins,
    policy: WRITE_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionNewArgs {
    project: String,
    /// Kebab-case mission id.
    slug: String,
    agent: String,
    brief: String,
    /// Operator-facing display name for mission navigation.
    name: Option<String>,
    budget: Option<String>,
    /// Optional per-source overrides; omitted sources inherit project pins.
    pins: Option<BTreeMap<String, String>>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionSetBudgetArgs {
    project: String,
    mission: String,
    budget: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionSetPinsArgs {
    project: String,
    mission: String,
    pins: BTreeMap<String, String>,
}

fn normalized_name(name: Option<String>) -> Option<String> {
    name.map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn validate_pins(ctx: &Ctx<'_>, project: &str, pins: &BTreeMap<String, String>) -> Result<()> {
    for (repository, revision) in pins {
        corpus_observe::validate_pin(ctx.store, project, repository, revision)
            .map_err(|error| Error::Args(error.to_string()))?;
    }
    Ok(())
}

fn mission_new(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionNewArgs = parse_args(NEW.name, value)?;
    let mut pins = corpus_observe::project_source_pins(ctx.store, &args.project)
        .map_err(|error| Error::Args(error.to_string()))?;
    pins.extend(args.pins.unwrap_or_default());
    validate_pins(ctx, &args.project, &pins)?;
    let mission = Mission {
        agent: args.agent,
        pins,
        budget: args.budget,
        created: now(),
        name: normalized_name(args.name),
        session: None,
        control: None,
        opencode_session: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    ctx.store
        .write_mission(&args.project, &args.slug, &mission, &args.brief)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "created mission {}/{} (agent {})",
        args.project, args.slug, mission.agent
    ))
}

fn mission_set_budget(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionSetBudgetArgs = parse_args(SET_BUDGET.name, value)?;
    let mut mission = ctx
        .store
        .load_mission(&args.project, &args.mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    let old = mission.budget.replace(args.budget.clone());
    ctx.store
        .update_mission(&args.project, &args.mission, &mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "set mission {}/{} budget: {} -> {} (budget is per-MISSION)",
        args.project,
        args.mission,
        old.as_deref().unwrap_or("-"),
        args.budget
    ))
}

fn mission_set_pins(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionSetPinsArgs = parse_args(SET_PINS.name, value)?;
    let mut mission = ctx
        .store
        .load_mission(&args.project, &args.mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    validate_pins(ctx, &args.project, &args.pins)?;
    mission.pins = args.pins;
    ctx.store
        .update_mission(&args.project, &args.mission, &mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "set mission {}/{} pins: {:?}",
        args.project, args.mission, mission.pins
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contracts_type_creation_budget_and_pin_maps() {
        assert_eq!(
            NEW.catalog_entry()["inputSchema"]["required"],
            json!(["project", "slug", "agent", "brief"])
        );
        assert_eq!(
            SET_BUDGET.catalog_entry()["inputSchema"]["required"],
            json!(["project", "mission", "budget"])
        );
        assert_eq!(
            SET_PINS.catalog_entry()["inputSchema"]["required"],
            json!(["project", "mission", "pins"])
        );

        let created: MissionNewArgs = parse_args(
            NEW.name,
            &json!({
                "project": "p",
                "slug": "recon",
                "agent": "researcher",
                "brief": "work",
                "name": "  Recon  ",
                "pins": {"source": "main"},
                "future_field": true
            }),
        )
        .unwrap();
        assert_eq!(normalized_name(created.name), Some("Recon".into()));
        assert_eq!(created.pins.unwrap()["source"], "main");
        assert_eq!(normalized_name(Some("  ".into())), None);

        assert!(parse_args::<MissionNewArgs>(
            NEW.name,
            &json!({"project":"p","slug":"m","agent":"a","brief":"b","pins":[]})
        )
        .is_err());
        assert!(parse_args::<MissionSetPinsArgs>(
            SET_PINS.name,
            &json!({"project":"p","mission":"m","pins":{"source":42}})
        )
        .is_err());
        assert!(parse_args::<MissionSetBudgetArgs>(
            SET_BUDGET.name,
            &json!({"project":"p","mission":"m","budget":42})
        )
        .is_err());
    }

    #[test]
    fn mission_persistence_tools_are_audited_refreshing_writes() {
        for tool in [&NEW, &SET_BUDGET, &SET_PINS] {
            assert_eq!(tool.policy.kind, ToolKind::Write);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::Category("missions"));
            assert_eq!(tool.policy.refresh, RefreshPolicy::Area("missions"));
        }
    }
}
