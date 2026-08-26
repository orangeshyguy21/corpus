//! Origin-preserving mission launch requests.

use corpus_store::{MissionLaunchRequest, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::common::{load_project, now};
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

pub(crate) static LAUNCH: ToolDefinition = ToolDefinition {
    name: "mission_launch",
    description: "Launch a mission: the app spawns a full opencode TUI session for it and kicks it off with the mission's brief as the prompt. The operator can watch and steer the session live in the app. Use this when a mission is ready to run — mission_new only writes the record; this starts it. The launch happens the moment the app picks up the request; a mission whose session is already live is left alone.",
    input_schema: input_schema::<MissionLaunchArgs>,
    handler: mission_launch,
    policy: WRITE_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionLaunchArgs {
    project: String,
    mission: String,
}

fn validate_origin_project(project: &str, origin: Option<&MissionRunRef>) -> Result<()> {
    if origin.is_some_and(|origin| origin.project != project) {
        return Err(Error::Args(
            "launch origin does not belong to the proven project scope".into(),
        ));
    }
    Ok(())
}

fn mission_launch(
    ctx: &mut Ctx<'_>,
    value: &Value,
    origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionLaunchArgs = parse_args(LAUNCH.name, value)?;
    validate_origin_project(&args.project, origin)?;
    let mut mission = ctx
        .store
        .load_mission(&args.project, &args.mission)
        .map_err(|error| Error::Args(error.to_string()))?;
    if load_project(ctx.store, &args.project)?
        .delete_requested
        .is_some()
    {
        return Err(Error::Args(format!(
            "project {} is pending deletion",
            args.project
        )));
    }
    if ctx
        .store
        .load_agent(&args.project, &mission.agent)
        .map_err(|error| Error::Args(error.to_string()))?
        .meta
        .delete_requested
        .is_some()
    {
        return Err(Error::Args(format!(
            "agent {}/{} is pending deletion",
            args.project, mission.agent
        )));
    }
    if mission.delete_requested.is_some() {
        return Err(Error::Args(format!(
            "mission {}/{} is pending deletion",
            args.project, args.mission
        )));
    }
    if mission.launch_requested.is_none() {
        mission.launch_requested = Some(MissionLaunchRequest {
            requested_at: now(),
            requested_by: origin.cloned(),
        });
        ctx.store
            .update_mission(&args.project, &args.mission, &mission)
            .map_err(|error| Error::Args(error.to_string()))?;
    }
    Ok(format!(
        "launch requested for {}/{} — the app will spawn its opencode session and kick it off with the brief",
        args.project, args.mission
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contract_excludes_model_supplied_origin() {
        let schema = LAUNCH.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "mission"]));
        assert!(schema["properties"].get("requested_by").is_none());

        let args: MissionLaunchArgs = parse_args(
            LAUNCH.name,
            &json!({
                "project": "p",
                "mission": "child",
                "requested_by": {"project": "spoof"}
            }),
        )
        .unwrap();
        assert_eq!(args.mission, "child");
        assert!(parse_args::<MissionLaunchArgs>(
            LAUNCH.name,
            &json!({"project": "p", "mission": 42})
        )
        .is_err());
    }

    #[test]
    fn launcher_origin_must_match_the_proven_project() {
        let matching = MissionRunRef {
            project: "p".into(),
            mission: "curator".into(),
            run_id: "run-1".into(),
        };
        assert!(validate_origin_project("p", Some(&matching)).is_ok());
        assert!(validate_origin_project("p", None).is_ok());

        let mismatched = MissionRunRef {
            project: "other".into(),
            ..matching
        };
        assert!(validate_origin_project("p", Some(&mismatched)).is_err());
    }

    #[test]
    fn launch_is_an_audited_refreshing_write() {
        assert_eq!(LAUNCH.policy.kind, ToolKind::Write);
        assert_eq!(LAUNCH.policy.confirmation, ConfirmationPolicy::None);
        assert_eq!(LAUNCH.policy.audit, AuditPolicy::Category("missions"));
        assert_eq!(LAUNCH.policy.refresh, RefreshPolicy::Area("missions"));
    }
}
