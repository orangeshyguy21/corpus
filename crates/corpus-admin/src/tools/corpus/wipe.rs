use corpus_store::MissionRunRef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::common::load_project;
use crate::confirmation::{confirm_and_run, mint_confirm};
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

pub(crate) static WIPE: ToolDefinition = ToolDefinition {
    name: "corpus_wipe",
    description: "CONFIRM-GATED. Wipe a project's corpus (working tree + generation bump; project and agents survive). Dry-run first; returns a one-shot token to complete.",
    input_schema: input_schema::<CorpusWipeArgs>,
    handler: corpus_wipe,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Destructive,
        confirmation: ConfirmationPolicy::Token,
        audit: AuditPolicy::Category("corpus"),
        refresh: RefreshPolicy::Area("corpus"),
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct CorpusWipeArgs {
    project: String,
    confirm_token: Option<String>,
}

fn dry_run_summary(project: &str, files: u64, bytes: u64, next_generation: u64) -> String {
    format!(
        "DRY RUN — would wipe the corpus of project {project} ({files} files, {bytes} bytes, generation -> {next_generation}); project and its agents survive"
    )
}

fn corpus_wipe(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: CorpusWipeArgs = parse_args(WIPE.name, value)?;
    if let Some(token) = args.confirm_token {
        confirm_and_run(ctx, WIPE.name, &args.project, &token, |store| {
            let project = store
                .wipe_project_corpus(&args.project)
                .map_err(|error| Error::Args(error.to_string()))?;
            Ok(format!(
                "wiped project corpus {} (generation -> {})",
                args.project, project.corpus_generation
            ))
        })
    } else {
        let stats = corpus_store::corpus_stats(ctx.store, &args.project)
            .map_err(|error| Error::Args(error.to_string()))?;
        let project = load_project(ctx.store, &args.project)?;
        let summary = dry_run_summary(
            &args.project,
            stats.files,
            stats.bytes,
            project.corpus_generation + 1,
        );
        mint_confirm(ctx, WIPE.name, &args.project, &summary)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use corpus_store::{AgentRole, Project};
    use serde_json::json;

    use super::*;

    #[test]
    fn generated_contract_and_policy_require_typed_confirmation() {
        let schema = WIPE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project"]));
        assert_eq!(
            schema["properties"]["confirm_token"]["type"],
            json!(["string", "null"])
        );
        assert!(parse_args::<CorpusWipeArgs>(
            WIPE.name,
            &json!({"project": "p", "confirm_token": 42})
        )
        .is_err());

        assert_eq!(WIPE.policy.kind, ToolKind::Destructive);
        assert_eq!(WIPE.policy.confirmation, ConfirmationPolicy::Token);
        assert_eq!(WIPE.policy.audit, AuditPolicy::Category("corpus"));
        assert_eq!(WIPE.policy.refresh, RefreshPolicy::Area("corpus"));
    }

    #[test]
    fn dry_run_names_scope_size_generation_and_survivors() {
        assert_eq!(
            dry_run_summary("p", 3, 42, 8),
            "DRY RUN — would wipe the corpus of project p (3 files, 42 bytes, generation -> 8); project and its agents survive"
        );
    }

    #[test]
    fn confirmed_wipe_advances_generation_while_project_and_agents_survive() {
        let root =
            std::env::temp_dir().join(format!("corpus-admin-wipe-tool-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = corpus_store::Store::new(root.join("store"));
        store
            .create_project("p", "P", "cdk-regtest")
            .expect("project fixture");
        store
            .create_agent_with_role("p", "keeper", AgentRole::Researcher)
            .expect("agent fixture");
        store
            .write_corpus_entry("p", "findings/evidence.md", "evidence\n")
            .expect("corpus fixture");
        let mut pending_confirms = HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };

        let dry = crate::dispatch(&mut ctx, WIPE.name, &json!({"project": "p"})).unwrap();
        assert!(dry.contains("1 files, 9 bytes, generation -> 1"), "{dry}");
        let token = dry
            .split("confirm_token: ")
            .nth(1)
            .expect("dry run token")
            .split_whitespace()
            .next()
            .unwrap();
        let output = crate::dispatch(
            &mut ctx,
            WIPE.name,
            &json!({"project": "p", "confirm_token": token}),
        )
        .unwrap();
        assert!(output.contains("generation -> 1"), "{output}");
        assert!(!store
            .project_corpus_dir("p")
            .join("findings/evidence.md")
            .exists());
        assert_eq!(Project::load(&store, "p").unwrap().corpus_generation, 1);
        assert!(store.load_agent("p", "keeper").is_ok());

        let _ = fs::remove_dir_all(root);
    }
}
