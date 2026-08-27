//! Project-scoped OpenCode agent rendering and delegation closure.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use super::permissions::{bind_permission, canonical_json, yaml_scalar, DataRoots, RenderCtx};
use super::validation::resolve_prompt_ref;
use super::{AgentConfig, AgentRole, AgentSidecar};
use crate::error::{Error, Result};
use crate::filesystem::atomic_write;
use crate::store::Store;

impl Store {
    // -----------------------------------------------------------------
    // Renderer (.opencode/agent/)
    // -----------------------------------------------------------------

    /// The directory materialized agents land in: the PROJECT's own
    /// `.opencode/agent/` inside its run directory. Per-project by
    /// construction — one project's launch never rewrites another's.
    ///
    /// Pure path computation. It used to provision the run dir as a side
    /// effect, which meant merely CLEARING the old agent set resolved the
    /// resource root and rewrote the MCP config. Provisioning happens once,
    /// at launch.
    pub fn opencode_agent_dir(&self, project: &str) -> PathBuf {
        self.project_run_dir(project)
            .join(".opencode")
            .join("agent")
    }

    /// Clear the previously generated agent set in the project's
    /// `.opencode/agent/` (corpus-managed: the dir is regenerated per
    /// launch).
    pub fn clear_opencode_agents(&self, project: &str) {
        {
            let dir = self.opencode_agent_dir(project);
            if let Ok(read) = fs::read_dir(&dir) {
                for entry in read.flatten() {
                    let path = entry.path();
                    if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                        let _ = fs::remove_file(path);
                    }
                }
            }
        }
    }

    /// Verify the project's agent set is CLOSED UNDER DELEGATION: every
    /// entry a `task:` allowlist names is declared by some agent in the
    /// project. A dangling name is not inert — opencode resolves an agent
    /// it cannot find in the run dir from whatever config it discovers
    /// above the cwd, which is how a project's primary came to delegate to
    /// another project's scout. Public so the app can flag it before a
    /// launch tries to render.
    pub fn check_project_delegation(&self, project: &str) -> Result<()> {
        let agents = self.list_agents(project)?;
        let claimed = claim_entries(&agents, project)?;
        check_delegation_closure(&agents, &claimed, project)
    }

    /// Render EVERY agent of the project into `.opencode/agent/*.md` (bare
    /// names) — after clearing the previous generated set, so the agent
    /// list opencode shows is scoped to this project. Agents render in
    /// slug order. Returns the written paths.
    ///
    /// Takes no source pins: they are a property of the RUN and reach the
    /// agent through `target_info`, so this render is a pure function of
    /// the project and produces identical bytes on every launch. That is
    /// what makes re-rendering safe while another mission is live.
    ///
    /// Validation runs BEFORE the clear, so a refused render leaves the
    /// previously rendered set on disk rather than half-scoping the
    /// project.
    pub fn render_project_agents(&self, project: &str) -> Result<Vec<PathBuf>> {
        let agents = self.list_agents(project)?;
        let claimed = claim_entries(&agents, project)?;
        check_delegation_closure(&agents, &claimed, project)?;
        let handles = primary_handles(&agents);
        let known: BTreeSet<String> = claimed.into_keys().collect();
        let roots = DataRoots::for_store(self);
        self.clear_opencode_agents(project);
        let out_dir = self.opencode_agent_dir(project);
        fs::create_dir_all(&out_dir)?;
        let mut written = Vec::new();
        for (slug, agent) in &agents {
            let dir = self.project_agent_dir(project, slug);
            let Some(agent_map) = agent.doc.get("agent").and_then(|v| v.as_object()) else {
                continue;
            };
            let mut names: Vec<&String> = agent_map.keys().collect();
            names.sort();
            for name in names {
                let cfg = agent_map[name]
                    .as_object()
                    .ok_or_else(|| Error::Store(format!("agent {slug}/{name}: not an object")))?;
                let ctx = RenderCtx {
                    project,
                    role: entry_role(&agent.meta, name, slug),
                    known_entries: &known,
                    roots: roots.clone(),
                };
                let body = render_agent_file(cfg, &dir, &ctx)?;
                // The primary renders under its project handle (a name, not
                // the opaque slug); subagents keep their bare keys.
                let dest = out_dir.join(format!("{}.md", rendered_name(&handles, slug, name)));
                atomic_write(&dest, body)?;
                written.push(dest);
            }
        }
        Ok(written)
    }

    /// Render the launched agent into `.opencode/agent/*.md` (bare names) —
    /// ADDITIVE (no clear): used to layer a follow-up agent (the CLI
    /// `--research` pass) onto an already project-scoped set. Launch paths
    /// scope the set with [`Store::render_project_agents`] instead.
    /// Returns the written paths.
    pub fn render_agent(&self, project: &str, slug: &str) -> Result<Vec<PathBuf>> {
        let agent = self.load_agent(project, slug)?;
        // Delegation is checked against the WHOLE project: this path layers
        // one agent onto an already project-scoped set, so the set it joins
        // is what its `task:` rules may name.
        let agents = self.list_agents(project)?;
        let claimed = claim_entries(&agents, project)?;
        check_delegation_closure(&agents, &claimed, project)?;
        let handles = primary_handles(&agents);
        let known: BTreeSet<String> = claimed.into_keys().collect();
        let roots = DataRoots::for_store(self);
        let out_dir = self.opencode_agent_dir(project);
        fs::create_dir_all(&out_dir)?;
        let dir = self.project_agent_dir(project, slug);
        let agent_map = agent
            .doc
            .get("agent")
            .and_then(|v| v.as_object())
            .ok_or_else(|| Error::Store(format!("agent {slug}: missing agent map")))?;
        let mut names: Vec<&String> = agent_map.keys().collect();
        names.sort();
        let mut written = Vec::new();
        for name in names {
            let cfg = agent_map[name]
                .as_object()
                .ok_or_else(|| Error::Store(format!("agent {slug}/{name}: not an object")))?;
            let ctx = RenderCtx {
                project,
                role: entry_role(&agent.meta, name, slug),
                known_entries: &known,
                roots: roots.clone(),
            };
            let body = render_agent_file(cfg, &dir, &ctx)?;
            // The primary renders under its project handle; subagents keep
            // their bare keys (see `render_project_agents`).
            let dest = out_dir.join(format!("{}.md", rendered_name(&handles, slug, name)));
            atomic_write(&dest, body)?;
            written.push(dest);
        }
        Ok(written)
    }
}

/// The opencode identifier for every PRIMARY in a project: a name-derived
/// handle, unique within the project, keyed by the agent's dir slug.
///
/// opencode shows an agent by its rendered `.opencode/agent/<name>.md`
/// filename, so this handle — not the opaque dir slug (a UUID) — is what the
/// operator sees in the TUI and what a launch passes as `--agent`. An
/// UNNAMED agent (sidecar name empty or still equal to the slug) keeps the
/// slug: there is nothing friendlier to show.
///
/// PURE over the project's agent set, so the renderer (which writes the
/// files) and the launcher (which passes `--agent`) derive the SAME value
/// without sharing state. Names are not unique; when two agents' names
/// slugify to the same base, BOTH are disambiguated by appending a short
/// slice of their (unique) dir slug — so a launch is never blocked by a
/// name clash.
pub fn primary_handles(agents: &[(String, AgentConfig)]) -> BTreeMap<String, String> {
    let base: BTreeMap<String, String> = agents
        .iter()
        .map(|(slug, agent)| (slug.clone(), handle_base(&agent.meta.name, slug)))
        .collect();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for handle in base.values() {
        *counts.entry(handle.as_str()).or_default() += 1;
    }
    base.iter()
        .map(|(slug, handle)| {
            let resolved = if counts.get(handle.as_str()).copied().unwrap_or(0) > 1 {
                format!("{handle}-{}", slug_fragment(slug))
            } else {
                handle.clone()
            };
            (slug.clone(), resolved)
        })
        .collect()
}

/// The undisambiguated handle for one agent: its slugified name, or the dir
/// slug when unnamed (empty name, name == slug, or a name that slugifies to
/// nothing).
fn handle_base(name: &str, slug: &str) -> String {
    if name.is_empty() || name == slug {
        return slug.to_string();
    }
    let s = crate::store::slugify(name);
    if s.is_empty() {
        slug.to_string()
    } else {
        s
    }
}

/// A short, stable disambiguator from a dir slug: the first UUID segment
/// (8 hex), or the leading 8 chars of a non-UUID slug.
fn slug_fragment(slug: &str) -> String {
    slug.split('-')
        .next()
        .unwrap_or(slug)
        .chars()
        .take(8)
        .collect()
}

/// The rendered `.opencode/agent/<name>.md` stem for one agent-map entry:
/// the PRIMARY (entry key == dir slug) renders under its project handle;
/// every subagent renders under its own bare key.
fn rendered_name(handles: &BTreeMap<String, String>, slug: &str, entry: &str) -> String {
    if entry == slug {
        handles
            .get(slug)
            .cloned()
            .unwrap_or_else(|| slug.to_string())
    } else {
        entry.to_string()
    }
}

/// Claim every RENDERED entry name the project declares, refusing a
/// collision. Rendered names are FLAT across a project's agents (one
/// `.opencode/agent/<name>.md` each), so a collision would let one agent's
/// entry silently replace another's — different prompt, different role.
/// Same-named primaries are auto-disambiguated by `primary_handles`, so a
/// surviving collision here is a genuine authoring conflict (e.g. a
/// primary's handle equal to another agent's subagent name): refuse loudly
/// instead of letting slug order pick.
fn claim_entries(
    agents: &[(String, AgentConfig)],
    project: &str,
) -> Result<BTreeMap<String, String>> {
    let handles = primary_handles(agents);
    let mut claimed: BTreeMap<String, String> = BTreeMap::new();
    for (slug, agent) in agents {
        let Some(agent_map) = agent.doc.get("agent").and_then(|v| v.as_object()) else {
            continue;
        };
        let mut names: Vec<&String> = agent_map.keys().collect();
        names.sort();
        for name in names {
            let rname = rendered_name(&handles, slug, name);
            if let Some(owner) = claimed.get(&rname) {
                return Err(Error::Store(format!(
                    "agent entry {rname:?} is declared by both {owner:?} and {slug:?} in project \
                     {project} — rendered agent names are flat across a project; rename one"
                )));
            }
            claimed.insert(rname, slug.clone());
        }
    }
    Ok(claimed)
}

/// The entries an agent-map entry may delegate to: the `permission.task`
/// rule keys other than the `*` default whose action is not `deny`.
fn delegation_targets(cfg: &serde_json::Map<String, serde_json::Value>) -> Vec<String> {
    cfg.get("permission")
        .and_then(|p| p.get("task"))
        .and_then(|t| t.as_object())
        .map(|rules| {
            rules
                .iter()
                .filter(|(name, action)| name.as_str() != "*" && action.as_str() != Some("deny"))
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Refuse a set that is not closed under delegation. A `task:` allowlist
/// may name an entry belonging to ANOTHER of the project's agents — that
/// is legal, which is exactly why this cannot live in `validate_agent_doc`
/// (it sees one document and cannot know the set).
fn check_delegation_closure(
    agents: &[(String, AgentConfig)],
    claimed: &BTreeMap<String, String>,
    project: &str,
) -> Result<()> {
    for (slug, agent) in agents {
        let Some(agent_map) = agent.doc.get("agent").and_then(|v| v.as_object()) else {
            continue;
        };
        let mut names: Vec<&String> = agent_map.keys().collect();
        names.sort();
        for name in names {
            let Some(cfg) = agent_map[name].as_object() else {
                continue;
            };
            for target in delegation_targets(cfg) {
                if !claimed.contains_key(&target) {
                    return Err(Error::Store(format!(
                        "agent entry {name:?} (agent {slug:?}) delegates to {target:?}, which no \
                         agent in project {project} declares — a rendered agent set must be closed \
                         under delegation, or opencode resolves the name from config outside the \
                         run dir. Add the subagent or drop the task rule."
                    )));
                }
            }
        }
    }
    Ok(())
}

/// The role an agent-map ENTRY renders under. The primary carries the
/// sidecar's role; a subagent carries its own, capped by the primary's —
/// corpus-mcp cannot distinguish a subagent from its parent at runtime, so
/// a subagent must never render wider than the ceiling the server enforces
/// for the whole session.
pub(super) fn entry_role(meta: &AgentSidecar, entry: &str, slug: &str) -> AgentRole {
    if entry == slug {
        return meta.role();
    }
    match meta.subagent_roles.get(entry) {
        Some(sub) => sub.cap_under(meta.role()),
        None => meta.role(),
    }
}

/// Render one agent-map entry into the opencode agent-markdown body.
/// The render BINDS the agent to its project: `store/projects/*`
/// permission patterns are rewritten to the concrete project, a wildcard
/// read-allow gains the corpus boundary (other projects' corpora denied),
/// delegation is confined to the project's own entries, and a Corpus scope
/// section is appended naming the exact corpus dir — a rendered agent never
/// has to guess which project's corpus is home. Non-empty pins append the
/// launch's source pins too. The permission block is DERIVED from the role,
/// so it is emitted for every entry, including ones authored without a
/// `permission` key (silence must never mean allow).
fn render_agent_file(
    cfg: &serde_json::Map<String, serde_json::Value>,
    dir: &Path,
    ctx: &RenderCtx<'_>,
) -> Result<String> {
    let mut out = String::with_capacity(256);
    out.push_str("---\n");
    let description = cfg
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    out.push_str("description: ");
    out.push_str(&yaml_scalar(description));
    out.push('\n');
    let mode = cfg
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("primary");
    out.push_str("mode: ");
    out.push_str(&yaml_scalar(mode));
    out.push('\n');
    if let Some(model) = cfg
        .get("model")
        .and_then(|v| v.as_str())
        .filter(|m| !m.is_empty())
    {
        out.push_str("model: ");
        out.push_str(&yaml_scalar(model));
        out.push('\n');
    }
    if let Some(temperature) = cfg.get("temperature").and_then(|v| v.as_f64()) {
        out.push_str("temperature: ");
        out.push_str(&format!("{temperature}"));
        out.push('\n');
    }
    // ALWAYS emitted, even with no stored block: the role ceiling is the
    // point, and an entry without permissions would otherwise inherit
    // opencode's defaults.
    out.push_str("permission:\n");
    let stored = cfg
        .get("permission")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let bound = bind_permission(&stored, ctx);
    let yaml = crate::yaml::to_string(&canonical_json(&bound))
        .map_err(|e| Error::Store(format!("cannot serialize permission: {e}")))?;
    for line in yaml.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("---\n");
    if let Some(prompt) = cfg.get("prompt").and_then(|v| v.as_str()) {
        out.push_str(&inline_file_refs(dir, prompt)?);
    }
    out.push_str(&corpus_scope_section(ctx.project));
    out.push_str(pinned_sources_section(ctx.role));
    Ok(out)
}

/// The launch-bound orientation footer: which corpus is home, and the
/// project-boundary rule. Appended after the agent's own prompt so stale
/// prompt text (legacy flat-store paths) is overridden by recency.
fn corpus_scope_section(project: &str) -> String {
    format!(
        "\n---\n\n## Corpus scope (bound at launch)\n\n\
         You are bound to project `{project}`. Your corpus is\n\
         `store/projects/{project}/corpus/`. Read ONLY inside this project's\n\
         mounted corpus. Persist durable work with `entry_write`, using any\n\
         corpus-relative path that best represents the data. `runs/` is\n\
         immutable. Other projects' corpora are denied by\n\
         permissions and strictly off-limits: reading them pollutes the\n\
         project boundary. Any path in this prompt that names a corpus\n\
         category without the `store/projects/{project}/` prefix means\n\
         the one inside YOUR project corpus.\n"
    )
}

/// The source-pin instruction. CONSTANT, and that is the point.
///
/// This footer used to name the literal `sources/<name>/<sha>/` trees of
/// whichever launch rendered last — a per-RUN fact written into a file the
/// whole project shares. Two consequences, both bad: launching a second
/// mission rewrote the trees under a live one, and keeping them apart
/// meant a run directory per mission, duplicating opencode's
/// `node_modules` with it.
///
/// The pins already travel per-run as `CORPUS_SOURCE_PINS`, which
/// `start_tui` sets on the tmux session and `target_info` reports as an
/// exact `sources/<name>/<sha>` path per pin. So the FILE states the rule
/// and the TOOL states the facts — which is the same split
/// `write_run_opencode_config` already draws between project config and
/// run identity.
///
/// Rendered only for a role that can actually call `target_info`; a
/// curator manages the project and reads no source.
fn pinned_sources_section(role: AgentRole) -> &'static str {
    if !role.allows("target_info") {
        return "";
    }
    "\n---\n\n## Pinned sources\n\n\
     Call `target_info` before you read any source. It names the exact\n\
     `sources/<name>/<sha>/` trees THIS run is pinned to — read those\n\
     literal paths. Do NOT derive source paths from an ambient plugin manifest:\n\
     it records only the DEFAULT pin and may name a different (usually older)\n\
     tree. Verify every claim against the pinned trees; treat anything not\n\
     traced in them as unverified.\n"
}

/// Substitute every `{file:<rel>}` token in a prompt with the contents of
/// the referenced file under the agent dir.
fn inline_file_refs(dir: &Path, prompt: &str) -> Result<String> {
    let mut out = String::with_capacity(prompt.len());
    let mut rest = prompt;
    while let Some(start) = rest.find("{file:") {
        out.push_str(&rest[..start]);
        rest = &rest[start + 6..];
        let Some(end) = rest.find('}') else {
            return Err(Error::Store("unterminated {file:} ref".into()));
        };
        let rel = &rest[..end];
        rest = &rest[end + 1..];
        let path = resolve_prompt_ref(dir, rel)?;
        let body = fs::read_to_string(&path)
            .map_err(|e| Error::Store(format!("prompt ref {rel:?}: {e}")))?;
        out.push_str(&body);
    }
    out.push_str(rest);
    Ok(out)
}
