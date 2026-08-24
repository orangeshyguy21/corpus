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

use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use corpus_observe::{MissionActivity, MissionRunState};
use corpus_store::{
    fnv1a_hex, AgentRole, EntryAccess, FindingQuery, FindingSeverity, FindingSort, Mission,
    MissionDeleteRequest, MissionLaunchRequest, MissionRunRef, Project, Store, CATEGORIES,
};
use serde_json::{json, Value};

pub mod error;

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
        Self { store: Store::from_env(), pending_confirms: HashMap::new() }
    }

    pub fn context(&mut self) -> Ctx<'_> {
        Ctx { store: &self.store, pending_confirms: &mut self.pending_confirms }
    }
}

/// A pending destructive-op confirmation: a single-use, short-TTL token
/// minted by a dry-run call and consumed by the token-bearing re-call.
#[derive(Debug)]
pub struct PendingConfirm {
    pub op: String,
    pub target: String,
    pub expires_at: u64,
}

/// Corpus walk for dry-run summaries (renamed to avoid shadowing the admin
/// `corpus_stats` tool handler).
use corpus_store::corpus_stats as walk_corpus_stats;

/// TTL for a confirm token: short, so an abandoned dry-run cannot be
/// replayed later to a now-stale target.
const CONFIRM_TTL_SECS: u64 = 60;

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
    json!([
        // --- projects ---
        {
            "name": "project_list",
            "description": "List projects (slug, name, plugin binding, generation).",
            "inputSchema": {"type": "object", "properties": {}, "required": []}
        },
        {
            "name": "project_new",
            "description": "Create an empty project. Add agents explicitly by role.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": {"type": "string"},
                    "name": {"type": "string"},
                    "plugin": {"type": "string", "description": "environment plugin, default cdk-regtest"}
                },
                "required": ["slug"]
            }
        },
        {
            "name": "project_clone",
            "description": "Clone a project (config + agents + missions; corpus is opt-in).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "name": {"type": "string"},
                    "with_corpus": {"type": "boolean"}
                },
                "required": ["from", "to"]
            }
        },
        {
            "name": "project_delete",
            "description": "CONFIRM-GATED. Delete a project (whole subtree). Dry-run first; returns a one-shot token to complete.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": {"type": "string"},
                    "confirm_token": {"type": "string"}
                },
                "required": ["slug"]
            }
        },
        {
            "name": "project_rebind",
            "description": "Rebind a project to an environment plugin. The plugin must exist in the registry.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "slug": {"type": "string"},
                    "plugin": {"type": "string"}
                },
                "required": ["slug", "plugin"]
            }
        },
        // --- agents ---
        {
            "name": "agent_list",
            "description": "List a project's agents (slug, name, config hash).",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}},
                "required": ["project"]
            }
        },
        {
            "name": "agent_get",
            "description": "Read an agent's opencode.json document (the config you edit).",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}, "agent": {"type": "string"}},
                "required": ["project", "agent"]
            }
        },
        {
            "name": "agent_new",
            "description": "Create a NEW agent from structured fields — the server builds the opencode.json (prefer this over agent_save for creation; agent_save only edits existing agents). Pass 'from' to inherit an existing agent's permissions/prompts (e.g. \"researcher\") with your description/prompt overlaid.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string", "description": "kebab-case slug; also the opencode agent name"},
                    "description": {"type": "string"},
                    "prompt": {"type": "string", "description": "the system prompt body"},
                    "model": {"type": "string", "description": "optional model id"},
                    "from": {"type": "string", "description": "optional existing agent to inherit permissions/prompts from"},
                    "role": {"type": "string", "enum": ["super", "curator", "tester", "researcher"], "description": "capability ceiling; defaults to researcher (or the inherited agent's role with `from`)"}
                },
                "required": ["project", "agent", "description", "prompt"]
            }
        },
        {
            "name": "agent_save",
            "description": "Validate and save an agent's opencode.json. The core validator runs first; an invalid document is refused with the validator's message.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "document": {"type": "object"}
                },
                "required": ["project", "agent", "document"]
            }
        },
        {
            "name": "agent_clone",
            "description": "Clone an agent (config + prompts + subagents) to a new slug WITHIN one project. To copy into a DIFFERENT project use agent_copy.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "from": {"type": "string"},
                    "to": {"type": "string"}
                },
                "required": ["project", "from", "to"]
            }
        },
        {
            "name": "agent_copy",
            "description": "Copy an agent BETWEEN projects (prompts, subagents and role included). This is the tool for 'copy these agents into that project' — agent_clone cannot cross a project boundary.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "from_project": {"type": "string"},
                    "from": {"type": "string"},
                    "to_project": {"type": "string"},
                    "to": {"type": "string", "description": "destination slug; defaults to the source slug"}
                },
                "required": ["from_project", "from", "to_project"]
            }
        },
        {
            "name": "agent_set",
            "description": "Set ONE field of an agent (or of one of its subagents) without resending the whole document: model, description, prompt, or temperature. Prefer this over agent_save for a single change. Pass null to clear a field.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "subagent": {"type": "string", "description": "target this subagent entry instead of the primary"},
                    "field": {"type": "string", "enum": ["model", "description", "prompt", "temperature"]},
                    "value": {"description": "the new value; null clears the field"}
                },
                "required": ["project", "agent", "field", "value"]
            }
        },
        {
            "name": "agent_set_role",
            "description": "Set an agent's ROLE — the capability ceiling the corpus server enforces for missions launched as it. super = every current-project research, sandbox, corpus and management capability, including confirmation-gated corpus wipe; curator = scoped project management with agent/mission/entry deletion but no wipe, sandbox or internet; tester = sandbox/oracle/faucet/findings, no internet; researcher = read + technique_save + internet. Cross-project and project-lifecycle administration remain operator-only. A role also regenerates permissions at launch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "subagent": {"type": "string", "description": "set a subagent's role instead (capped by the primary's)"},
                    "role": {"type": "string", "enum": ["super", "curator", "tester", "researcher"]}
                },
                "required": ["project", "agent", "role"]
            }
        },
        {
            "name": "agent_set_permission",
            "description": "MERGE a permission patch into an agent (or subagent) entry — top-level keys replace, null removes, everything else is left alone. Note the role ceiling still wins: granting a corpus_* tool outside the agent's role has no effect at launch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "subagent": {"type": "string"},
                    "patch": {"type": "object", "description": "e.g. {\"webfetch\": \"allow\", \"bash\": null}"}
                },
                "required": ["project", "agent", "patch"]
            }
        },
        {
            "name": "agent_subagent_add",
            "description": "Add a subagent to an agent's document and wire the primary's task: permission to allow delegating to it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "name": {"type": "string", "description": "kebab-case entry name, unique across the PROJECT"},
                    "description": {"type": "string"},
                    "prompt": {"type": "string"},
                    "model": {"type": "string"},
                    "role": {"type": "string", "enum": ["super", "curator", "tester", "researcher"]}
                },
                "required": ["project", "agent", "name", "description", "prompt"]
            }
        },
        {
            "name": "agent_subagent_remove",
            "description": "Remove a subagent entry, its delegation rule, and its role.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "name": {"type": "string"}
                },
                "required": ["project", "agent", "name"]
            }
        },
        {
            "name": "agent_delete",
            "description": "CONFIRM-GATED. Delete an agent and every mission assigned to it. Dry-run first; returns a one-shot token and lists the missions that will also be deleted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "agent": {"type": "string"},
                    "confirm_token": {"type": "string"}
                },
                "required": ["project", "agent"]
            }
        },
        // --- missions ---
        {
            "name": "mission_list",
            "description": "List a project's missions — per row: slug, agent, budget, and `live`. `live` only reports whether a launch session for the mission currently exists (yes) or not (no); a finished agent parked at its prompt STILL reads live=yes, so this is not a working/done signal. To tell whether an agent is actually running, waiting, or idle, use mission_status. For a mission's brief and pins, use mission_get.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}},
                "required": ["project"]
            }
        },
        {
            "name": "mission_get",
            "description": "Read one mission in full: its agent, budget, source pins, and brief body. The `live` line means only that a launch session exists — not that the agent is working. For run state (running / waiting / idle), use mission_status.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}, "mission": {"type": "string"}},
                "required": ["project", "mission"]
            }
        },
        {
            "name": "mission_status",
            "description": "Read ONE immediate snapshot of mission run state: 'running' (the agent is producing now), 'waiting · last active <dur>' (session live but parked), or 'idle' (nothing up). This — not the `live` flag from mission_list/mission_get — distinguishes work from a parked session. Omit 'mission' for every project mission, or name one. Do not call this in a polling loop. Agent roles should finish their turn after dispatch; `mission_await` remains only as a one-shot operator diagnostic.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"project": {"type": "string"}, "mission": {"type": "string", "description": "optional — one mission's status; omitted = all"}},
                    "required": ["project"]
                }
        },
        {
            "name": "mission_await",
            "description": "Operator diagnostic: block once (up to timeout_secs, default 45, max 90) until a launched mission changes, then return what changed. Do NOT call this repeatedly in one model turn; agent roles do not receive this tool because Corpus owns background supervision. Omit 'mission' to observe any mission on the project, or name one. While blocked, this MCP session cannot service another call.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}, "mission": {"type": "string", "description": "optional — one mission's status; omitted = all"}},
                "required": ["project"]
            }
        },
        {
            "name": "mission_new",
            "description": "Create a mission for an existing agent on the project. 'slug' is the mission's id (kebab-case); 'name' is the human display label shown in the app's mission nav — set it so the operator sees a real name, not a placeholder (defaults to the slug when omitted).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "slug": {"type": "string"},
                    "agent": {"type": "string"},
                    "brief": {"type": "string"},
                    "name": {"type": "string", "description": "operator-facing display name for the mission nav"},
                    "budget": {"type": "string"},
                    "pins": {"type": "object", "description": "optional per-source overrides; omitted sources inherit the project's selected revisions (or plugin defaults when the project has no explicit selection)"}
                },
                "required": ["project", "slug", "agent", "brief"]
            }
        },
        {
            "name": "mission_launch",
            "description": "Launch a mission: the app spawns a full opencode TUI session for it and kicks it off with the mission's brief as the prompt. The operator can watch and steer the session live in the app. Use this when a mission is ready to run — mission_new only writes the record; this starts it. The launch happens the moment the app picks up the request; a mission whose session is already live is left alone.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "mission": {"type": "string"}
                },
                "required": ["project", "mission"]
            }
        },
        {
            "name": "mission_delete",
            "description": "CONFIRM-GATED. Request mission deletion. The app tears down any run and plugin environment first, then removes the record; cleanup failures retain the mission for retry. Dry-run first; returns a one-shot token to complete.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "mission": {"type": "string"},
                    "confirm_token": {"type": "string"}
                },
                "required": ["project", "mission"]
            }
        },
        {
            "name": "mission_set_budget",
            "description": "Set a mission's execution budget (per-MISSION, never per-agent).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "mission": {"type": "string"},
                    "budget": {"type": "string"}
                },
                "required": ["project", "mission", "budget"]
            }
        },
        {
            "name": "mission_set_pins",
            "description": "Set a mission's source pins (repo -> rev map).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "mission": {"type": "string"},
                    "pins": {"type": "object"}
                },
                "required": ["project", "mission", "pins"]
            }
        },
        // --- corpus ---
        {
            "name": "corpus_wipe",
            "description": "CONFIRM-GATED. Wipe a project's corpus (working tree + generation bump; project and agents survive). Dry-run first; returns a one-shot token to complete.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "confirm_token": {"type": "string"}
                },
                "required": ["project"]
            }
        },
        {
            "name": "corpus_stats",
            "description": "Count files + bytes in a project's corpus.",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}},
                "required": ["project"]
            }
        },
        {
            "name": "corpus_list",
            "description": "List entries in a corpus category (hypotheses | techniques | findings | attacks | retro | runs).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "category": {"type": "string"}
                },
                "required": ["project", "category"]
            }
        },
        {
            "name": "corpus_read",
            "description": "Read a store entry's markdown body by relative path under the project's corpus (findings/..., attacks/<slug>/attack.md, ...).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "path": {"type": "string", "description": "relative path under store/projects/<p>/corpus/"}
                },
                "required": ["project", "path"]
            }
        },
        {
            "name": "finding_list",
            "description": "List recursively discovered findings as structured JSON. Missing or invalid severity remains visible as unrated with metadata warnings. Filters never read full Markdown bodies.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "severity": {
                        "description": "Optional severity or array of severities; omitted means every rated severity.",
                        "oneOf": [
                            {"type": "string", "enum": ["critical", "high", "medium", "low"]},
                            {"type": "array", "items": {"type": "string", "enum": ["critical", "high", "medium", "low"]}, "uniqueItems": true}
                        ]
                    },
                    "include_unrated": {"type": "boolean", "description": "Include findings with missing/invalid severity (default true)."},
                    "text": {"type": "string", "description": "Case-insensitive title, reference, and relative-path search."},
                    "sort": {"type": "string", "enum": ["newest", "severity"], "description": "Default newest."},
                    "limit": {"type": "integer", "minimum": 1}
                },
                "required": ["project"]
            }
        },
        {
            "name": "entry_delete",
            "description": "CONFIRM-GATED. Delete ONE entry from the project's corpus by relative path (findings/x.md, attacks/<slug>/, ...). Dry-run first; returns a one-shot token bound to the entry's current state. A directory needs recursive: true. runs/ is not deletable — technique cards cite those transcripts by name and the operator audits them.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "path": {"type": "string", "description": "relative path under the project corpus"},
                    "recursive": {"type": "boolean", "description": "required to remove a directory"},
                    "confirm_token": {"type": "string"}
                },
                "required": ["project", "path"]
            }
        },
        {
            "name": "entry_move",
            "description": "Move or rename ONE entry within the project's corpus — the tool for reorganising it. Both paths are relative and stay inside the same corpus; missing destination directories are created. Refuses an existing destination unless overwrite: true. runs/ is not movable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "overwrite": {"type": "boolean", "description": "replace an existing destination"}
                },
                "required": ["project", "from", "to"]
            }
        },
        {
            "name": "entry_write",
            "description": "Write (create or replace in place) ONE entry in the project's corpus by relative path (techniques/plan.md, findings/x.md, ...). The path is relative and stays inside the corpus — pass 'techniques/plan.md', never an absolute or cwd path. Missing parent directories are created. The first path segment must be a real corpus category (hypotheses, techniques, findings, attacks, retro). runs/ is not writable — those are mission transcripts. Prefer this over raw file tools: it needs no knowledge of where the corpus lives on disk, and every write is recorded in the audit log.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "path": {"type": "string", "description": "relative path under the project corpus, e.g. techniques/plan.md"},
                    "content": {"type": "string", "description": "the full entry body to write — replaces any existing content at this path"}
                },
                "required": ["project", "path", "content"]
            }
        },
        // --- models ---
        {
            "name": "model_list",
            "description": "Discover the models available to opencode launches: the exact provider/model id strings (with display names) that are valid in an agent config's model field or a launch arg, as reported by `opencode models --verbose`. Use 'filter' to narrow by substring (id or display name) instead of printing the whole catalog, and 'refresh' to bypass the TTL cache when the catalog changed. Always resolve an id through this list before writing it into an agent config — never guess one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filter": {"type": "string", "description": "optional case-insensitive substring matched against id and display name"},
                    "refresh": {"type": "boolean", "description": "bypass the TTL cache and re-pull opencode's catalog"}
                },
                "required": []
            }
        }
    ])
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
    let project = |args: &Value| -> Result<String> { require_str(args, "project") };
    match name {
        "project_list" => project_list(ctx),
        "project_new" => project_new(ctx, args),
        "project_clone" => project_clone(ctx, args),
        "project_delete" => project_delete(ctx, args),
        "project_rebind" => project_rebind(ctx, args),
        "agent_list" => agent_list(ctx, &project(args)?),
        "agent_get" => agent_get(ctx, &project(args)?, &require_str(args, "agent")?),
        "agent_save" => agent_save(ctx, args),
        "agent_new" => agent_new(ctx, args),
        "agent_clone" => agent_clone(ctx, args),
        "agent_copy" => agent_copy(ctx, args),
        "agent_set" => agent_set(ctx, args),
        "agent_set_role" => agent_set_role(ctx, args),
        "agent_set_permission" => agent_set_permission(ctx, args),
        "agent_subagent_add" => agent_subagent_add(ctx, args),
        "agent_subagent_remove" => agent_subagent_remove(ctx, args),
        "agent_delete" => agent_delete(ctx, args),
        "mission_list" => mission_list(ctx, &project(args)?),
        "mission_get" => mission_get(ctx, &project(args)?, &require_str(args, "mission")?),
        "mission_status" => mission_status(
            ctx,
            &project(args)?,
            args.get("mission").and_then(Value::as_str),
        ),
        "mission_await" => mission_await(
            ctx,
            &project(args)?,
            args.get("mission").and_then(Value::as_str),
            args.get("timeout_secs").and_then(Value::as_u64),
        ),
        "mission_new" => mission_new(ctx, args),
        "mission_launch" => mission_launch(ctx, args, origin),
        "mission_delete" => mission_delete(ctx, args),
        "mission_set_budget" => mission_set_budget(ctx, args),
        "mission_set_pins" => mission_set_pins(ctx, args),
        "corpus_wipe" => corpus_wipe(ctx, args),
        "corpus_stats" => corpus_stats(ctx, &project(args)?),
        "corpus_list" => corpus_list(ctx, args),
        "corpus_read" => corpus_read(ctx, args),
        "finding_list" => finding_list(ctx, args),
        "entry_delete" => entry_delete(ctx, args),
        "entry_move" => entry_move(ctx, args),
        "entry_write" => entry_write(ctx, args),
        "model_list" => model_list(args),
        other => Err(Error::Args(format!("unknown admin tool: {other}"))),
    }
}

fn require_str(args: &Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| Error::Args(format!("missing string argument: {key}")))
}

fn require_bool(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Load a project or surface a clean error.
fn load_project(store: &Store, slug: &str) -> Result<Project> {
    Project::load(store, slug).map_err(|e| Error::Args(e.to_string()))
}

// --- projects ---

fn project_list(ctx: &mut Ctx) -> Result<String> {
    let projects = ctx.store.list_projects().map_err(|e| Error::Args(e.to_string()))?;
    Ok(projects
        .iter()
        .map(|(slug, p)| {
            format!(
                "{slug} \"{name}\" plugin={} gen={}{}",
                p.plugin,
                p.corpus_generation,
                p.cloned_from
                    .as_deref()
                    .map(|f| format!(" cloned-from={f}"))
                    .unwrap_or_default(),
                name = if p.name.is_empty() { slug } else { &p.name },
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn project_new(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let slug = require_str(args, "slug")?;
    let name = args.get("name").and_then(Value::as_str).unwrap_or(&slug).to_string();
    let plugin = args.get("plugin").and_then(Value::as_str).unwrap_or("cdk-regtest");
    let project = ctx
        .store
        .create_project(&slug, &name, plugin)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("created project {slug} (plugin: {})", project.plugin))
}

fn project_clone(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let from = require_str(args, "from")?;
    let to = require_str(args, "to")?;
    let name = args.get("name").and_then(Value::as_str);
    let with_corpus = require_bool(args, "with_corpus");
    ctx.store
        .clone_project(&from, &to, name, with_corpus)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("cloned project {from} -> {to}"))
}

fn project_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let slug = require_str(args, "slug")?;
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "project_delete", &slug, token, |store| {
            store.delete_project(&slug).map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!("deleted project {slug}"))
        })
    } else {
        let p = load_project(&ctx.store, &slug)?;
        let stats = walk_corpus_stats(&ctx.store, &slug).unwrap_or_default();
        let agents = ctx
            .store
            .list_agents(&slug)
            .map(|a| a.len())
            .unwrap_or(0);
        let missions = ctx
            .store
            .list_missions(&slug)
            .map(|m| m.len())
            .unwrap_or(0);
        mint_confirm(ctx, "project_delete", &slug, &format!(
            "DRY RUN — would delete project {slug} (plugin {}, gen {}, agents {}, missions {}, corpus files {})",
            p.plugin, p.corpus_generation, agents, missions, stats.files
        ))
    }
}

fn project_rebind(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let slug = require_str(args, "slug")?;
    let plugin = require_str(args, "plugin")?;
    // Rebind must validate against discovery: a hallucinated plugin name is
    // refused (chunk-0 finding). Enforced against the registry, not vibes.
    let known = corpus_observe::plugin_names().map_err(|e| Error::Args(e.to_string()))?;
    if !known.iter().any(|name| name == &plugin) {
        return Err(Error::Args(format!(
            "unknown plugin {plugin:?} — not in the registry; known plugins:\n{}",
            known
                .iter()
                .map(|name| format!("  {name}"))
                .collect::<Vec<_>>()
                .join("\n")
        )));
    }
    let project = ctx
        .store
        .rebind_project(&slug, &plugin)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("rebound project {slug} -> plugin {plugin} (gen {})", project.corpus_generation))
}

// --- agents ---

fn agent_list(ctx: &mut Ctx, project: &str) -> Result<String> {
    let agents = ctx
        .store
        .list_agents(project)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(agents
        .iter()
        .map(|(slug, a)| {
            // One-line description from the primary agent's config, so the
            // caller rarely needs the (huge) agent_get dump to know what an
            // agent IS.
            let desc = a
                .doc
                .get("agent")
                .and_then(|m| m.as_object())
                .and_then(|m| {
                    m.values().find_map(|cfg| cfg.get("description").and_then(|d| d.as_str()))
                })
                .unwrap_or("")
                .replace('\n', " ");
            let desc: String = desc.chars().take(80).collect();
            format!(
                "{slug} hash={} — {desc}",
                ctx.store.agent_config_hash(project, slug).unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn agent_get(ctx: &mut Ctx, project: &str, agent: &str) -> Result<String> {
    let config = ctx
        .store
        .load_agent(project, agent)
        .map_err(|e| Error::Args(e.to_string()))?;
    serde_json::to_string_pretty(&config.doc).map_err(Error::Json)
}

fn agent_save(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let doc = args.get("document").ok_or_else(|| Error::Args("missing document".into()))?;
    // The core validator runs server-side; a rejected doc is never written.
    ctx.store
        .save_agent(&project, &agent, doc)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("saved agent {project}/{agent} (validator passed)"))
}

fn agent_new(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let description = require_str(args, "description")?;
    let prompt = require_str(args, "prompt")?;
    let model = args.get("model").and_then(Value::as_str);
    let from = args.get("from").and_then(Value::as_str);
    // An explicit role beats inference: inference reads the permission
    // block the new agent inherited, which says what a DIFFERENT agent was
    // allowed to do.
    let role = match args.get("role").and_then(Value::as_str) {
        Some(raw) => Some(AgentRole::parse(raw).ok_or_else(|| {
            Error::Args(format!(
                "role {raw:?} is not one of {}",
                AgentRole::names()
            ))
        })?),
        None => None,
    };
    // The core validator runs server-side on the built document.
    ctx.store
        .create_agent(&project, &agent, &description, &prompt, model, from, role)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!(
        "created agent {project}/{agent}{} (validator passed)",
        from.map(|f| format!(" from {f}")).unwrap_or_default()
    ))
}

fn agent_clone(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let from = require_str(args, "from")?;
    let to = require_str(args, "to")?;
    ctx.store
        .clone_agent(&project, &from, &to)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("cloned agent {project}/{from} -> {to}"))
}

/// Cross-project copy — the operation `agent_clone` cannot express.
fn agent_copy(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let from_project = require_str(args, "from_project")?;
    let from = require_str(args, "from")?;
    let to_project = require_str(args, "to_project")?;
    // Defaulting `to` to the source slug makes the common case ("copy this
    // agent over there") a three-argument call.
    let to = args
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or(&from)
        .to_string();
    ctx.store
        .copy_agent(&from_project, &from, &to_project, &to)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!(
        "copied agent {from_project}/{from} -> {to_project}/{to}"
    ))
}

/// The `subagent` argument, shared by the granular editors.
fn subagent_arg(args: &Value) -> Option<String> {
    args.get("subagent")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn agent_set(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let field = require_str(args, "field")?;
    let value = args
        .get("value")
        .cloned()
        .ok_or_else(|| Error::Args("missing value (pass null to clear)".into()))?;
    let subagent = subagent_arg(args);
    ctx.store
        .set_agent_field(&project, &agent, subagent.as_deref(), &field, value)
        .map_err(|e| Error::Args(e.to_string()))?;
    let target = subagent.unwrap_or_else(|| agent.clone());
    Ok(format!("set {field} on {project}/{agent} entry {target} (validator passed)"))
}

fn agent_set_role(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let raw = require_str(args, "role")?;
    let role = AgentRole::parse(&raw).ok_or_else(|| {
        Error::Args(format!(
            "unknown role {raw:?} — one of {}",
            AgentRole::names()
        ))
    })?;
    match subagent_arg(args) {
        Some(sub) => {
            ctx.store
                .set_subagent_role(&project, &agent, &sub, role)
                .map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!(
                "{project}/{agent} subagent {sub}: role -> {} (capped by the primary's at launch)",
                role.as_str()
            ))
        }
        None => {
            ctx.store
                .set_agent_role(&project, &agent, role)
                .map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!(
                "{project}/{agent}: role -> {} (server-enforced; grants {})",
                role.as_str(),
                role.tools()
                    .iter()
                    .map(|t| t.trim_start_matches("corpus_"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        }
    }
}

fn agent_set_permission(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let patch = args
        .get("patch")
        .ok_or_else(|| Error::Args("missing patch object".into()))?;
    let subagent = subagent_arg(args);
    ctx.store
        .patch_agent_permission(&project, &agent, subagent.as_deref(), patch)
        .map_err(|e| Error::Args(e.to_string()))?;
    let target = subagent.unwrap_or_else(|| agent.clone());
    Ok(format!("patched permissions on {project}/{agent} entry {target} (validator passed)"))
}

fn agent_subagent_add(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let name = require_str(args, "name")?;
    let description = require_str(args, "description")?;
    let prompt = require_str(args, "prompt")?;
    let model = args.get("model").and_then(Value::as_str);
    let role = args
        .get("role")
        .and_then(Value::as_str)
        .map(|r| {
            AgentRole::parse(r).ok_or_else(|| {
                Error::Args(format!(
                    "unknown role {r:?} — one of {}",
                    AgentRole::names()
                ))
            })
        })
        .transpose()?;
    ctx.store
        .add_subagent(&project, &agent, &name, &description, &prompt, model, role)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("added subagent {name} to {project}/{agent} (delegation wired)"))
}

fn agent_subagent_remove(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let name = require_str(args, "name")?;
    ctx.store
        .remove_subagent(&project, &agent, &name)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("removed subagent {name} from {project}/{agent}"))
}

fn agent_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let target = format!("{project}/{agent}");
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "agent_delete", &target, token, |store| {
            let missions = store
                .missions_for_agent(&project, &agent)
                .map_err(|e| Error::Args(e.to_string()))?;
            store.delete_agent(&project, &agent).map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!(
                "deleted agent {project}/{agent} and {} assigned mission(s){}",
                missions.len(),
                mission_list_suffix(&missions)
            ))
        })
    } else {
        // Load the target FIRST. A dry-run that mints a token for an agent
        // it never looked at spends a turn and then fails on the SECOND
        // call, with an error about the target rather than about the typo —
        // and says nothing about what deleting it would cost.
        let config = ctx
            .store
            .load_agent(&project, &agent)
            .map_err(|e| Error::Args(e.to_string()))?;
        let subagents = config
            .doc
            .get("agent")
            .and_then(|a| a.as_object())
            .map_or(0, |m| m.len().saturating_sub(1));
        let orphaned = delegation_dependents(&ctx.store, &project, &agent);
        let missions = ctx
            .store
            .missions_for_agent(&project, &agent)
            .map_err(|e| Error::Args(e.to_string()))?;
        let consequence = match orphaned.is_empty() {
            true => String::new(),
            // The delegation-closure check refuses to render a project
            // whose agents delegate to a name nobody declares, so this
            // deletion would take the whole project's next launch with it.
            false => format!(
                "; {} would be left delegating to entries this removes ({}), and the next \
                 launch would refuse to render the project until that is fixed",
                orphaned.len(),
                orphaned.join(", ")
            ),
        };
        mint_confirm(ctx, "agent_delete", &target, &format!(
            "DRY RUN — would delete agent {project}/{agent} (role {}, {subagents} subagent(s)) and {} assigned mission(s){}{consequence}",
            config.meta.role().as_str(),
            missions.len(),
            mission_list_suffix(&missions)
        ))
    }
}

fn mission_list_suffix(missions: &[String]) -> String {
    (!missions.is_empty())
        .then(|| format!(": {}", missions.join(", ")))
        .unwrap_or_default()
}

/// Agents that delegate to an entry `agent` declares. Deleting it would
/// leave their `task:` rules pointing at a name no agent in the project
/// declares.
fn delegation_dependents(store: &Store, project: &str, agent: &str) -> Vec<String> {
    let Ok(agents) = store.list_agents(project) else {
        return Vec::new();
    };
    let entries: Vec<String> = agents
        .iter()
        .find(|(slug, _)| slug == agent)
        .and_then(|(_, cfg)| cfg.doc.get("agent").and_then(|a| a.as_object()))
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    for (slug, config) in &agents {
        if slug == agent {
            continue;
        }
        let Some(map) = config.doc.get("agent").and_then(|a| a.as_object()) else {
            continue;
        };
        let delegates = map.values().any(|entry| {
            entry
                .get("permission")
                .and_then(|p| p.get("task"))
                .and_then(|t| t.as_object())
                .is_some_and(|rules| {
                    rules.iter().any(|(name, action)| {
                        action.as_str() != Some("deny") && entries.contains(name)
                    })
                })
        });
        if delegates {
            out.push(slug.clone());
        }
    }
    out
}

// --- missions ---

/// Whether a mission is up RIGHT NOW, derived rather than stored: its
/// recorded tmux session appears in the live listing. The mission record
/// keeps no lifecycle field — a mission is live because a session is, and
/// that account stays true across an app crash where a stored one would
/// not. `live` is the listing, taken once per call.
fn mission_live(mission: &Mission, live: &[String]) -> bool {
    mission
        .session
        .as_deref()
        .is_some_and(|session| live.iter().any(|l| l == session))
}

/// `yes`/`no` for the operator-facing listings.
fn live_label(mission: &Mission, live: &[String]) -> &'static str {
    if mission_live(mission, live) { "yes" } else { "no" }
}

fn mission_list(ctx: &mut Ctx, project: &str) -> Result<String> {
    let missions = ctx
        .store
        .list_missions(project)
        .map_err(|e| Error::Args(e.to_string()))?;
    let live = corpus_observe::live_tui_sessions();
    Ok(missions
        .iter()
        .map(|(slug, m)| {
            format!(
                "{:<20} agent={} budget={} live={}",
                slug,
                m.agent,
                m.budget.as_deref().unwrap_or("-"),
                live_label(m, &live)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn mission_get(ctx: &mut Ctx, project: &str, slug: &str) -> Result<String> {
    let mission = ctx
        .store
        .load_mission(project, slug)
        .map_err(|e| Error::Args(e.to_string()))?;
    let brief = ctx
        .store
        .mission_brief(project, slug)
        .map_err(|e| Error::Args(e.to_string()))?;
    let live = corpus_observe::live_tui_sessions();
    Ok(format!(
        "--- mission {project}/{slug} ---\nagent: {}\nbudget: {}\nlive: {}\npins: {:?}\n\n{}",
        mission.agent,
        mission.budget.as_deref().unwrap_or("-"),
        live_label(&mission, &live),
        mission.pins,
        brief
    ))
}

/// The live run state of a mission, worded for the curator: `running`
/// (the agent is producing right now), `waiting · last active <dur>` (session
/// live but parked at its prompt), or `idle` (nothing up). This is the same
/// signal the app's sidebar dots show — the curator polls it to pace a team.
fn status_label(state: &MissionRunState) -> String {
    match state.activity {
        MissionActivity::Working => "running".to_string(),
        MissionActivity::Waiting => match state.idle_secs {
            Some(secs) => format!("waiting · last active {}", fmt_idle(secs)),
            None => "waiting".to_string(),
        },
        MissionActivity::Idle => "idle".to_string(),
    }
}

/// A compact idle duration: `<Ns` / `<Nm` / `<NhNm`.
fn fmt_idle(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Report the LIVE activity of missions — `running` / `waiting` / `idle` —
/// derived from tmux + the run capture, not a stored field. With `mission`,
/// one row; else every mission on the project. The tmux listing is fetched
/// once and shared across rows.
fn mission_status(ctx: &mut Ctx, project: &str, mission: Option<&str>) -> Result<String> {
    let live = corpus_observe::live_tui_sessions();
    let rows: Vec<(String, Mission)> = match mission {
        Some(slug) => {
            let m = ctx
                .store
                .load_mission(project, slug)
                .map_err(|e| Error::Args(e.to_string()))?;
            vec![(slug.to_string(), m)]
        }
        None => ctx
            .store
            .list_missions(project)
            .map_err(|e| Error::Args(e.to_string()))?,
    };
    if rows.is_empty() {
        return Ok(format!("no missions on {project}"));
    }
    Ok(rows
        .iter()
        .map(|(slug, m)| {
            let state = corpus_observe::mission_run_state(&ctx.store, project, m, &live);
            format!("{:<24} {}", slug, status_label(&state))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// How often `mission_await` re-checks while it blocks.
const AWAIT_POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// Default and hard cap for how long ONE `mission_await` call blocks. Kept
/// short of a typical MCP client's tool-call timeout. The modest cap also
/// bounds how long a blocked call
/// withholds this single-threaded server from its next request (a cancel
/// included). Tune to opencode's actual tool timeout if it differs.
const AWAIT_DEFAULT_SECS: u64 = 45;
const AWAIT_CAP_SECS: u64 = 90;

/// The missions `mission_await`/`mission_status` watch: one named, or all on
/// the project.
fn watched_missions(
    store: &Store,
    project: &str,
    mission: Option<&str>,
) -> Result<Vec<(String, Mission)>> {
    match mission {
        Some(slug) => {
            let m = store
                .load_mission(project, slug)
                .map_err(|e| Error::Args(e.to_string()))?;
            Ok(vec![(slug.to_string(), m)])
        }
        None => store
            .list_missions(project)
            .map_err(|e| Error::Args(e.to_string())),
    }
}

/// Per-mission run state right now, keyed by slug — what `mission_await`
/// diffs across polls to notice a mission stopping (or starting) work.
fn state_snapshot(
    store: &Store,
    project: &str,
    missions: &[(String, Mission)],
    live: &[String],
) -> std::collections::BTreeMap<String, MissionRunState> {
    missions
        .iter()
        .map(|(slug, m)| {
            (
                slug.clone(),
                corpus_observe::mission_run_state(store, project, m, live),
            )
        })
        .collect()
}

/// Every corpus entry path (relative, all categories including runs) — the
/// snapshot `mission_await` diffs to notice new output a mission produced.
/// A running mission's `runs/` capture already exists at entry, so its
/// growth is not seen here; a freshly written technique/finding/attack is.
fn corpus_entry_set(store: &Store, project: &str) -> std::collections::BTreeSet<String> {
    let root = store.project_corpus_dir(project);
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(rel) = path.strip_prefix(&root) {
                out.insert(rel.to_string_lossy().into_owned());
            }
        }
    }
    out
}

fn activity_word(a: MissionActivity) -> &'static str {
    match a {
        MissionActivity::Working => "running",
        MissionActivity::Waiting => "waiting",
        MissionActivity::Idle => "idle",
    }
}

/// The change report `mission_await` returns — or `None` when nothing the
/// curator would act on has happened yet. Pure over the before/after run
/// states and the corpus paths that appeared since, so it is tested without
/// tmux or sleeps: an activity flip (running→waiting = a turn finished) or
/// any new corpus output is a wake; a mission unchanged and silent is not.
fn await_report(
    before: &std::collections::BTreeMap<String, MissionRunState>,
    now: &std::collections::BTreeMap<String, MissionRunState>,
    new_entries: &[String],
) -> Option<String> {
    let mut lines = Vec::new();
    for (slug, state) in now {
        let prev = before.get(slug).map(|b| b.activity);
        if prev != Some(state.activity) {
            let was = prev.map(activity_word).unwrap_or("new");
            lines.push(format!("{slug}: {was} → {}", status_label(state)));
        }
    }
    if lines.is_empty() && new_entries.is_empty() {
        return None;
    }
    if !new_entries.is_empty() {
        lines.push(format!("new corpus output: {}", new_entries.join(", ")));
    }
    Some(lines.join("\n"))
}

/// Operator diagnostic: block until a watched mission's run state flips or
/// new corpus output lands, then report it. Agent roles do not receive this
/// tool; background supervision belongs to the app. Bounded by `timeout_secs`
/// (default 45, max 90); on timeout it returns the current state and stops.
fn mission_await(
    ctx: &mut Ctx,
    project: &str,
    mission: Option<&str>,
    timeout_secs: Option<u64>,
) -> Result<String> {
    let cap = timeout_secs.unwrap_or(AWAIT_DEFAULT_SECS).clamp(1, AWAIT_CAP_SECS);
    let missions = watched_missions(&ctx.store, project, mission)?;
    if missions.is_empty() {
        return Ok(format!("no missions on {project}"));
    }
    let before = state_snapshot(
        &ctx.store,
        project,
        &missions,
        &corpus_observe::live_tui_sessions(),
    );
    let entries0 = corpus_entry_set(&ctx.store, project);

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(cap);
    loop {
        if std::time::Instant::now() >= deadline {
            let live = corpus_observe::live_tui_sessions();
            let status = state_snapshot(&ctx.store, project, &missions, &live)
                .iter()
                .map(|(slug, st)| format!("{slug}: {}", status_label(st)))
                .collect::<Vec<_>>()
                .join("\n");
            return Ok(format!(
                "no change in {cap}s — bounded diagnostic wait ended.\n{status}"
            ));
        }
        std::thread::sleep(AWAIT_POLL);
        // Re-load records so a mission created, deleted, or relaunched (new
        // session id) since entry is reflected, not stale.
        let missions_now =
            watched_missions(&ctx.store, project, mission).unwrap_or_else(|_| missions.clone());
        let live = corpus_observe::live_tui_sessions();
        let now = state_snapshot(&ctx.store, project, &missions_now, &live);
        let new_entries: Vec<String> = corpus_entry_set(&ctx.store, project)
            .difference(&entries0)
            .cloned()
            .collect();
        if let Some(report) = await_report(&before, &now, &new_entries) {
            return Ok(report);
        }
    }
}

#[cfg(test)]
mod await_tests {
    use super::*;
    fn st(a: MissionActivity, idle: Option<u64>) -> MissionRunState {
        MissionRunState { activity: a, idle_secs: idle }
    }

    fn map(pairs: &[(&str, MissionRunState)]) -> std::collections::BTreeMap<String, MissionRunState> {
        pairs.iter().map(|(s, r)| (s.to_string(), *r)).collect()
    }

    #[test]
    fn no_change_and_no_output_is_none() {
        let before = map(&[("recon", st(MissionActivity::Working, Some(1)))]);
        let now = map(&[("recon", st(MissionActivity::Working, Some(2)))]);
        assert!(await_report(&before, &now, &[]).is_none());
    }

    #[test]
    fn a_finished_turn_is_reported() {
        let before = map(&[("recon", st(MissionActivity::Working, Some(1)))]);
        let now = map(&[("recon", st(MissionActivity::Waiting, Some(9)))]);
        let report = await_report(&before, &now, &[]).expect("a flip is a wake");
        assert!(report.contains("recon: running → waiting"), "{report}");
    }

    #[test]
    fn new_output_alone_is_reported() {
        let before = map(&[("recon", st(MissionActivity::Working, Some(1)))]);
        let now = map(&[("recon", st(MissionActivity::Working, Some(2)))]);
        let report = await_report(&before, &now, &["techniques/c2.md".to_string()])
            .expect("new output is a wake even with no state flip");
        assert!(report.contains("new corpus output: techniques/c2.md"), "{report}");
    }
}

fn mission_new(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let slug = require_str(args, "slug")?;
    let agent = require_str(args, "agent")?;
    let brief = require_str(args, "brief")?;
    let budget = args.get("budget").and_then(Value::as_str).map(str::to_string);
    // The operator-facing display name (the sidebar label). Optional: an
    // empty/absent name falls back to the slug in the nav.
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let mut pins = corpus_observe::project_source_pins(ctx.store, &project)
        .map_err(|e| Error::Args(e.to_string()))?;
    pins.extend(parse_pins(args.get("pins"))?);
    validate_pins(ctx, &project, &pins)?;
    let mission = Mission {
        agent,
        pins,
        budget,
        created: now(),
        name,
        session: None,
        control: None,
        opencode_session: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    };
    ctx.store
        .write_mission(&project, &slug, &mission, &brief)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("created mission {project}/{slug} (agent {})", mission.agent))
}

/// Request that the app launch a mission — flag the record and let the
/// app's poll beat spawn the session. The MCP process cannot spawn a run
/// itself (run spawning, tmux, and the embedded pane are the app's), so
/// this is a REQUEST, honored the moment the app sees it. Idempotent: a
/// mission already flagged (or already live) is left as it is.
fn mission_launch(
    ctx: &mut Ctx,
    args: &Value,
    origin: Option<&MissionRunRef>,
) -> Result<String> {
    let project = require_str(args, "project")?;
    let slug = require_str(args, "mission")?;
    if origin.is_some_and(|origin| origin.project != project) {
        return Err(Error::Args(
            "launch origin does not belong to the proven project scope".into(),
        ));
    }
    let mut mission = ctx
        .store
        .load_mission(&project, &slug)
        .map_err(|e| Error::Args(e.to_string()))?;
    if mission.delete_requested.is_some() {
        return Err(Error::Args(format!(
            "mission {project}/{slug} is pending deletion"
        )));
    }
    if mission.launch_requested.is_none() {
        mission.launch_requested = Some(MissionLaunchRequest {
            requested_at: now(),
            requested_by: origin.cloned(),
        });
        ctx.store
            .update_mission(&project, &slug, &mission)
            .map_err(|e| Error::Args(e.to_string()))?;
    }
    Ok(format!(
        "launch requested for {project}/{slug} — the app will spawn its opencode session and kick it off with the brief"
    ))
}

fn mission_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let mission = require_str(args, "mission")?;
    let target = format!("{project}/{mission}");
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "mission_delete", &target, token, |store| {
            let mut record = store
                .load_mission(&project, &mission)
                .map_err(|e| Error::Args(e.to_string()))?;
            if store.ensure_mission_deletable(&project, &mission).is_ok() {
                store
                    .delete_mission(&project, &mission)
                    .map_err(|e| Error::Args(e.to_string()))?;
                return Ok(format!("deleted mission {project}/{mission}"));
            }
            record.launch_requested = None;
            if record.delete_requested.is_none() {
                record.delete_requested = Some(MissionDeleteRequest {
                    requested_at: now(),
                });
                store
                    .update_mission(&project, &mission, &record)
                    .map_err(|e| Error::Args(e.to_string()))?;
            }
            Ok(format!(
                "deletion requested for mission {project}/{mission} — the app will tear down its run and environment before removing the record"
            ))
        })
    } else {
        let record = ctx
            .store
            .load_mission(&project, &mission)
            .map_err(|e| Error::Args(e.to_string()))?;
        // Liveness belongs in the dry run above all: deleting the record of
        // a mission whose session is still up orphans a running agent.
        let live = corpus_observe::live_tui_sessions();
        mint_confirm(ctx, "mission_delete", &target, &format!(
            "DRY RUN — would delete mission {project}/{mission} (agent {}, live {}{})",
            record.agent,
            live_label(&record, &live),
            record
                .budget
                .as_deref()
                .map(|b| format!(", budget {b}"))
                .unwrap_or_default()
        ))
    }
}

fn mission_set_budget(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let mission_slug = require_str(args, "mission")?;
    let budget = require_str(args, "budget")?;
    let mut mission = ctx
        .store
        .load_mission(&project, &mission_slug)
        .map_err(|e| Error::Args(e.to_string()))?;
    let old = mission.budget.clone();
    mission.budget = Some(budget.clone());
    ctx.store
        .update_mission(&project, &mission_slug, &mission)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!(
        "set mission {project}/{mission_slug} budget: {} -> {} (budget is per-MISSION)",
        old.as_deref().unwrap_or("-"),
        budget
    ))
}

fn mission_set_pins(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let mission_slug = require_str(args, "mission")?;
    let mut mission = ctx
        .store
        .load_mission(&project, &mission_slug)
        .map_err(|e| Error::Args(e.to_string()))?;
    mission.pins = parse_pins(args.get("pins"))?;
    validate_pins(ctx, &project, &mission.pins)?;
    ctx.store
        .update_mission(&project, &mission_slug, &mission)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("set mission {project}/{mission_slug} pins: {:?}", mission.pins))
}

fn parse_pins(value: Option<&Value>) -> Result<std::collections::BTreeMap<String, String>> {
    let mut pins = std::collections::BTreeMap::new();
    if let Some(obj) = value.and_then(Value::as_object) {
        for (repo, rev) in obj {
            let rev = rev
                .as_str()
                .ok_or_else(|| Error::Args(format!("pin {repo} value must be a string")))?;
            pins.insert(repo.clone(), rev.to_string());
        }
    }
    Ok(pins)
}

/// Reject a pin the curator authored that could never resolve — a typo'd
/// rev fails HERE, at authoring, not at launch. Structural (no network);
/// a raw commit sha and the manifest tag are both accepted.
fn validate_pins(
    ctx: &Ctx,
    project: &str,
    pins: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    for (repo, rev) in pins {
        corpus_observe::validate_pin(&ctx.store, project, repo, rev)
            .map_err(|e| Error::Args(e.to_string()))?;
    }
    Ok(())
}

// --- corpus ---

fn corpus_wipe(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "corpus_wipe", &project, token, |store| {
            let p = store
                .wipe_project_corpus(&project)
                .map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!("wiped project corpus {project} (generation -> {})", p.corpus_generation))
        })
    } else {
        // No token: dry-run. Report what would be deleted; mint the token.
        let stats = walk_corpus_stats(&ctx.store, &project).map_err(|e| Error::Args(e.to_string()))?;
        let p = load_project(&ctx.store, &project)?;
        mint_confirm(ctx, "corpus_wipe", &project, &format!(
            "DRY RUN — would wipe the corpus of project {project} ({} files, {} bytes, generation -> {}); project and its agents survive",
            stats.files, stats.bytes, p.corpus_generation + 1
        ))
    }
}

fn corpus_stats(ctx: &mut Ctx, project: &str) -> Result<String> {
    let stats = walk_corpus_stats(&ctx.store, project).map_err(|e| Error::Args(e.to_string()))?;
    // Mission logs are reported apart from the knowledge categories —
    // transcripts dominate the byte total and would hide the rest.
    Ok(format!(
        "corpus {project}: {} files, {} bytes; mission logs: {} files, {} bytes",
        stats.knowledge_files(),
        stats.knowledge_bytes(),
        stats.logs.files,
        stats.logs.bytes
    ))
}

fn corpus_list(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let category = require_str(args, "category")?;
    if !CATEGORIES.contains(&category.as_str()) {
        return Err(Error::Args(format!(
            "unknown category {category:?}; categories: {}",
            CATEGORIES.join(", ")
        )));
    }
    let dir = ctx.store.project_corpus_dir(&project).join(&category);
    let mut entries: Vec<String> = std::fs::read_dir(&dir)
        .map_err(|e| Error::Args(format!("corpus {project}/{category}: {e}")))?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    entries.sort();
    Ok(if entries.is_empty() {
        format!("(empty) {project}/{category}/")
    } else {
        format!("{project}/{category}/:\n{}", entries.join("\n"))
    })
}

fn finding_list(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let mut severities = BTreeSet::new();
    let severity_values: Vec<&Value> = match args.get("severity") {
        None => Vec::new(),
        Some(value @ Value::String(_)) => vec![value],
        Some(Value::Array(values)) => values.iter().collect(),
        Some(_) => {
            return Err(Error::Args(
                "severity must be a string or an array of strings".into(),
            ))
        }
    };
    for value in severity_values {
        let raw = value
            .as_str()
            .ok_or_else(|| Error::Args("severity array entries must be strings".into()))?;
        severities.insert(FindingSeverity::parse(raw).ok_or_else(|| {
            Error::Args(format!(
                "invalid finding severity {raw:?}; expected critical, high, medium, or low"
            ))
        })?);
    }
    let sort = match args.get("sort").and_then(Value::as_str).unwrap_or("newest") {
        "newest" => FindingSort::Newest,
        "severity" => FindingSort::Severity,
        value => {
            return Err(Error::Args(format!(
                "invalid finding sort {value:?}; expected newest or severity"
            )))
        }
    };
    let limit = match args.get("limit") {
        None => None,
        Some(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| Error::Args("limit must be a positive integer".into()))?;
            if value == 0 {
                return Err(Error::Args("limit must be a positive integer".into()));
            }
            Some(
                usize::try_from(value)
                    .map_err(|_| Error::Args("limit is too large for this platform".into()))?,
            )
        }
    };
    let query = FindingQuery {
        severities,
        include_unrated: args
            .get("include_unrated")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        text: args.get("text").and_then(Value::as_str).map(str::to_string),
        sort,
        limit,
    };
    let cards = corpus_store::finding_cards(&ctx.store, &project)
        .map_err(|error| Error::Args(error.to_string()))?;
    let cards = corpus_store::query_findings(&cards, &query);
    let findings: Vec<Value> = cards
        .iter()
        .map(|card| {
            json!({
                "path": card.path.to_string_lossy(),
                "title": card.title,
                "title_source": card.title_source.as_str(),
                "severity": card.severity.map(|severity| severity.as_str()),
                "unrated": card.severity.is_none(),
                "timestamp": card.timestamp,
                "time_source": card.time_source.map(|source| source.as_str()),
                "reference": card.reference,
                "reference_source": card.reference_source.as_str(),
                "status": card.status,
                "oracle_verified": card.oracle_verified,
                "sensitivity": card.sensitivity.map(|value| value.as_str()),
                "warnings": card.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "project": project,
        "count": findings.len(),
        "findings": findings,
    }))
    .map_err(Error::Json)
}

fn corpus_read(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let path = require_str(args, "path")?;
    // The shared guard, rather than a second inline one. The version that
    // lived here compared a canonical path against a possibly-NON-canonical
    // root, so it refused legal paths whenever the store sat behind a
    // symlink — which it does whenever a run dir is involved.
    let resolved = ctx
        .store
        .resolve_corpus_entry(&project, &path, EntryAccess::Read)
        .map_err(|e| Error::Args(e.to_string()))?;
    let text = std::fs::read_to_string(&resolved)
        .map_err(|e| Error::Args(format!("cannot read {}: {e}", resolved.display())))?;
    Ok(text)
}

fn entry_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let path = require_str(args, "path")?;
    let recursive = args.get("recursive").and_then(Value::as_bool).unwrap_or(false);
    let resolved = ctx
        .store
        .resolve_corpus_entry(&project, &path, EntryAccess::Mutate)
        .map_err(|e| Error::Args(e.to_string()))?;
    let preview = entry_preview(&resolved).map_err(|e| {
        Error::Args(format!("cannot inspect {project}/corpus/{path} before deletion: {e}"))
    })?;
    if preview.dirs > 0 && !recursive {
        return Err(Error::Args(format!(
            "{path} is a directory — pass recursive to preview and remove it and everything under it"
        )));
    }
    // Bind the token to both the requested deletion mode and a deterministic
    // snapshot of the target. If the entry changes after the dry-run, the
    // second call no longer matches and must be previewed again.
    let target = format!(
        "{project}/corpus/{path}|recursive={recursive}|snapshot={}",
        preview.fingerprint
    );
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "entry_delete", &target, token, |store| {
            let freed = store
                .delete_corpus_entry(&project, &path, recursive)
                .map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!("deleted {project}/corpus/{path} ({freed} bytes)"))
        })
    } else {
        let kind = if preview.dirs > 0 { "directory tree" } else { "file" };
        mint_confirm(
            ctx,
            "entry_delete",
            &target,
            &format!(
                "DRY RUN — would delete {kind} {project}/corpus/{path} ({} file(s), {} directory/directories, {} bytes)",
                preview.files, preview.dirs, preview.bytes
            ),
        )
    }
}

#[derive(Default)]
struct EntryPreview {
    files: u64,
    dirs: u64,
    bytes: u64,
    fingerprint: String,
}

/// A stable-enough state fingerprint for the short confirm window. It includes
/// every relative name, type, size and modification timestamp without reading
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
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let kind = if metadata.is_dir() {
            preview.dirs += 1;
            "dir"
        } else {
            preview.files += 1;
            preview.bytes = preview.bytes.saturating_add(metadata.len());
            if metadata.file_type().is_symlink() { "link" } else { "file" }
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

fn entry_move(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let from = require_str(args, "from")?;
    let to = require_str(args, "to")?;
    let overwrite = args.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
    ctx.store
        .move_corpus_entry(&project, &from, &to, overwrite)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("moved {project}/corpus/{from} -> {to}"))
}

fn entry_write(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let path = require_str(args, "path")?;
    let content = require_str(args, "content")?;
    let bytes = ctx
        .store
        .write_corpus_entry(&project, &path, &content)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("wrote {project}/corpus/{path} ({bytes} bytes)"))
}

// --- models ---

/// The opencode launch catalog (NOT the benchmark registry and NOT the chat's
/// ollama set): the exact `provider/model` ids an agent config's model field
/// accepts. A chat agent resolving "my deepseek model" to a launchable id does
/// `model_list {"filter": "deepseek"}` — a half-guessed string baked into six
/// agent JSONs is the failure this exists to prevent.
fn model_list(args: &Value) -> Result<String> {
    let filter = args
        .get("filter")
        .and_then(Value::as_str)
        .map(str::to_lowercase);
    let refresh = args
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let list = corpus_observe::model_list(refresh).map_err(|e| {
        Error::Args(format!("opencode model catalog unavailable: {e}"))
    })?;
    let mut lines = Vec::new();
    let mut total = 0usize;
    for group in &list.groups {
        let mut first_in_group = true;
        for m in &group.models {
            total += 1;
            if let Some(f) = &filter {
                if !m.id.to_lowercase().contains(f) && !m.name.to_lowercase().contains(f) {
                    continue;
                }
            }
            if first_in_group {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(format!("== {}", group.label));
                first_in_group = false;
            }
            lines.push(format!("  {}  ({})", m.id, m.name));
        }
    }
    if lines.is_empty() {
        return Ok(format!(
            "no model matches {:?} ({} models in the catalog) — widen or drop the filter",
            filter.unwrap_or_default(),
            total
        ));
    }
    let header = match &filter {
        Some(f) => format!("{} of {} models matching {f:?} — ids as in `opencode models --verbose`:", lines.iter().filter(|l| l.starts_with("  ")).count(), total),
        None => format!("{total} models available — ids as in `opencode models --verbose` (use these exact strings in agent configs):"),
    };
    Ok(format!("{header}\n{}", lines.join("\n")))
}

// --- confirm-token gate ---

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Mint a one-shot confirm token for a destructive op (dry-run call), store
/// it with a short TTL, and return it with the dry-run summary.
///
/// What this IS: a place where the consequences get computed and stated
/// before anything commits, and a requirement to name the target twice —
/// which catches the wrong-slug slip at a cost of one turn. In the
/// operator's host-admin chat it is also a human gate, because a person
/// reads the dry run between the two calls.
///
/// What it is NOT: a control on an autonomous caller. The token is returned
/// to whoever asked, so an agent completes the ritual in two calls with
/// nobody in between. Project-scoped curators therefore receive no
/// destructive tools; their audit log is accountability, not authorization.
fn mint_confirm(
    ctx: &mut Ctx,
    op: &str,
    target: &str,
    summary: &str,
) -> Result<String> {
    let nonce = format!("{}|{}", target, now());
    // Hash of op+target+nonce (fnv1a: provenance-grade, not an auth key).
    let token = fnv1a_hex(format!("{op}|{target}|{nonce}").as_bytes());
    ctx.pending_confirms.insert(
        token.clone(),
        PendingConfirm {
            op: op.to_string(),
            target: target.to_string(),
            expires_at: now() + CONFIRM_TTL_SECS,
        },
    );
    Ok(format!(
        "{summary}\n\nconfirm_token: {token} (one-shot, {}s TTL)\n\
         Call the same op again with confirm_token to commit.",
        CONFIRM_TTL_SECS
    ))
}

/// Complete a destructive op given a token. The token must exist, match the
/// op+target, and be unexpired; it is consumed (single-use) on success.
fn confirm_and_run<R: std::fmt::Display>(
    ctx: &mut Ctx,
    op: &str,
    target: &str,
    token: &str,
    run: impl FnOnce(&Store) -> Result<R>,
) -> Result<String> {
    let pending = ctx
        .pending_confirms
        .remove(token)
        .ok_or_else(|| Error::Args("invalid or expired confirm_token — re-run the dry-run to mint a fresh one".into()))?;
    if pending.op != op || pending.target != target {
        return Err(Error::Args("confirm_token does not match this op+target".to_string()));
    }
    if pending.expires_at < now() {
        return Err(Error::Args("confirm_token expired — re-run the dry-run".to_string()));
    }
    // Token consumed on a failed mutation too: a failed run is not retryable
    // via a stale token.
    let result = run(&ctx.store)?;
    Ok(format!("{}\n[confirmed with one-shot token]", result))
}
