//! Confirmation-gated agent deletion and consequence projection.

use corpus_store::{MissionRunRef, Store};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::confirmation::{confirm_and_run, mint_confirm};
use crate::error::{Error, Result};
use crate::Ctx;

use super::super::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};

pub(crate) static DELETE: ToolDefinition = ToolDefinition {
    name: "agent_delete",
    description: "CONFIRM-GATED. Delete an agent and every mission assigned to it, lifecycle-tearing down those missions first when needed. Dry-run first; returns a one-shot token and lists the missions that will also be deleted.",
    input_schema: input_schema::<AgentDeleteArgs>,
    handler: agent_delete,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Destructive,
        confirmation: ConfirmationPolicy::Token,
        audit: AuditPolicy::Category("agents"),
        refresh: RefreshPolicy::Area("agents"),
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentDeleteArgs {
    project: String,
    agent: String,
    confirm_token: Option<String>,
}

fn agent_delete(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentDeleteArgs = parse_args(DELETE.name, value)?;
    let target = format!("{}/{}", args.project, args.agent);
    if let Some(token) = args.confirm_token {
        confirm_and_run(ctx, DELETE.name, &target, &token, |store| {
            let missions = store
                .missions_for_agent(&args.project, &args.agent)
                .map_err(|error| Error::Args(error.to_string()))?;
            if missions.iter().any(|mission| {
                store
                    .ensure_mission_deletable(&args.project, mission)
                    .is_err()
            }) {
                store
                    .request_agent_delete(&args.project, &args.agent)
                    .map_err(|error| Error::Args(error.to_string()))?;
                Ok(format!(
                    "requested deletion of agent {}/{}; the app will tear down {} assigned mission(s) first{}",
                    args.project,
                    args.agent,
                    missions.len(),
                    mission_list_suffix(&missions)
                ))
            } else {
                store
                    .delete_agent(&args.project, &args.agent)
                    .map_err(|error| Error::Args(error.to_string()))?;
                Ok(format!(
                    "deleted agent {}/{} and {} assigned mission(s){}",
                    args.project,
                    args.agent,
                    missions.len(),
                    mission_list_suffix(&missions)
                ))
            }
        })
    } else {
        // Load the target before minting a token so a typo fails during the
        // dry run and the preview can state every consequence.
        let config = ctx
            .store
            .load_agent(&args.project, &args.agent)
            .map_err(|error| Error::Args(error.to_string()))?;
        let subagents = config
            .doc
            .get("agent")
            .and_then(Value::as_object)
            .map_or(0, |entries| entries.len().saturating_sub(1));
        let orphaned = delegation_dependents(ctx.store, &args.project, &args.agent);
        let missions = ctx
            .store
            .missions_for_agent(&args.project, &args.agent)
            .map_err(|error| Error::Args(error.to_string()))?;
        let consequence = if orphaned.is_empty() {
            String::new()
        } else {
            format!(
                "; {} would be left delegating to entries this removes ({}), and the next \
                 launch would refuse to render the project until that is fixed",
                orphaned.len(),
                orphaned.join(", ")
            )
        };
        mint_confirm(
            ctx,
            DELETE.name,
            &target,
            &format!(
                "DRY RUN — would delete agent {}/{} (role {}, {subagents} subagent(s)) and {} assigned mission(s){}{consequence}",
                args.project,
                args.agent,
                config.meta.role().as_str(),
                missions.len(),
                mission_list_suffix(&missions)
            ),
        )
    }
}

fn mission_list_suffix(missions: &[String]) -> String {
    if missions.is_empty() {
        String::new()
    } else {
        format!(": {}", missions.join(", "))
    }
}

/// Agents that delegate to an entry the target agent declares. Deleting the
/// target would leave those task rules dangling and make project rendering
/// fail closed.
fn delegation_dependents(store: &Store, project: &str, agent: &str) -> Vec<String> {
    let Ok(agents) = store.list_agents(project) else {
        return Vec::new();
    };
    let entries: Vec<String> = agents
        .iter()
        .find(|(slug, _)| slug == agent)
        .and_then(|(_, config)| config.doc.get("agent").and_then(Value::as_object))
        .map(|entries| entries.keys().cloned().collect())
        .unwrap_or_default();
    let mut dependents = Vec::new();
    for (slug, config) in &agents {
        if slug == agent {
            continue;
        }
        let Some(agent_entries) = config.doc.get("agent").and_then(Value::as_object) else {
            continue;
        };
        if agent_entries
            .values()
            .any(|entry| delegates_to_any(entry, &entries))
        {
            dependents.push(slug.clone());
        }
    }
    dependents
}

fn delegates_to_any(entry: &Value, targets: &[String]) -> bool {
    entry
        .get("permission")
        .and_then(|permission| permission.get("task"))
        .and_then(Value::as_object)
        .is_some_and(|rules| has_allowed_target(rules, targets))
}

fn has_allowed_target(rules: &Map<String, Value>, targets: &[String]) -> bool {
    rules.iter().any(|(name, action)| {
        action.as_str() != Some("deny") && targets.iter().any(|target| target == name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_contract_and_policy_require_typed_confirmation() {
        let schema = DELETE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "agent"]));
        assert!(parse_args::<AgentDeleteArgs>(
            DELETE.name,
            &json!({"project": "p", "agent": "a", "confirm_token": 42})
        )
        .is_err());
        assert_eq!(DELETE.policy.kind, ToolKind::Destructive);
        assert_eq!(DELETE.policy.confirmation, ConfirmationPolicy::Token);
        assert_eq!(DELETE.policy.audit, AuditPolicy::Category("agents"));
        assert_eq!(DELETE.policy.refresh, RefreshPolicy::Area("agents"));
    }

    #[test]
    fn consequence_helpers_report_missions_and_only_allowed_delegations() {
        assert_eq!(mission_list_suffix(&[]), "");
        assert_eq!(
            mission_list_suffix(&["one".into(), "two".into()]),
            ": one, two"
        );
        let targets = vec!["helper".to_string()];
        assert!(delegates_to_any(
            &json!({"permission": {"task": {"helper": "allow"}}}),
            &targets
        ));
        assert!(!delegates_to_any(
            &json!({"permission": {"task": {"helper": "deny"}}}),
            &targets
        ));
    }
}
