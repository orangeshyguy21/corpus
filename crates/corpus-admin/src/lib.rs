//! corpus-admin MCP profile: natural-language administration of the corpus
//! store (projects, agents, missions, corpus lifecycle, read-only queries).
//!
//! This library backs the dedicated host-side `corpus-admin-mcp` artifact. It
//! sits OUTSIDE
//! the research trust domains (no sandbox, no targets, no oracles) and never
//! runs missions — it prepares them. Every tool is a thin wrapper over the
//! corpus-store and corpus-observe APIs; nothing here touches the plugin protocol or the
//! filesystem outside the store. Destructive handlers are operator-facing:
//! project-scoped curators receive none of them.
//!
//! The research MCP reaches a project-scoped subset through a narrow adapter;
//! it cannot enable the host-global wire profile.

#![recursion_limit = "256"]

use std::collections::HashMap;

use corpus_store::{MissionRunRef, Store};
use serde_json::Value;

mod common;
mod confirmation;
pub mod error;
mod tools;

pub use confirmation::PendingConfirm;
use error::{Error, Result};

/// The narrow mutable state required by admin handlers. Callers lend their
/// store and confirmation map; no plugin/runtime state crosses this boundary.
pub struct Ctx<'a> {
    pub store: &'a Store,
    pub pending_confirms: &'a mut HashMap<String, PendingConfirm>,
}

/// Owning state for the dedicated host-side admin server.
pub struct State {
    pub store: Store,
    pending_confirms: HashMap<String, PendingConfirm>,
}

impl State {
    pub fn from_env() -> Self {
        Self {
            store: Store::from_env(),
            pending_confirms: HashMap::new(),
        }
    }

    pub fn context(&mut self) -> Ctx<'_> {
        Ctx {
            store: &self.store,
            pending_confirms: &mut self.pending_confirms,
        }
    }
}

/// Every destructive op. All require the confirm-token ritual in the operator
/// profile; project-scoped curators receive none of them.
pub const DESTRUCTIVE_OPS: [&str; 5] = [
    "project_delete",
    "agent_delete",
    "mission_delete",
    "corpus_wipe",
    "entry_delete",
];

/// Every tool name this group serves. The catalog is asserted against it,
/// so a tool added to one and not the other fails a test rather than going
/// quietly unroutable (or unadvertised).
pub const ADMIN_TOOLS: [&str; 35] = [
    "project_list",
    "project_new",
    "project_clone",
    "project_delete",
    "project_rebind",
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_copy",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "agent_delete",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_await",
    "mission_new",
    "mission_delete",
    "mission_launch",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_wipe",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "entry_delete",
    "entry_move",
    "entry_write",
    "model_list",
];

/// The catalog filtered to `allowed`, with the `project` argument stripped
/// from every schema.
///
/// The project is not the caller's to choose — a scoped server takes it
/// from `CORPUS_PROJECT` — so advertising the field would invite a model to
/// name one and then have it silently overwritten. A tool that quietly
/// ignores an argument it asked for is worse than one that never asked.
pub fn scoped_catalog(allowed: &[&str]) -> Value {
    let mut out = catalog();
    if let Some(list) = out.as_array_mut() {
        list.retain(|tool| {
            tool.get("name")
                .and_then(Value::as_str)
                .is_some_and(|n| allowed.contains(&n))
        });
        for tool in list.iter_mut() {
            let Some(schema) = tool.get_mut("inputSchema") else {
                continue;
            };
            if let Some(props) = schema.get_mut("properties").and_then(Value::as_object_mut) {
                props.remove("project");
            }
            if let Some(required) = schema.get_mut("required").and_then(Value::as_array_mut) {
                required.retain(|k| k.as_str() != Some("project"));
            }
        }
    }
    out
}

/// The admin tool catalog advertised in tools/list (admin profile only).
pub fn catalog() -> Value {
    let mut catalog: Vec<Value> = tools::registry::catalog_entries().collect();
    catalog.sort_by_key(|tool| {
        tool["name"]
            .as_str()
            .and_then(|name| ADMIN_TOOLS.iter().position(|known| known == &name))
            .unwrap_or(usize::MAX)
    });
    Value::Array(catalog)
}

/// Dispatch a corpus-admin tools/call. Rejects any tool not in this group —
/// the admin profile carries NO sandbox/oracle/faucet tools by construction.
pub fn dispatch(ctx: &mut Ctx, name: &str, args: &Value) -> Result<String> {
    dispatch_with_origin(ctx, name, args, None)
}

/// Scoped-agent entry point. `origin` is launcher-proven context supplied by
/// corpus-mcp, never a model argument. The host admin profile calls [`dispatch`]
/// and therefore cannot invent a Curator return address.
pub fn dispatch_with_origin(
    ctx: &mut Ctx,
    name: &str,
    args: &Value,
    origin: Option<&MissionRunRef>,
) -> Result<String> {
    let definition = tools::registry::definition(name)
        .ok_or_else(|| Error::Args(format!("unknown admin tool: {name}")))?;
    validate_project_arguments(name, args)?;
    definition.invoke(ctx, args, origin)
}

/// Reject project identities before any handler can turn them into paths.
///
/// JSON Schema is a client contract, not an authorization boundary. The host
/// admin server receives model-authored values, so all project-bearing fields
/// are checked again at the single dispatch door. Scoped research calls are
/// covered too, after their launcher-proven project has been injected.
fn validate_project_arguments(name: &str, args: &Value) -> Result<()> {
    let project_keys: &[&str] = match name {
        "project_new" | "project_delete" | "project_rebind" => &["slug"],
        "project_clone" => &["from", "to"],
        "agent_copy" => &["from_project", "to_project"],
        _ => &["project"],
    };
    let Some(arguments) = args.as_object() else {
        return Ok(()); // the typed handler reports the malformed object
    };
    for key in project_keys {
        let Some(value) = arguments.get(*key) else {
            continue; // the typed handler reports a missing required field
        };
        let Some(project) = value.as_str() else {
            continue; // the typed handler reports the field's type error
        };
        corpus_store::validate_slug(project)
            .map_err(|error| Error::Args(format!("{name} argument {key}: {error}")))?;
    }
    Ok(())
}
