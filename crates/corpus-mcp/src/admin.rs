//! corpus-admin MCP profile: natural-language administration of the corpus
//! store (projects, agents, missions, corpus lifecycle, read-only queries).
//!
//! This is the second trust profile of the SAME binary, gated behind the
//! `--admin` server flag. It is host-side operator tooling: it sits OUTSIDE
//! the research trust domains (no sandbox, no targets, no oracles) and never
//! runs missions — it prepares them. Every tool is a thin wrapper over the
//! corpus-core API; nothing here touches the plugin protocol or the
//! filesystem outside the store.
//!
//! The sandbox-facing profile (operator/researcher agents) never enables
//! this group — enforcement is at config level (the `--admin` flag), the
//! same pattern as the opencode permission files.

use std::time::{SystemTime, UNIX_EPOCH};

use corpus_core::{fnv1a_hex, Mission, Project, Store};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::tools::{Ctx, PendingConfirm};

/// Corpus walk for dry-run summaries (renamed to avoid shadowing the admin
/// `corpus_stats` tool handler).
use corpus_core::corpus_stats as walk_corpus_stats;

/// TTL for a confirm token: short, so an abandoned dry-run cannot be
/// replayed later to a now-stale target.
const CONFIRM_TTL_SECS: u64 = 60;

/// The four destructive ops. All require the confirm-token ritual.
pub const DESTRUCTIVE_OPS: [&str; 4] = [
    "project_delete",
    "agent_delete",
    "mission_delete",
    "corpus_wipe",
];

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
            "description": "Create a project, seeded with the core agent pair.",
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
                    "from": {"type": "string", "description": "optional existing agent to inherit permissions/prompts from"}
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
            "description": "Clone an agent (config + prompts) to a new slug.",
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
            "name": "agent_delete",
            "description": "CONFIRM-GATED. Delete an agent. Dry-run first; returns a one-shot token to complete.",
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
            "description": "List a project's missions (slug, agent, budget, status, pins).",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}},
                "required": ["project"]
            }
        },
        {
            "name": "mission_get",
            "description": "Read a mission record (frontmatter + brief body).",
            "inputSchema": {
                "type": "object",
                "properties": {"project": {"type": "string"}, "mission": {"type": "string"}},
                "required": ["project", "mission"]
            }
        },
        {
            "name": "mission_new",
            "description": "Create a mission for an existing agent on the project.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "project": {"type": "string"},
                    "slug": {"type": "string"},
                    "agent": {"type": "string"},
                    "brief": {"type": "string"},
                    "budget": {"type": "string"},
                    "pins": {"type": "object"}
                },
                "required": ["project", "slug", "agent", "brief"]
            }
        },
        {
            "name": "mission_delete",
            "description": "CONFIRM-GATED. Delete a mission record. Dry-run first; returns a one-shot token to complete.",
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
            "description": "List entries in a corpus category (hypotheses | techniques | findings | attacks | runs).",
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
        "agent_delete" => agent_delete(ctx, args),
        "mission_list" => mission_list(ctx, &project(args)?),
        "mission_get" => mission_get(ctx, &project(args)?, &require_str(args, "mission")?),
        "mission_new" => mission_new(ctx, args),
        "mission_delete" => mission_delete(ctx, args),
        "mission_set_budget" => mission_set_budget(ctx, args),
        "mission_set_pins" => mission_set_pins(ctx, args),
        "corpus_wipe" => corpus_wipe(ctx, args),
        "corpus_stats" => corpus_stats(ctx, &project(args)?),
        "corpus_list" => corpus_list(ctx, args),
        "corpus_read" => corpus_read(ctx, args),
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
        mint_confirm(ctx, "project_delete", &slug, true, &format!(
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
    let known = corpus_core::discover(&corpus_core::plugins_dir())
        .map_err(|e| Error::Args(e.to_string()))?
        .into_iter()
        .any(|d| d.manifest.name == plugin);
    if !known {
        return Err(Error::Args(format!(
            "unknown plugin {plugin:?} — not in the registry; known plugins:\n{}",
            corpus_core::plugin_status()
                .iter()
                .map(|p| format!("  {}", p.name))
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
    // The core validator runs server-side on the built document.
    ctx.store
        .create_agent(&project, &agent, &description, &prompt, model, from)
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

fn agent_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let agent = require_str(args, "agent")?;
    let target = format!("{project}/{agent}");
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "agent_delete", &target, token, |store| {
            store.delete_agent(&project, &agent).map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!("deleted agent {project}/{agent}"))
        })
    } else {
        mint_confirm(ctx, "agent_delete", &target, true, &format!(
            "DRY RUN — would delete agent {project}/{agent}"
        ))
    }
}

// --- missions ---

fn mission_list(ctx: &mut Ctx, project: &str) -> Result<String> {
    let missions = ctx
        .store
        .list_missions(project)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(missions
        .iter()
        .map(|(slug, m)| {
            format!(
                "{:<20} agent={} budget={} status={}",
                slug,
                m.agent,
                m.budget.as_deref().unwrap_or("-"),
                m.status
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
    Ok(format!(
        "--- mission {project}/{slug} ---\nagent: {}\nbudget: {}\nstatus: {}\npins: {:?}\n\n{}",
        mission.agent,
        mission.budget.as_deref().unwrap_or("-"),
        mission.status,
        mission.pins,
        brief
    ))
}

fn mission_new(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let slug = require_str(args, "slug")?;
    let agent = require_str(args, "agent")?;
    let brief = require_str(args, "brief")?;
    let budget = args.get("budget").and_then(Value::as_str).map(str::to_string);
    let mission = Mission {
        agent,
        pins: parse_pins(args.get("pins"))?,
        budget,
        status: "queued".to_string(),
        created: now(),
        name: None,
        session: None,
        opencode_session: None,
    };
    ctx.store
        .write_mission(&project, &slug, &mission, &brief)
        .map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("created mission {project}/{slug} (agent {})", mission.agent))
}

fn mission_delete(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let mission = require_str(args, "mission")?;
    let target = format!("{project}/{mission}");
    if let Some(token) = args.get("confirm_token").and_then(Value::as_str) {
        confirm_and_run(ctx, "mission_delete", &target, token, |store| {
            store.delete_mission(&project, &mission).map_err(|e| Error::Args(e.to_string()))?;
            Ok(format!("deleted mission {project}/{mission}"))
        })
    } else {
        mint_confirm(ctx, "mission_delete", &target, true, &format!(
            "DRY RUN — would delete mission {project}/{mission}"
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
        mint_confirm(ctx, "corpus_wipe", &project, true, &format!(
            "DRY RUN — would wipe the corpus of project {project} ({} files, {} bytes, generation -> {}); project and its agents survive",
            stats.files, stats.bytes, p.corpus_generation + 1
        ))
    }
}

fn corpus_stats(ctx: &mut Ctx, project: &str) -> Result<String> {
    let stats = walk_corpus_stats(&ctx.store, project).map_err(|e| Error::Args(e.to_string()))?;
    Ok(format!("corpus {project}: {} files, {} bytes", stats.files, stats.bytes))
}

fn corpus_list(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let category = require_str(args, "category")?;
    if !corpus_core::CATEGORIES.contains(&category.as_str()) {
        return Err(Error::Args(format!(
            "unknown category {category:?}; categories: {}",
            corpus_core::CATEGORIES.join(", ")
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

fn corpus_read(ctx: &mut Ctx, args: &Value) -> Result<String> {
    let project = require_str(args, "project")?;
    let path = require_str(args, "path")?;
    let corpus = ctx.store.project_corpus_dir(&project);
    // Path traversal guard: resolve under the corpus root only.
    let joined = corpus.join(&path);
    let root = corpus.canonicalize().unwrap_or(corpus.clone());
    let canonical = joined.canonicalize().map_err(|e| {
        Error::Args(format!("cannot read {project}/corpus/{path}: {e}"))
    })?;
    if !canonical.starts_with(&root) {
        return Err(Error::Args("path escapes the project corpus".into()));
    }
    let text = std::fs::read_to_string(&canonical)
        .map_err(|e| Error::Args(format!("cannot read {}: {e}", canonical.display())))?;
    Ok(text)
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
    let list = corpus_core::model_list(refresh).map_err(|e| {
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
/// it with a short TTL, and return the dry-run summary plus the token so the
/// operator can see intent spelled out before anything commits.
fn mint_confirm(
    ctx: &mut Ctx,
    op: &str,
    target: &str,
    _destructive: bool,
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
        "{summary}\n\nconfirm_token: {token} (one-shot, {}s TTL)\nCall the same op again with confirm_token to commit; \
         the operator sees this intent spelled out before you can mutate.",
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
