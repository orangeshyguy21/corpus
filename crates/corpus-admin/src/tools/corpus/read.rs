use corpus_store::{EntryAccess, MissionRunRef, CATEGORIES};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

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

pub(crate) static STATS: ToolDefinition = ToolDefinition {
    name: "corpus_stats",
    description: "Count files + bytes in a project's corpus.",
    input_schema: input_schema::<CorpusStatsArgs>,
    handler: corpus_stats,
    policy: READ_POLICY,
};

pub(crate) static LIST: ToolDefinition = ToolDefinition {
    name: "corpus_list",
    description: "List entries in a corpus category (hypotheses | techniques | findings | probes | retro | runs).",
    input_schema: input_schema::<CorpusListArgs>,
    handler: corpus_list,
    policy: READ_POLICY,
};

pub(crate) static READ: ToolDefinition = ToolDefinition {
    name: "corpus_read",
    description: "Read a store entry's markdown body by relative path under the project's corpus (findings/..., probes/<slug>/probe.md, ...).",
    input_schema: input_schema::<CorpusReadArgs>,
    handler: corpus_read,
    policy: READ_POLICY,
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct CorpusStatsArgs {
    project: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct CorpusListArgs {
    project: String,
    category: String,
}

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct CorpusReadArgs {
    project: String,
    /// relative path under `store/projects/<project>/corpus/`.
    path: String,
}

fn corpus_stats(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: CorpusStatsArgs = parse_args(STATS.name, value)?;
    let stats = corpus_store::corpus_stats(ctx.store, &args.project)
        .map_err(|error| Error::Args(error.to_string()))?;
    // Mission logs are reported apart from the knowledge categories —
    // transcripts dominate the byte total and would hide the rest.
    Ok(format!(
        "corpus {}: {} files, {} bytes; mission logs: {} files, {} bytes",
        args.project,
        stats.knowledge_files(),
        stats.knowledge_bytes(),
        stats.logs.files,
        stats.logs.bytes
    ))
}

fn corpus_list(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: CorpusListArgs = parse_args(LIST.name, value)?;
    if !CATEGORIES.contains(&args.category.as_str()) {
        return Err(Error::Args(format!(
            "unknown category {:?}; categories: {}",
            args.category,
            CATEGORIES.join(", ")
        )));
    }
    let dir = ctx
        .store
        .project_corpus_dir(&args.project)
        .join(&args.category);
    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|error| {
            Error::Args(format!(
                "corpus {}/{}: {error}",
                args.project, args.category
            ))
        })?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    Ok(if entries.is_empty() {
        format!("(empty) {}/{}/", args.project, args.category)
    } else {
        format!(
            "{}/{}/:\n{}",
            args.project,
            args.category,
            entries.join("\n")
        )
    })
}

fn corpus_read(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: CorpusReadArgs = parse_args(READ.name, value)?;
    // The shared guard, rather than a second inline one. The version that
    // lived here compared a canonical path against a possibly-NON-canonical
    // root, so it refused legal paths whenever the store sat behind a
    // symlink — which it does whenever a run dir is involved.
    let resolved = ctx
        .store
        .resolve_corpus_entry(&args.project, &args.path, EntryAccess::Read)
        .map_err(|error| Error::Args(error.to_string()))?;
    std::fs::read_to_string(&resolved)
        .map_err(|error| Error::Args(format!("cannot read {}: {error}", resolved.display())))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn generated_schemas_and_deserializers_share_corpus_read_contracts() {
        assert_eq!(
            STATS.catalog_entry()["inputSchema"]["required"],
            json!(["project"])
        );
        assert_eq!(
            LIST.catalog_entry()["inputSchema"]["required"],
            json!(["project", "category"])
        );
        assert_eq!(
            READ.catalog_entry()["inputSchema"]["required"],
            json!(["project", "path"])
        );
        assert!(
            READ.catalog_entry()["inputSchema"]["properties"]["path"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("relative"))
        );

        assert!(parse_args::<CorpusStatsArgs>(STATS.name, &json!({"project": 42})).is_err());
        assert!(parse_args::<CorpusListArgs>(
            LIST.name,
            &json!({"project": "p", "category": false})
        )
        .is_err());
        assert!(
            parse_args::<CorpusReadArgs>(READ.name, &json!({"project": "p", "path": 42})).is_err()
        );
        assert!(parse_args::<CorpusStatsArgs>(
            STATS.name,
            &json!({"project": "p", "future_field": true})
        )
        .is_ok());
    }

    #[test]
    fn corpus_reads_are_immediate_unconfirmed_and_side_effect_free() {
        for tool in [&STATS, &LIST, &READ] {
            assert_eq!(tool.policy.kind, ToolKind::Read);
            assert_eq!(tool.policy.confirmation, ConfirmationPolicy::None);
            assert_eq!(tool.policy.audit, AuditPolicy::None);
            assert_eq!(tool.policy.refresh, RefreshPolicy::None);
        }
    }

    #[test]
    fn dispatch_preserves_sorted_listing_exact_reads_and_category_validation() {
        let root =
            std::env::temp_dir().join(format!("corpus-admin-read-tools-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = corpus_store::Store::new(root.join("store"));
        store
            .create_project("p", "P", "cdk-regtest")
            .expect("project fixture");
        store
            .write_corpus_entry("p", "findings/z.md", "z body\n")
            .expect("z fixture");
        store
            .write_corpus_entry("p", "findings/a.md", "a body\n")
            .expect("a fixture");
        let mut pending_confirms = HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };

        assert_eq!(
            crate::dispatch(
                &mut ctx,
                LIST.name,
                &json!({"project": "p", "category": "findings"})
            )
            .unwrap(),
            "p/findings/:\na.md\nz.md"
        );
        assert_eq!(
            crate::dispatch(
                &mut ctx,
                READ.name,
                &json!({"project": "p", "path": "findings/a.md"})
            )
            .unwrap(),
            "a body\n"
        );
        let stats = crate::dispatch(&mut ctx, STATS.name, &json!({"project": "p"})).unwrap();
        assert!(stats.starts_with("corpus p: "), "{stats}");
        assert!(stats.contains("mission logs: 0 files, 0 bytes"), "{stats}");

        let error = crate::dispatch(
            &mut ctx,
            LIST.name,
            &json!({"project": "p", "category": "unknown"}),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("unknown category \"unknown\""), "{error}");
        assert!(error.contains(&CATEGORIES.join(", ")), "{error}");

        let _ = fs::remove_dir_all(root);
    }
}
