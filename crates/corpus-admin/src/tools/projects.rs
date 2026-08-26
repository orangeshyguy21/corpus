//! Typed project administration tools.

use corpus_store::{MissionRunRef, Project};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::confirmation::{confirm_and_run, mint_confirm};
use crate::error::{Error, Result};
use crate::Ctx;

const READ_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Read,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::None,
    refresh: RefreshPolicy::None,
};

const WRITE_POLICY: ToolPolicy = ToolPolicy {
    capability: ToolCapability::Admin,
    kind: ToolKind::Write,
    confirmation: ConfirmationPolicy::None,
    audit: AuditPolicy::Category("projects"),
    refresh: RefreshPolicy::Area("projects"),
};

pub(crate) static LIST: ToolDefinition = ToolDefinition {
    name: "project_list",
    description: "List projects (slug, name, plugin binding, generation).",
    input_schema: input_schema::<ProjectListArgs>,
    handler: project_list,
    policy: READ_POLICY,
};

pub(crate) static NEW: ToolDefinition = ToolDefinition {
    name: "project_new",
    description: "Create an empty project. Add agents explicitly by role.",
    input_schema: input_schema::<ProjectNewArgs>,
    handler: project_new,
    policy: WRITE_POLICY,
};

pub(crate) static CLONE: ToolDefinition = ToolDefinition {
    name: "project_clone",
    description: "Clone a project (config + agents + missions; corpus is opt-in).",
    input_schema: input_schema::<ProjectCloneArgs>,
    handler: project_clone,
    policy: WRITE_POLICY,
};

pub(crate) static DELETE: ToolDefinition = ToolDefinition {
    name: "project_delete",
    description: "CONFIRM-GATED. Delete a project (whole subtree), lifecycle-tearing down contained missions first when needed. Dry-run first; returns a one-shot token to complete.",
    input_schema: input_schema::<ProjectDeleteArgs>,
    handler: project_delete,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Destructive,
        confirmation: ConfirmationPolicy::Token,
        audit: AuditPolicy::Category("projects"),
        refresh: RefreshPolicy::Area("projects"),
    },
};

pub(crate) static REBIND: ToolDefinition = ToolDefinition {
    name: "project_rebind",
    description:
        "Rebind a project to an environment plugin. The plugin must exist in the registry.",
    input_schema: input_schema::<ProjectRebindArgs>,
    handler: project_rebind,
    policy: WRITE_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct ProjectListArgs {}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct ProjectNewArgs {
    slug: String,
    name: Option<String>,
    /// Environment plugin; defaults to `cdk-regtest`.
    plugin: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct ProjectCloneArgs {
    from: String,
    to: String,
    name: Option<String>,
    #[serde(default)]
    with_corpus: bool,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct ProjectDeleteArgs {
    slug: String,
    confirm_token: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct ProjectRebindArgs {
    slug: String,
    plugin: String,
}

fn project_list(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let _: ProjectListArgs = parse_args(LIST.name, value)?;
    let projects = ctx
        .store
        .list_projects()
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(projects
        .iter()
        .map(|(slug, project)| {
            format!(
                "{slug} \"{name}\" plugin={} gen={}{}",
                project.plugin,
                project.corpus_generation,
                project
                    .cloned_from
                    .as_deref()
                    .map(|from| format!(" cloned-from={from}"))
                    .unwrap_or_default(),
                name = if project.name.is_empty() {
                    slug
                } else {
                    &project.name
                },
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn project_new(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: ProjectNewArgs = parse_args(NEW.name, value)?;
    let name = args.name.as_deref().unwrap_or(&args.slug);
    let plugin = args.plugin.as_deref().unwrap_or("cdk-regtest");
    let project = ctx
        .store
        .create_project(&args.slug, name, plugin)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "created project {} (plugin: {})",
        args.slug, project.plugin
    ))
}

fn project_clone(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: ProjectCloneArgs = parse_args(CLONE.name, value)?;
    ctx.store
        .clone_project(&args.from, &args.to, args.name.as_deref(), args.with_corpus)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!("cloned project {} -> {}", args.from, args.to))
}

fn project_delete(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: ProjectDeleteArgs = parse_args(DELETE.name, value)?;
    if let Some(token) = args.confirm_token {
        confirm_and_run(ctx, DELETE.name, &args.slug, &token, |store| {
            let missions = store
                .list_missions(&args.slug)
                .map_err(|error| Error::Args(error.to_string()))?;
            if missions
                .iter()
                .any(|(mission, _)| store.ensure_mission_deletable(&args.slug, mission).is_err())
            {
                store
                    .request_project_delete(&args.slug)
                    .map_err(|error| Error::Args(error.to_string()))?;
                Ok(format!(
                    "requested deletion of project {}; the app will tear down its missions first",
                    args.slug
                ))
            } else {
                store
                    .delete_project(&args.slug)
                    .map_err(|error| Error::Args(error.to_string()))?;
                Ok(format!("deleted project {}", args.slug))
            }
        })
    } else {
        let project =
            Project::load(ctx.store, &args.slug).map_err(|error| Error::Args(error.to_string()))?;
        let stats = corpus_store::corpus_stats(ctx.store, &args.slug).unwrap_or_default();
        let agents = ctx
            .store
            .list_agents(&args.slug)
            .map(|agents| agents.len())
            .unwrap_or(0);
        let missions = ctx
            .store
            .list_missions(&args.slug)
            .map(|missions| missions.len())
            .unwrap_or(0);
        mint_confirm(
            ctx,
            DELETE.name,
            &args.slug,
            &format!(
                "DRY RUN — would delete project {} (plugin {}, gen {}, agents {}, missions {}, corpus files {})",
                args.slug,
                project.plugin,
                project.corpus_generation,
                agents,
                missions,
                stats.files
            ),
        )
    }
}

fn project_rebind(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: ProjectRebindArgs = parse_args(REBIND.name, value)?;
    let known = corpus_observe::plugin_names().map_err(|error| Error::Args(error.to_string()))?;
    if !known.iter().any(|name| name == &args.plugin) {
        return Err(Error::Args(format!(
            "unknown plugin {:?} — not in the registry; known plugins:\n{}",
            args.plugin,
            known
                .iter()
                .map(|name| format!("  {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }
    let project = ctx
        .store
        .rebind_project(&args.slug, &args.plugin)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "rebound project {} -> plugin {} (gen {})",
        args.slug, args.plugin, project.corpus_generation
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_schemas_and_deserializers_share_project_contracts() {
        let new_schema = NEW.catalog_entry()["inputSchema"].clone();
        assert_eq!(new_schema["type"], "object");
        assert_eq!(new_schema["properties"]["slug"]["type"], "string");
        assert!(new_schema["required"]
            .as_array()
            .is_some_and(|required| required == &[json!("slug")]));

        let clone: ProjectCloneArgs = parse_args(
            CLONE.name,
            &json!({
                "from": "source",
                "to": "copy",
                "future_optional_field": true
            }),
        )
        .unwrap();
        assert!(!clone.with_corpus);
        assert!(parse_args::<ProjectCloneArgs>(
            CLONE.name,
            &json!({
                "from": "source",
                "to": "copy",
                "with_corpus": "yes"
            })
        )
        .is_err());
        assert!(parse_args::<ProjectDeleteArgs>(
            DELETE.name,
            &json!({
                "slug": "project",
                "confirm_token": 42
            })
        )
        .is_err());
        assert!(parse_args::<ProjectListArgs>(LIST.name, &json!({"future_field": true})).is_ok());
    }

    #[test]
    fn project_policies_distinguish_reads_writes_and_destruction() {
        assert_eq!(LIST.policy.kind, ToolKind::Read);
        for tool in [&NEW, &CLONE, &REBIND] {
            assert_eq!(tool.policy.kind, ToolKind::Write);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::Category("projects"));
            assert_eq!(tool.policy.refresh, RefreshPolicy::Area("projects"));
        }
        assert_eq!(DELETE.policy.kind, ToolKind::Destructive);
        assert_eq!(DELETE.policy.confirmation, ConfirmationPolicy::Token);
        assert_eq!(DELETE.policy.audit, AuditPolicy::Category("projects"));
        assert_eq!(DELETE.policy.refresh, RefreshPolicy::Area("projects"));
    }
}
