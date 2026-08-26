use std::path::Path;
use std::time::UNIX_EPOCH;

use corpus_store::{fnv1a_hex, EntryAccess, MissionRunRef};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::confirmation::{confirm_and_run, mint_confirm};
use crate::error::{Error, Result};
use crate::tools::registry::{
    input_schema, parse_args, AuditPolicy, ConfirmationPolicy, RefreshPolicy, ToolCapability,
    ToolDefinition, ToolKind, ToolPolicy,
};
use crate::Ctx;

pub(crate) static DELETE: ToolDefinition = ToolDefinition {
    name: "entry_delete",
    description: "CONFIRM-GATED. Delete ONE entry from the project's corpus by relative path (findings/x.md, attacks/<slug>/, ...). Dry-run first; returns a one-shot token bound to the entry's current state. A directory needs recursive: true. runs/ is not deletable — technique cards cite those transcripts by name and the operator audits them.",
    input_schema: input_schema::<EntryDeleteArgs>,
    handler: entry_delete,
    policy: ToolPolicy {
        capability: ToolCapability::Admin,
        kind: ToolKind::Destructive,
        confirmation: ConfirmationPolicy::Token,
        audit: AuditPolicy::Category("corpus"),
        refresh: RefreshPolicy::Area("corpus"),
    },
};

#[derive(Debug, Deserialize, JsonSchema, PartialEq, Eq)]
struct EntryDeleteArgs {
    project: String,
    /// relative path under the project corpus.
    path: String,
    /// required to remove a directory.
    #[serde(default)]
    recursive: bool,
    confirm_token: Option<String>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct EntryPreview {
    files: u64,
    dirs: u64,
    bytes: u64,
    fingerprint: String,
}

fn dry_run_summary(project: &str, path: &str, preview: &EntryPreview) -> String {
    let kind = if preview.dirs > 0 {
        "directory tree"
    } else {
        "file"
    };
    format!(
        "DRY RUN — would delete {kind} {project}/corpus/{path} ({} file(s), {} directory/directories, {} bytes)",
        preview.files, preview.dirs, preview.bytes
    )
}

fn entry_delete(
    ctx: &mut Ctx<'_>,
    value: &Value,
    _origin: Option<&MissionRunRef>,
) -> Result<String> {
    let args: EntryDeleteArgs = parse_args(DELETE.name, value)?;
    let resolved = ctx
        .store
        .resolve_corpus_entry(&args.project, &args.path, EntryAccess::Mutate)
        .map_err(|error| Error::Args(error.to_string()))?;
    let preview = entry_preview(&resolved).map_err(|error| {
        Error::Args(format!(
            "cannot inspect {}/corpus/{} before deletion: {error}",
            args.project, args.path
        ))
    })?;
    if preview.dirs > 0 && !args.recursive {
        return Err(Error::Args(format!(
            "{} is a directory — pass recursive to preview and remove it and everything under it",
            args.path
        )));
    }
    // Bind the token to both the requested deletion mode and a deterministic
    // snapshot of the target. If the entry changes after the dry-run, the
    // second call no longer matches and must be previewed again.
    let target = format!(
        "{}/corpus/{}|recursive={}|snapshot={}",
        args.project, args.path, args.recursive, preview.fingerprint
    );
    if let Some(token) = args.confirm_token {
        confirm_and_run(ctx, DELETE.name, &target, &token, |store| {
            let freed = store
                .delete_corpus_entry(&args.project, &args.path, args.recursive)
                .map_err(|error| Error::Args(error.to_string()))?;
            Ok(format!(
                "deleted {}/corpus/{} ({freed} bytes)",
                args.project, args.path
            ))
        })
    } else {
        mint_confirm(
            ctx,
            DELETE.name,
            &target,
            &dry_run_summary(&args.project, &args.path, &preview),
        )
    }
}

/// A stable state fingerprint for the short confirmation window. It includes
/// every relative name, type, size, and modification timestamp without reading
/// potentially large attack artifacts into memory.
fn entry_preview(root: &Path) -> std::io::Result<EntryPreview> {
    fn visit(
        path: &Path,
        rel: &Path,
        preview: &mut EntryPreview,
        records: &mut Vec<String>,
    ) -> std::io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let kind = if metadata.is_dir() {
            preview.dirs += 1;
            "dir"
        } else {
            preview.files += 1;
            preview.bytes = preview.bytes.saturating_add(metadata.len());
            if metadata.file_type().is_symlink() {
                "link"
            } else {
                "file"
            }
        };
        records.push(format!(
            "{kind}|{}|{}|{modified}",
            rel.display(),
            metadata.len()
        ));
        if metadata.is_dir() {
            let mut children = std::fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                visit(
                    &child.path(),
                    &rel.join(child.file_name()),
                    preview,
                    records,
                )?;
            }
        }
        Ok(())
    }

    let mut preview = EntryPreview::default();
    let mut records = Vec::new();
    visit(root, Path::new("."), &mut preview, &mut records)?;
    preview.fingerprint = fnv1a_hex(records.join("\n").as_bytes());
    Ok(preview)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn generated_contract_and_policy_type_recursive_confirmation() {
        let schema = DELETE.catalog_entry()["inputSchema"].clone();
        assert_eq!(schema["required"], json!(["project", "path"]));
        assert_eq!(schema["properties"]["recursive"]["type"], "boolean");
        assert!(schema["properties"].get("confirm_token").is_some());
        for bad in [
            json!({"project": "p", "path": "findings/f.md", "recursive": "yes"}),
            json!({"project": "p", "path": "findings/f.md", "confirm_token": 42}),
        ] {
            assert!(parse_args::<EntryDeleteArgs>(DELETE.name, &bad).is_err());
        }

        assert_eq!(DELETE.policy.kind, ToolKind::Destructive);
        assert_eq!(DELETE.policy.confirmation, ConfirmationPolicy::Token);
        assert_eq!(DELETE.policy.audit, AuditPolicy::Category("corpus"));
        assert_eq!(DELETE.policy.refresh, RefreshPolicy::Area("corpus"));
    }

    #[test]
    fn previews_are_sorted_counted_and_sensitive_to_target_changes() {
        let root =
            std::env::temp_dir().join(format!("corpus-admin-entry-preview-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("z.md"), "zz").unwrap();
        fs::write(root.join("nested/a.md"), "aaa").unwrap();

        let first = entry_preview(&root).unwrap();
        let repeated = entry_preview(&root).unwrap();
        assert_eq!(first, repeated);
        assert_eq!(first.files, 2);
        assert_eq!(first.dirs, 2);
        assert_eq!(first.bytes, 5);
        assert_eq!(
            dry_run_summary("p", "attacks/x", &first),
            "DRY RUN — would delete directory tree p/corpus/attacks/x (2 file(s), 2 directory/directories, 5 bytes)"
        );

        fs::write(root.join("z.md"), "changed-size").unwrap();
        let changed = entry_preview(&root).unwrap();
        assert_ne!(changed.fingerprint, first.fingerprint);
        assert_ne!(changed.bytes, first.bytes);
        let _ = fs::remove_dir_all(root);
    }
}
