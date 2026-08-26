use corpus_store::{AgentRole, CreateAgentRequest, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::super::registry::{input_schema, parse_args, ToolDefinition};
use super::common::{AgentRoleArg, WRITE_POLICY};
use crate::error::{Error, Result};
use crate::Ctx;

pub(crate) static NEW: ToolDefinition = ToolDefinition {
    name: "agent_new",
    description: "Create a NEW agent from structured fields — the server builds the opencode.json (prefer this over agent_save for creation; agent_save only edits existing agents). Pass 'from' to inherit an existing agent's permissions/prompts (e.g. \"researcher\") with your description/prompt overlaid.",
    input_schema: input_schema::<AgentNewArgs>,
    handler: agent_new,
    policy: WRITE_POLICY,
};

pub(crate) static SAVE: ToolDefinition = ToolDefinition {
    name: "agent_save",
    description: "Validate and save an agent's opencode.json. The core validator runs first; an invalid document is refused with the validator's message.",
    input_schema: input_schema::<AgentSaveArgs>,
    handler: agent_save,
    policy: WRITE_POLICY,
};

pub(crate) static CLONE: ToolDefinition = ToolDefinition {
    name: "agent_clone",
    description: "Clone an agent (config + prompts + subagents) to a new slug WITHIN one project. To copy into a DIFFERENT project use agent_copy.",
    input_schema: input_schema::<AgentCloneArgs>,
    handler: agent_clone,
    policy: WRITE_POLICY,
};

pub(crate) static COPY: ToolDefinition = ToolDefinition {
    name: "agent_copy",
    description: "Copy an agent BETWEEN projects (prompts, subagents and role included). This is the tool for 'copy these agents into that project' — agent_clone cannot cross a project boundary.",
    input_schema: input_schema::<AgentCopyArgs>,
    handler: agent_copy,
    policy: WRITE_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct AgentNewArgs {
    project: String,
    /// Kebab-case slug; also the OpenCode agent name.
    agent: String,
    description: String,
    /// The system prompt body.
    prompt: String,
    /// Optional model id.
    model: Option<String>,
    /// Optional existing agent whose permissions and prompts are inherited.
    from: Option<String>,
    /// Capability ceiling; defaults to researcher or the inherited role.
    role: Option<AgentRoleArg>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
struct AgentSaveArgs {
    project: String,
    agent: String,
    document: serde_json::Map<String, Value>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentCloneArgs {
    project: String,
    from: String,
    to: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentCopyArgs {
    from_project: String,
    from: String,
    to_project: String,
    /// Destination slug; defaults to the source slug.
    to: Option<String>,
}

fn agent_new(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentNewArgs = parse_args(NEW.name, value)?;
    let inherited_from = args.from.clone();
    let request = CreateAgentRequest {
        project: args.project.clone(),
        slug: args.agent.clone(),
        description: args.description,
        prompt: args.prompt,
        model: args.model,
        from: args.from,
        role: args.role.map(AgentRole::from),
    };
    ctx.store
        .create_agent(&request)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "created agent {}/{}{} (validator passed)",
        args.project,
        args.agent,
        inherited_from
            .map(|from| format!(" from {from}"))
            .unwrap_or_default()
    ))
}

fn agent_save(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentSaveArgs = parse_args(SAVE.name, value)?;
    ctx.store
        .save_agent(&args.project, &args.agent, &Value::Object(args.document))
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "saved agent {}/{} (validator passed)",
        args.project, args.agent
    ))
}

fn agent_clone(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: AgentCloneArgs = parse_args(CLONE.name, value)?;
    ctx.store
        .clone_agent(&args.project, &args.from, &args.to)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "cloned agent {}/{} -> {}",
        args.project, args.from, args.to
    ))
}

fn agent_copy(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentCopyArgs = parse_args(COPY.name, value)?;
    let to = args.to.as_deref().unwrap_or(&args.from);
    ctx.store
        .copy_agent(&args.from_project, &args.from, &args.to_project, to)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "copied agent {}/{} -> {}/{}",
        args.from_project, args.from, args.to_project, to
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::registry::{AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolKind};
    use serde_json::json;

    #[test]
    fn generated_schemas_and_deserializers_share_agent_write_contracts() {
        let new_schema = NEW.catalog_entry()["inputSchema"].clone();
        assert_eq!(
            new_schema["required"],
            json!(["project", "agent", "description", "prompt"])
        );
        assert!(new_schema["properties"]["role"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("Capability ceiling")));
        let rendered_schema = new_schema.to_string();
        for role in ["super", "curator", "tester", "researcher"] {
            assert!(rendered_schema.contains(role));
        }

        let copy: AgentCopyArgs = parse_args(
            COPY.name,
            &json!({
                "from_project": "source",
                "from": "researcher",
                "to_project": "destination",
                "future_field": true
            }),
        )
        .unwrap();
        assert_eq!(copy.to, None);
        assert!(parse_args::<AgentNewArgs>(
            NEW.name,
            &json!({
                "project": "p",
                "agent": "a",
                "description": "d",
                "prompt": "p",
                "role": "owner"
            })
        )
        .is_err());
        assert!(parse_args::<AgentSaveArgs>(
            SAVE.name,
            &json!({"project": "p", "agent": "a", "document": []})
        )
        .is_err());
    }

    #[test]
    fn agent_persistence_tools_are_audited_refreshing_writes() {
        for tool in [&NEW, &SAVE, &CLONE, &COPY] {
            assert_eq!(tool.policy.kind, ToolKind::Write);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::Category("agents"));
            assert_eq!(tool.policy.refresh, RefreshPolicy::Area("agents"));
        }
    }
}
