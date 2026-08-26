use corpus_store::MissionRunRef;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

pub(crate) static MOVE: ToolDefinition = ToolDefinition {
    name: "entry_move",
    description: "Move or rename ONE entry within the project's corpus — the tool for reorganising it. Both paths are relative and stay inside the same corpus; missing destination directories are created. Refuses an existing destination unless overwrite: true. runs/ is not movable.",
    input_schema: input_schema::<EntryMoveArgs>,
    handler: entry_move,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Write,
        confirmation: ConfirmationPolicy::None,
        audit: AuditPolicy::Category("corpus"),
        refresh: RefreshPolicy::Area("corpus"),
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct EntryMoveArgs {
    project: String,
    from: String,
    to: String,
    /// replace an existing destination.
    #[serde(default)]
    overwrite: bool,
}

fn entry_move(ctx: &mut Ctx<'_>, value: &Value, _origin: Option<&MissionRunRef>) -> Result<String> {
    let args: EntryMoveArgs = parse_args(MOVE.name, value)?;
    ctx.store
        .move_corpus_entry(&args.project, &args.from, &args.to, args.overwrite)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "moved {}/corpus/{} -> {}",
        args.project, args.from, args.to
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn generated_contract_and_policy_type_overwrite() {
        let schema = MOVE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "from", "to"]));
        assert_eq!(schema["properties"]["overwrite"]["type"], "boolean");
        assert!(parse_args::<EntryMoveArgs>(
            MOVE.name,
            &json!({
                "project": "p",
                "from": "findings/a.md",
                "to": "findings/b.md",
                "overwrite": "yes"
            })
        )
        .is_err());
        assert!(parse_args::<EntryMoveArgs>(
            MOVE.name,
            &json!({
                "project": "p",
                "from": "findings/a.md",
                "to": "findings/b.md",
                "future_field": true
            })
        )
        .is_ok());

        assert_eq!(MOVE.policy.kind, ToolKind::Write);
        assert_eq!(MOVE.policy.confirmation, ConfirmationPolicy::None);
        assert_eq!(MOVE.policy.audit, AuditPolicy::Category("corpus"));
        assert_eq!(MOVE.policy.refresh, RefreshPolicy::Area("corpus"));
    }

    #[test]
    fn dispatch_preserves_parent_creation_output_and_explicit_overwrite() {
        let root =
            std::env::temp_dir().join(format!("corpus-admin-entry-move-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = corpus_store::Store::new(root.join("store"));
        store
            .create_project("p", "P", "cdk-regtest")
            .expect("project fixture");
        store
            .write_corpus_entry("p", "findings/a.md", "first\n")
            .expect("source fixture");
        let mut pending_confirms = HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };

        let moved = crate::dispatch(
            &mut ctx,
            MOVE.name,
            &json!({
                "project": "p",
                "from": "findings/a.md",
                "to": "techniques/nested/moved.md"
            }),
        )
        .unwrap();
        assert_eq!(
            moved,
            "moved p/corpus/findings/a.md -> techniques/nested/moved.md"
        );
        assert_eq!(
            fs::read_to_string(
                store
                    .project_corpus_dir("p")
                    .join("techniques/nested/moved.md")
            )
            .unwrap(),
            "first\n"
        );

        store
            .write_corpus_entry("p", "findings/replacement.md", "new\n")
            .unwrap();
        store
            .write_corpus_entry("p", "techniques/existing.md", "old\n")
            .unwrap();
        let args = json!({
            "project": "p",
            "from": "findings/replacement.md",
            "to": "techniques/existing.md"
        });
        let error = crate::dispatch(&mut ctx, MOVE.name, &args)
            .unwrap_err()
            .to_string();
        assert!(error.contains("already exists"), "{error}");
        crate::dispatch(
            &mut ctx,
            MOVE.name,
            &json!({
                "project": "p",
                "from": "findings/replacement.md",
                "to": "techniques/existing.md",
                "overwrite": true
            }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(store.project_corpus_dir("p").join("techniques/existing.md"))
                .unwrap(),
            "new\n"
        );
        assert!(!store
            .project_corpus_dir("p")
            .join("findings/replacement.md")
            .exists());
        let _ = fs::remove_dir_all(root);
    }
}
