use corpus_store::{AddSubagentRequest, AgentRole, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::super::registry::{input_schema, parse_args, ToolDefinition};
use super::common::{AgentRoleArg, WRITE_POLICY};
use crate::error::{Error, Result};
use crate::Ctx;

pub(crate) static SET: ToolDefinition = ToolDefinition {
    name: "agent_set",
    description: "Set ONE field of an agent (or of one of its subagents) without resending the whole document: model, description, prompt, or temperature. Prefer this over agent_save for a single change. Pass null to clear a field.",
    input_schema: input_schema::<AgentSetArgs>, handler: agent_set, policy: WRITE_POLICY,
};
pub(crate) static SET_ROLE: ToolDefinition = ToolDefinition {
    name: "agent_set_role",
    description: "Set an agent's ROLE — the capability ceiling the corpus server enforces for missions launched as it. super = every current-project research, sandbox, corpus and management capability, including confirmation-gated corpus wipe; curator = scoped project management with agent/mission/entry deletion but no wipe, sandbox or internet; tester = sandbox/oracle/faucet/findings, no internet; researcher = read + technique_save + internet. Cross-project and project-lifecycle administration remain operator-only. A role also regenerates permissions at launch.",
    input_schema: input_schema::<AgentSetRoleArgs>, handler: agent_set_role, policy: WRITE_POLICY,
};
pub(crate) static SET_PERMISSION: ToolDefinition = ToolDefinition {
    name: "agent_set_permission",
    description: "MERGE a permission patch into an agent (or subagent) entry — top-level keys replace, null removes, everything else is left alone. Note the role ceiling still wins: granting a corpus_* tool outside the agent's role has no effect at launch.",
    input_schema: input_schema::<AgentSetPermissionArgs>, handler: agent_set_permission, policy: WRITE_POLICY,
};
pub(crate) static SUBAGENT_ADD: ToolDefinition = ToolDefinition {
    name: "agent_subagent_add",
    description: "Add a subagent to an agent's document and wire the primary's task: permission to allow delegating to it.",
    input_schema: input_schema::<AgentSubagentAddArgs>, handler: agent_subagent_add, policy: WRITE_POLICY,
};
pub(crate) static SUBAGENT_REMOVE: ToolDefinition = ToolDefinition {
    name: "agent_subagent_remove",
    description: "Remove a subagent entry, its delegation rule, and its role.",
    input_schema: input_schema::<AgentSubagentRemoveArgs>,
    handler: agent_subagent_remove,
    policy: WRITE_POLICY,
};

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum AgentFieldArg {
    Model,
    Description,
    Prompt,
    Temperature,
}
impl AgentFieldArg {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Description => "description",
            Self::Prompt => "prompt",
            Self::Temperature => "temperature",
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct AgentSetArgs {
    project: String,
    agent: String,
    /// Target this subagent entry instead of the primary.
    subagent: Option<String>,
    field: AgentFieldArg,
    /// The new value; null clears the field.
    value: Value,
}
#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentSetRoleArgs {
    project: String,
    agent: String,
    /// Set a subagent's role instead; it remains capped by the primary.
    subagent: Option<String>,
    role: AgentRoleArg,
}
#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct AgentSetPermissionArgs {
    project: String,
    agent: String,
    subagent: Option<String>,
    /// For example: `{"webfetch": "allow", "bash": null}`.
    patch: serde_json::Map<String, Value>,
}
#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentSubagentAddArgs {
    project: String,
    agent: String,
    /// Kebab-case entry name, unique across the project.
    name: String,
    description: String,
    prompt: String,
    model: Option<String>,
    role: Option<AgentRoleArg>,
}
#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentSubagentRemoveArgs {
    project: String,
    agent: String,
    name: String,
}

fn normalized_subagent(subagent: Option<String>) -> Option<String> {
    subagent.filter(|name| !name.is_empty())
}

fn agent_set(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentSetArgs = parse_args(SET.name, value)?;
    let field = args.field.as_str();
    let subagent = normalized_subagent(args.subagent);
    ctx.store
        .set_agent_field(
            &args.project,
            &args.agent,
            subagent.as_deref(),
            field,
            args.value,
        )
        .map_err(|error| Error::Args(error.to_string()))?;
    let target = subagent.unwrap_or_else(|| args.agent.clone());
    Ok(format!(
        "set {field} on {}/{} entry {target} (validator passed)",
        args.project, args.agent
    ))
}

fn agent_set_role(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentSetRoleArgs = parse_args(SET_ROLE.name, value)?;
    let role = AgentRole::from(args.role);
    match normalized_subagent(args.subagent) {
        Some(subagent) => {
            ctx.store
                .set_subagent_role(&args.project, &args.agent, &subagent, role)
                .map_err(|error| Error::Args(error.to_string()))?;
            Ok(format!(
                "{}/{} subagent {subagent}: role -> {} (capped by the primary's at launch)",
                args.project,
                args.agent,
                role.as_str()
            ))
        }
        None => {
            ctx.store
                .set_agent_role(&args.project, &args.agent, role)
                .map_err(|error| Error::Args(error.to_string()))?;
            Ok(format!(
                "{}/{}: role -> {} (server-enforced; grants {})",
                args.project,
                args.agent,
                role.as_str(),
                role.tools()
                    .iter()
                    .map(|tool| tool.trim_start_matches("corpus_"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

fn agent_set_permission(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentSetPermissionArgs = parse_args(SET_PERMISSION.name, value)?;
    let subagent = normalized_subagent(args.subagent);
    ctx.store
        .patch_agent_permission(
            &args.project,
            &args.agent,
            subagent.as_deref(),
            &Value::Object(args.patch),
        )
        .map_err(|error| Error::Args(error.to_string()))?;
    let target = subagent.unwrap_or_else(|| args.agent.clone());
    Ok(format!(
        "patched permissions on {}/{} entry {target} (validator passed)",
        args.project, args.agent
    ))
}

fn agent_subagent_add(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentSubagentAddArgs = parse_args(SUBAGENT_ADD.name, value)?;
    let request = AddSubagentRequest {
        project: args.project.clone(),
        agent: args.agent.clone(),
        name: args.name.clone(),
        description: args.description,
        prompt: args.prompt,
        model: args.model,
        role: args.role.map(AgentRole::from),
    };
    ctx.store
        .add_subagent(&request)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "added subagent {} to {}/{} (delegation wired)",
        args.name, args.project, args.agent
    ))
}

fn agent_subagent_remove(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentSubagentRemoveArgs = parse_args(SUBAGENT_REMOVE.name, value)?;
    ctx.store
        .remove_subagent(&args.project, &args.agent, &args.name)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "removed subagent {} from {}/{}",
        args.name, args.project, args.agent
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::{AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolKind};
    use serde_json::json;

    #[test]
    fn generated_contracts_type_granular_mutations_without_losing_null_clear() {
        let set_schema = SET.catalog_entry()["inputSchema"].clone();
        let rendered = set_schema.to_string();
        for field in ["model", "description", "prompt", "temperature"] {
            assert!(rendered.contains(field));
        }
        let clear: AgentSetArgs = parse_args(
            SET.name,
            &json!({"project":"p","agent":"a","subagent":"","field":"model","value":null}),
        )
        .unwrap();
        assert_eq!(clear.value, Value::Null);
        assert_eq!(normalized_subagent(clear.subagent), None);
        assert!(parse_args::<AgentSetArgs>(
            SET.name,
            &json!({"project":"p","agent":"a","field":"owner","value":true})
        )
        .is_err());
        assert!(parse_args::<AgentSetPermissionArgs>(
            SET_PERMISSION.name,
            &json!({"project":"p","agent":"a","patch":[]})
        )
        .is_err());
        assert!(parse_args::<AgentSubagentAddArgs>(SUBAGENT_ADD.name, &json!({"project":"p","agent":"a","name":"helper","description":"d","prompt":"p","role":"owner"})).is_err());
    }

    #[test]
    fn granular_agent_mutations_share_the_agents_write_policy() {
        for tool in [
            &SET,
            &SET_ROLE,
            &SET_PERMISSION,
            &SUBAGENT_ADD,
            &SUBAGENT_REMOVE,
        ] {
            assert_eq!(tool.policy.kind, ToolKind::Write);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::Category("agents"));
            assert_eq!(tool.policy.refresh, RefreshPolicy::Area("agents"));
        }
    }
}
