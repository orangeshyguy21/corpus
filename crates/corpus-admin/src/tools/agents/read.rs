use corpus_store::MissionRunRef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::super::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::error::{Error, Result};
use crate::Ctx;

const READ_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Read,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::None,
    refresh: RefreshPolicy::None,
};

pub(crate) static LIST: ToolDefinition = ToolDefinition {
    name: "agent_list",
    description: "List a project's agents (slug, name, config hash).",
    input_schema: input_schema::<AgentListArgs>,
    handler: agent_list,
    policy: READ_POLICY,
};

pub(crate) static GET: ToolDefinition = ToolDefinition {
    name: "agent_get",
    description: "Read an agent's opencode.json document (the config you edit).",
    input_schema: input_schema::<AgentGetArgs>,
    handler: agent_get,
    policy: READ_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentListArgs {
    project: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct AgentGetArgs {
    project: String,
    agent: String,
}

fn agent_list(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentListArgs = parse_args(LIST.name, value)?;
    let agents = ctx
        .store
        .list_agents(&args.project)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(agents
        .iter()
        .map(|(slug, agent)| {
            let description = agent
                .doc
                .get("agent")
                .and_then(Value::as_object)
                .and_then(|entries| {
                    entries
                        .values()
                        .find_map(|config| config.get("description").and_then(Value::as_str))
                })
                .unwrap_or("")
                .replace('\n', " ");
            let description: String = description.chars().take(80).collect();
            format!(
                "{slug} hash={} — {description}",
                ctx.store
                    .agent_config_hash(&args.project, slug)
                    .unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn agent_get(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: AgentGetArgs = parse_args(GET.name, value)?;
    let config = ctx
        .store
        .load_agent(&args.project, &args.agent)
        .map_err(|error| Error::Args(error.to_string()))?;
    serde_json::to_string_pretty(&config.doc).map_err(Error::Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_schemas_and_deserializers_share_agent_read_contracts() {
        let list_schema = LIST.catalog_entry()["inputSchema"].clone();
        assert_eq!(list_schema["properties"]["project"]["type"], "string");
        assert_eq!(list_schema["required"], json!(["project"]));

        let get_schema = GET.catalog_entry()["inputSchema"].clone();
        assert_eq!(get_schema["required"], json!(["project", "agent"]));
        assert!(parse_args::<AgentListArgs>(LIST.name, &json!({"project": 42})).is_err());
        assert!(parse_args::<AgentGetArgs>(GET.name, &json!({"project": "p"})).is_err());
        assert!(parse_args::<AgentGetArgs>(
            GET.name,
            &json!({"project": "p", "agent": "a", "future_field": true})
        )
        .is_ok());
    }

    #[test]
    fn scoped_catalog_removes_only_the_launcher_proven_project_argument() {
        let catalog = crate::scoped_catalog(&[LIST.name, GET.name]);
        let tools = catalog.as_array().unwrap();
        let list = tools.iter().find(|tool| tool["name"] == LIST.name).unwrap();
        assert!(list["inputSchema"]["properties"].get("project").is_none());
        assert!(list["inputSchema"]["required"]
            .as_array()
            .is_some_and(Vec::is_empty));

        let get = tools.iter().find(|tool| tool["name"] == GET.name).unwrap();
        assert!(get["inputSchema"]["properties"].get("project").is_none());
        assert_eq!(get["inputSchema"]["required"], json!(["agent"]));
        assert_eq!(GET.policy.kind, ToolKind::Read);
    }
}
