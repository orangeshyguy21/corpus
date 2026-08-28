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

pub(crate) static WRITE: ToolDefinition = ToolDefinition {
    name: "entry_write",
    description: "Write (create or replace in place) ONE entry in the proven project's corpus by relative path (findings/x.md, notes/recon.md, ...). Choose any category or nested path that best represents the data. Missing parent directories are created. Absolute paths, traversal, symlink escapes, retired attacks/, and immutable runs/ are refused. Prefer this over raw file tools: it needs no knowledge of where the corpus lives on disk, and every write is recorded in the audit log.",
    input_schema: input_schema::<EntryWriteArgs>,
    handler: entry_write,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Write,
        confirmation: ConfirmationPolicy::None,
        audit: AuditPolicy::Category("corpus"),
        refresh: RefreshPolicy::Area("corpus"),
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct EntryWriteArgs {
    project: String,
    /// relative path under the project corpus, e.g. notes/recon.md.
    path: String,
    /// the full entry body to write — replaces any existing content at this path.
    content: String,
}

fn entry_write(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: EntryWriteArgs = parse_args(WRITE.name, value)?;
    let bytes = ctx
        .store
        .write_corpus_entry(&args.project, &args.path, &args.content)
        .map_err(|error| Error::Args(error.to_string()))?;
    Ok(format!(
        "wrote {}/corpus/{} ({bytes} bytes)",
        args.project, args.path
    ))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn generated_contract_and_policy_type_all_required_fields() {
        let schema = WRITE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "path", "content"]));
        for bad in [
            json!({"project": 7, "path": "findings/a.md", "content": "body"}),
            json!({"project": "p", "path": false, "content": "body"}),
            json!({"project": "p", "path": "findings/a.md", "content": 7}),
        ] {
            assert!(parse_args::<EntryWriteArgs>(WRITE.name, &bad).is_err());
        }
        assert!(parse_args::<EntryWriteArgs>(
            WRITE.name,
            &json!({
                "project": "p",
                "path": "findings/a.md",
                "content": "body",
                "future_field": true
            })
        )
        .is_ok());

        assert_eq!(WRITE.policy.kind, ToolKind::Write);
        assert_eq!(WRITE.policy.confirmation, ConfirmationPolicy::None);
        assert_eq!(WRITE.policy.audit, AuditPolicy::Category("corpus"));
        assert_eq!(WRITE.policy.refresh, RefreshPolicy::Area("corpus"));
    }

    #[test]
    fn dispatch_preserves_parent_creation_replacement_and_byte_output() {
        let root =
            std::env::temp_dir().join(format!("corpus-admin-entry-write-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let store = corpus_store::Store::new(root.join("store"));
        store
            .create_project("p", "P", "cdk-regtest")
            .expect("project fixture");
        let mut pending_confirms = HashMap::new();
        let mut ctx = Ctx {
            store: &store,
            pending_confirms: &mut pending_confirms,
        };

        let written = crate::dispatch(
            &mut ctx,
            WRITE.name,
            &json!({
                "project": "p",
                "path": "techniques/nested/plan.md",
                "content": "café\n"
            }),
        )
        .unwrap();
        assert_eq!(
            written,
            "wrote p/corpus/techniques/nested/plan.md (6 bytes)"
        );
        let path = store
            .project_corpus_dir("p")
            .join("techniques/nested/plan.md");
        assert_eq!(fs::read_to_string(&path).unwrap(), "café\n");

        let replaced = crate::dispatch(
            &mut ctx,
            WRITE.name,
            &json!({
                "project": "p",
                "path": "techniques/nested/plan.md",
                "content": "v2\n"
            }),
        )
        .unwrap();
        assert_eq!(
            replaced,
            "wrote p/corpus/techniques/nested/plan.md (3 bytes)"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "v2\n");

        crate::dispatch(
            &mut ctx,
            WRITE.name,
            &json!({
                "project": "p",
                "path": "notes/recon/evidence.md",
                "content": "custom category\n"
            }),
        )
        .expect("custom categories are valid organizational choices");
        assert_eq!(
            fs::read_to_string(
                store
                    .project_corpus_dir("p")
                    .join("notes/recon/evidence.md")
            )
            .unwrap(),
            "custom category\n"
        );
        let _ = fs::remove_dir_all(root);
    }
}
