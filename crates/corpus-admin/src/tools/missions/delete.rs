//! Confirmation-gated mission deletion and lifecycle teardown requests.

use corpus_store::{MissionDeleteRequest, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::common::live_label;
use crate::common::now;
use crate::confirmation::{confirm_and_run, mint_confirm};
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

const DELETE_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Destructive,
    confirmation: ConfirmationPolicy::Token,
    audit: AuditPolicy::Category("missions"),
    refresh: RefreshPolicy::Area("missions"),
};

pub(crate) static DELETE: ToolDefinition = ToolDefinition {
    name: "mission_delete",
    description: "CONFIRM-GATED. Request mission deletion. The app tears down any run and plugin environment first, then removes the record; cleanup failures retain the mission for retry. Dry-run first; returns a one-shot token to complete.",
    input_schema: input_schema::<MissionDeleteArgs>,
    handler: mission_delete,
    policy: DELETE_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct MissionDeleteArgs {
    project: String,
    mission: String,
    confirm_token: Option<String>,
}

fn dry_run_summary(
    project: &str,
    mission: &str,
    agent: &str,
    live: &str,
    budget: Option<&str>,
) -> String {
    format!(
        "DRY RUN — would delete mission {project}/{mission} (agent {agent}, live {live}{})",
        budget
            .map(|budget| format!(", budget {budget}"))
            .unwrap_or_default()
    )
}

fn mission_delete(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: MissionDeleteArgs = parse_args(DELETE.name, value)?;
    let target = format!("{}/{}", args.project, args.mission);
    if let Some(token) = args.confirm_token {
        confirm_and_run(ctx, DELETE.name, &target, &token, |store| {
            let mut record = store
                .load_mission(&args.project, &args.mission)
                .map_err(|error| Error::Args(error.to_string()))?;
            if store
                .ensure_mission_deletable(&args.project, &args.mission)
                .is_ok()
            {
                store
                    .delete_mission(&args.project, &args.mission)
                    .map_err(|error| Error::Args(error.to_string()))?;
                return Ok(format!("deleted mission {}/{}", args.project, args.mission));
            }
            record.launch_requested = None;
            if record.delete_requested.is_none() {
                record.delete_requested = Some(MissionDeleteRequest {
                    requested_at: now(),
                });
                store
                    .update_mission(&args.project, &args.mission, &record)
                    .map_err(|error| Error::Args(error.to_string()))?;
            }
            Ok(format!(
                "deletion requested for mission {}/{} — the app will tear down its run and environment before removing the record",
                args.project, args.mission
            ))
        })
    } else {
        let record = ctx
            .store
            .load_mission(&args.project, &args.mission)
            .map_err(|error| Error::Args(error.to_string()))?;
        let live = corpus_observe::live_tui_sessions();
        let summary = dry_run_summary(
            &args.project,
            &args.mission,
            &record.agent,
            live_label(&record, &live),
            record.budget.as_deref(),
        );
        mint_confirm(ctx, DELETE.name, &target, &summary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contract_and_policy_require_typed_confirmation() {
        let schema = DELETE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "mission"]));
        assert!(schema["properties"].get("confirm_token").is_some());
        assert!(parse_args::<MissionDeleteArgs>(
            DELETE.name,
            &json!({"project": "p", "mission": "m", "confirm_token": 42})
        )
        .is_err());

        assert_eq!(DELETE.policy.kind, ToolKind::Destructive);
        assert_eq!(DELETE.policy.confirmation, ConfirmationPolicy::Token);
        assert_eq!(DELETE.policy.audit, AuditPolicy::Category("missions"));
        assert_eq!(DELETE.policy.refresh, RefreshPolicy::Area("missions"));
    }

    #[test]
    fn dry_run_names_liveness_and_optional_budget() {
        assert_eq!(
            dry_run_summary("p", "m", "researcher", "yes", Some("10m")),
            "DRY RUN — would delete mission p/m (agent researcher, live yes, budget 10m)"
        );
        assert_eq!(
            dry_run_summary("p", "m", "researcher", "no", None),
            "DRY RUN — would delete mission p/m (agent researcher, live no)"
        );
    }
}
