//! Project-scoped agents and their enforced role policy.
//!
//! An agent is a directory `store/projects/<p>/agents/<slug>/` wrapping an
//! opencode config:
//!
//! ```text
//! agent.yaml    corpus metadata sidecar (name, created, cloned_from)
//! opencode.json THE config — a schema-valid opencode document: $schema +
//!               "agent" map (primary [+ subagents]); model/description/
//!               prompt/permission/temperature per entry
//! prompts/*.md optional prompt bodies resolved by `{file:}` refs
//! ```
//!
//! `opencode.json` is consumed by opencode as-is (unknown top-level keys are
//! rejected by opencode's schema, so the document stays clean); corpus
//! metadata lives in the `agent.yaml` sidecar, never in the JSON.
//!
//! The renderer materializes a project's agents into the PROJECT's own
//! `.opencode/agent/<name>.md` (inside its run directory,
//! `store/projects/<p>/var/run/` — see `Store::provision_run_dir`) — one
//! file per `agent` map entry, frontmatter carrying description/mode/
//! model/temperature/permission and a body of the prompt with `{file:}`
//! refs inlined from the agent dir. The dir is corpus-managed: a launch
//! first clears the previous generated set, then renders EVERY agent of
//! the launched project, so the agent list opencode shows is scoped to
//! the project (and subagent names stay bare so the primary's `task:`
//! permission keys match verbatim). Every render BINDS the agent to its
//! project: `store/projects/*` permission patterns are rewritten to the
//! concrete project, wildcard read-allows gain the corpus boundary, the
//! trust red lines (`benchmarks/**`, `plugins/**` read denies) are
//! injected unconditionally, and a Corpus scope section names the exact
//! corpus dir — agents stay in their own project's corpus.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::store::{now_epoch, validate_slug, Store};

// ---------------------------------------------------------------------
// Roles
// ---------------------------------------------------------------------

/// What an agent is ALLOWED TO DO, as a first-class record rather than a
/// hand-maintained permission block.
///
/// Why this exists: an agent's `opencode.json` `permission` block is
/// enforced by opencode, not by us. corpus-mcp had no idea which agent was
/// calling, so it would run any tool for anyone opencode routed — the
/// block was the only dam, it is open-by-omission (add a tool, forget to
/// deny it), and `agent_save` can rewrite it wholesale. A role is stored in
/// the corpus sidecar (which no agent config can reach), resolved by the
/// server from the run's identity, and enforced there.
///
/// TRUST BOUNDARY: this is enforceable only for agents WITHOUT a host
/// shell. An agent holding `bash: allow` can re-exec corpus-mcp with a
/// forged identity, or edit the sidecar through the `store` symlink that
/// `provision_run_dir` creates. `bind_permission` force-denies `bash` for
/// any role that [`AgentRole::shell_would_defeat_gate`] flags, so a stored
/// grant cannot survive into the rendered artifact.
///
/// Deliberately NOT `Ord`. Super contains every current-project capability,
/// while Curator and Tester occupy different risk domains beneath it. That is
/// an authority relation, not something enum declaration order can encode.
/// [`AgentRole::cap_under`] spells the relation out instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Reads and curates; never executes. The contamination-sensitive
    /// role, and the one the server enforces hardest. Also the safe
    /// default for an agent whose intent we cannot infer.
    #[default]
    Researcher,
    /// Acts in the regtest arena: sandbox, oracles, faucet, findings.
    /// No open internet — execution turns must not pull in untrusted
    /// external text.
    Tester,
    /// Everything inside one project: research, penetration, management, and
    /// confirmation-gated destructive maintenance. Cross-project and project
    /// lifecycle administration remain operator-only.
    Super,
    /// Manages the PROJECT rather than the target: creates and edits the
    /// project's other agents, sets their roles, writes mission records,
    /// curates the corpus. Holds zero sandbox tools and no open internet. It
    /// may perform confirmation-gated agent, mission, and entry deletion, but
    /// cannot wipe the corpus or perform project-lifecycle operations.
    Curator,
}

/// The sandbox tool catalog, as the permission-block keys spell them
/// (`corpus_` + the MCP tool name). One list, so a tool added to
/// corpus-mcp without a role decision fails the totality test rather
/// than silently defaulting to allowed.
pub const CORPUS_TOOLS: [&str; 9] = [
    "corpus_target_info",
    "corpus_technique_save",
    "corpus_sandbox_exec",
    "corpus_oracle_list",
    "corpus_oracle_run",
    "corpus_faucet",
    "corpus_wallet_fund",
    "corpus_attack_save",
    "corpus_finding_write",
];

/// Tools a researcher may call: read the target, and write working notes.
/// Everything else is execution or publication.
const RESEARCHER_TOOLS: [&str; 2] = ["corpus_target_info", "corpus_technique_save"];

/// The project-management tool catalog, as MCP names (the rendered
/// permission keys are `corpus_` + these, because the run config names the
/// server `corpus`).
///
/// A separate namespace from [`CORPUS_TOOLS`]: these mutate the project's own
/// configuration rather than acting on a target. Super deliberately holds
/// both namespaces. The list is the scoped Curator catalog:
///
/// - `project_list/new/clone/delete/rebind` and `agent_copy` name a project
///   by a key other than `project`, so scope injection cannot reach them;
/// - project lifecycle and cross-project operations remain operator-only;
/// - Curator may delete agents, missions, and corpus entries inside the
///   injected project, but only Super may wipe that project's whole corpus.
pub const CURATOR_TOOLS: [&str; 28] = [
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_delete",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_await",
    "mission_new",
    "mission_launch",
    "mission_delete",
    "mission_set_budget",
    "mission_set_pins",
    "corpus_stats",
    "corpus_list",
    "corpus_read",
    "finding_list",
    "entry_delete",
    "entry_move",
    "entry_write",
    "model_list",
];

/// Super's project-management surface: Curator plus project-local corpus wipe.
/// Destructive calls still pass through scope injection, audit recording, and
/// the server's dry-run/token confirmation gate.
pub const SUPER_ADMIN_TOOLS: [&str; 29] = [
    "agent_list",
    "agent_get",
    "agent_new",
    "agent_save",
    "agent_clone",
    "agent_delete",
    "agent_set",
    "agent_set_role",
    "agent_set_permission",
    "agent_subagent_add",
    "agent_subagent_remove",
    "mission_list",
    "mission_get",
    "mission_status",
    "mission_await",
    "mission_new",
    "mission_launch",
    "mission_delete",
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

/// Every project-management permission on which a project role has an
/// opinion. Alias the widest scoped catalog so render rules cannot drift from
/// Super's server grant set.
const PROJECT_MANAGEMENT_TOOLS: [&str; 29] = SUPER_ADMIN_TOOLS;

impl AgentRole {
    /// Parse a role name (config, CLI flag, sidecar).
    ///
    /// Derived from [`AgentRole::as_str`], which is exhaustive, rather than
    /// written as its own `match` with a `_ => None` arm. That arm meant a
    /// new variant compiled fine and was then silently unparseable at every
    /// entry point — `--role`, the MCP schemas, the sidecar — with nothing
    /// but one round-trip assertion standing between that and a role nobody
    /// could actually select.
    pub fn parse(raw: &str) -> Option<Self> {
        let key = raw.trim().to_ascii_lowercase();
        Self::ALL.into_iter().find(|role| role.as_str() == key)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Researcher => "researcher",
            Self::Tester => "tester",
            Self::Super => "super",
            Self::Curator => "curator",
        }
    }

    /// The role names, for a usage line. Derived
    /// so a new variant reaches every CLI help string and error message
    /// without ten separate edits.
    pub fn names() -> String {
        Self::ALL
            .iter()
            .map(|role| role.as_str())
            .collect::<Vec<_>>()
            .join("|")
    }

    /// Every role in operator-facing authority/risk order. This is UI order,
    /// never legacy-inference precedence.
    pub const ALL: [Self; 4] = [Self::Super, Self::Curator, Self::Tester, Self::Researcher];

    /// Safest-covering-first order for legacy permission inference. Kept
    /// separate from [`AgentRole::ALL`] so changing picker order can never
    /// silently migrate an old agent to Super.
    const LEGACY_INFERENCE_ORDER: [Self; 4] =
        [Self::Researcher, Self::Tester, Self::Super, Self::Curator];

    /// The starting prompt for a new agent of this role.
    ///
    /// Compiled INTO the binary rather than copied from a seed directory
    /// in the store. Seeds were data pretending to be code: they shipped
    /// in the repo, drifted from the renderer, and gave every project two
    /// sources of truth for what a role may do — the seed's permission
    /// block and the role itself. Now the role is the only one, and the
    /// prompt is the only thing a seed was still contributing.
    ///
    /// Deliberately says nothing about WHICH corpus: the launch-bound
    /// "Corpus scope" footer names it, and a prompt that hardcoded a path
    /// would go stale the moment an agent was cloned into another project.
    pub fn default_prompt(self) -> &'static str {
        match self {
            Self::Researcher => include_str!("prompts/researcher.md"),
            Self::Tester => include_str!("prompts/tester.md"),
            Self::Super => include_str!("prompts/super.md"),
            Self::Curator => include_str!("prompts/curator.md"),
        }
    }

    /// The description a new agent of this role carries.
    pub fn default_description(self) -> &'static str {
        match self {
            Self::Researcher => {
                "Reads the corpus, the pinned source and the open internet; never executes. \
                 Produces cited hypotheses and technique cards."
            }
            Self::Tester => {
                "Runs adversarial missions against sandboxed targets through the corpus tools \
                 (sandbox, oracles, faucet, gated findings). No open internet."
            }
            Self::Super => {
                "Full authority inside this project: research, sandbox execution, corpus work, \
                 team and mission management, and confirmation-gated destructive maintenance."
            }
            Self::Curator => {
                "Manages this project: its agents, their roles, its missions and its corpus. \
                 Runs no missions itself."
            }
        }
    }

    /// One line on what the role means, for pickers and tooltips. Lives
    /// here rather than in the UI so the CLI, the app and the admin tools
    /// describe a role the same way.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Researcher => {
                "reads and curates: target_info + technique_save, plus the open internet. \
                 No execution — enforced by the corpus server, not just by config."
            }
            Self::Tester => {
                "acts in the regtest arena: sandbox, oracles, faucet, findings, attacks. \
                 No open internet, so an execution turn cannot pull in untrusted text."
            }
            Self::Super => {
                "all current-project capabilities: web research, sandbox execution, findings, \
                 project management, and confirmation-gated destructive maintenance."
            }
            Self::Curator => {
                "manages the project's agents, missions and corpus through the corpus server. \
                 No sandbox or open internet; corpus wipe and project lifecycle stay above it."
            }
        }
    }

    /// The `corpus_*` tools this role may call. THE source of truth: both
    /// the permission generator and the corpus-mcp gate read this, so they
    /// cannot drift apart.
    pub fn tools(self) -> &'static [&'static str] {
        match self {
            Self::Researcher => &RESEARCHER_TOOLS,
            // Tester and Super hold the same corpus tools by operator
            // decision; they differ in open-internet access, which is
            // opencode-enforced (see `grants_web`). The server-enforced
            // boundary is researcher-vs-rest.
            Self::Tester | Self::Super => &CORPUS_TOOLS,
            // Not a narrower research role \u{2014} a different job. Its grants
            // are in `admin_tools`.
            Self::Curator => &[],
        }
    }

    /// The project-management tools this role may call. Kept apart from
    /// [`AgentRole::tools`] because one acts on the target and the other on
    /// project configuration. Super deliberately receives both catalogs.
    pub fn admin_tools(self) -> &'static [&'static str] {
        match self {
            Self::Super => &SUPER_ADMIN_TOOLS,
            Self::Curator => &CURATOR_TOOLS,
            Self::Researcher | Self::Tester => &[],
        }
    }

    /// May this role call `tool` (a bare MCP name like `sandbox_exec` or a
    /// permission key like `corpus_sandbox_exec`)?
    pub fn allows(self, tool: &str) -> bool {
        let key = if tool.starts_with("corpus_") {
            tool.to_string()
        } else {
            format!("corpus_{tool}")
        };
        self.tools().contains(&key.as_str())
    }

    /// Open-internet access. NOT server-enforced — `webfetch`/`websearch`
    /// are opencode's own tools and never reach corpus-mcp — so this is
    /// rendered into the permission block and honoured by opencode only.
    pub fn grants_web(self) -> bool {
        matches!(self, Self::Researcher | Self::Super)
    }

    /// Would granting a host shell to this role make its tool ceiling a
    /// fiction? All project roles are server-scoped, including Super: a host
    /// shell could forge a different project identity and escape that scope.
    pub fn shell_would_defeat_gate(self) -> bool {
        true
    }

    /// The role a SUBAGENT entry may render under, given the ceiling the
    /// server will enforce for the whole session — the primary's, because
    /// corpus-mcp resolves exactly one role per session from
    /// `CORPUS_OPENCODE_AGENT`, and that names the primary.
    ///
    /// Deliberately not a `min` over a derived ordering. Researcher ⊆
    /// Tester ⊆ Super holds over the corpus-tool sets, so it is written out
    /// here as three arms; the moment a role's grants live in a different
    /// namespace, "which is smaller" stops having an answer and a derived
    /// `Ord` would return whichever variant happened to be declared first —
    /// a number, not a decision.
    pub fn cap_under(self, primary: Self) -> Self {
        match (primary, self) {
            (a, b) if a == b => a,
            // Super contains every current-project role, so any requested
            // subagent role remains intact under it.
            (Self::Super, sub) => sub,
            // A curator's session is curated end to end: the server resolves
            // ONE role per session and takes it from the primary, so a
            // subagent rendered as a researcher would carry
            // `corpus_target_info: allow` into a session whose server
            // refuses that tool.
            (Self::Curator, _) => Self::Curator,
            // Non-Super research sessions cannot serve management tools.
            (_, Self::Curator) => Self::Researcher,
            // A tester's session cannot serve a wider ceiling than its own.
            (Self::Tester, Self::Super) => Self::Tester,
            (Self::Tester, sub) => sub,
            // The narrowest primary caps everything to itself.
            (Self::Researcher, _) => Self::Researcher,
        }
    }
}

/// Infer the role an existing agent entry is ALREADY operating under, by
/// reading what its permission block actually grants. Used to migrate
/// legacy agents (written before roles existed) without changing their
/// behaviour: the result is the SMALLEST role that still covers everything
/// the config allows, so nothing silently loses a capability it had.
///
/// Faithful to opencode's semantics, which is why this can over-grant:
/// a key that is absent is ALLOWED, and an entry with no permission block
/// at all is allowed everything — so it infers `Super`. That is an
/// authoring accident far more often than an intent, which is why
/// migration reports every inference for review instead of applying
/// silently.
pub fn infer_role(cfg: &serde_json::Map<String, serde_json::Value>) -> AgentRole {
    let Some(perm) = cfg.get("permission").and_then(|p| p.as_object()) else {
        // No block: opencode allows everything.
        return AgentRole::Super;
    };
    // "deny"/"ask" withhold the tool; "allow" or ABSENT grant it.
    let granted = |tool: &str| {
        !matches!(
            perm.get(tool).and_then(|v| v.as_str()),
            Some("deny") | Some("ask")
        )
    };
    let wants_web = ["webfetch", "websearch"].iter().any(|k| granted(k));
    let needed: Vec<&str> = CORPUS_TOOLS.into_iter().filter(|t| granted(t)).collect();
    AgentRole::LEGACY_INFERENCE_ORDER
        .into_iter()
        .find(|role| needed.iter().all(|t| role.allows(t)) && (!wants_web || role.grants_web()))
        .unwrap_or(AgentRole::Super)
}

/// The corpus metadata sidecar (`agent.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSidecar {
    pub name: String,
    #[serde(default)]
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
    /// The PRIMARY agent's role — the capability ceiling corpus-mcp
    /// enforces for any mission launched as this agent. Lives here, out of
    /// `opencode.json`, so no agent config (and no `agent_save`) can raise
    /// it.
    ///
    /// `None` means NEVER ASSIGNED (a sidecar written before roles existed)
    /// and reads as the safest role. It is deliberately distinguishable
    /// from an explicit `role: researcher`, so `corpus agent migrate-roles`
    /// can be re-run without silently re-inferring — and re-widening — an
    /// agent the operator had tightened by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<AgentRole>,
    /// Per-subagent roles, keyed by the subagent's entry name. Capped by
    /// the primary's role: corpus-mcp cannot tell a subagent from its
    /// parent at runtime (one MCP server serves the whole opencode
    /// session), so these shape the rendered permission block and are
    /// enforced by opencode, never by the server.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub subagent_roles: std::collections::BTreeMap<String, AgentRole>,
    /// When this agent was last changed, and by whom
    /// (`curator:<slug>` / `operator`).
    ///
    /// `None` means NEVER RECORDED — a sidecar written before provenance
    /// existed — and is deliberately distinguishable from an explicit
    /// `modified_by: operator`, the same way `role: None` is
    /// distinguishable from an explicit researcher. An agent that a curator
    /// built should be readable as such from its own sidecar, without
    /// consulting the log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified_by: Option<String>,
}

impl AgentSidecar {
    /// The effective role: an unassigned sidecar reads as the SAFEST role,
    /// never as a permissive default — a capability ceiling must not be
    /// widened by a missing field.
    pub fn role(&self) -> AgentRole {
        self.role.unwrap_or_default()
    }

    /// Has a role ever been assigned? False for sidecars predating roles.
    pub fn has_role(&self) -> bool {
        self.role.is_some()
    }
}

/// One agent's row in a role migration: what it holds now, what its
/// permissions imply, and whether the run wrote anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMigration {
    pub agent: String,
    /// `None` = never assigned (the rows a migration is actually for).
    pub current: Option<AgentRole>,
    pub inferred: AgentRole,
    pub applied: bool,
    /// The inference came from an ABSENT permission block (opencode treats
    /// silence as allow), so `Super` here is a guess worth eyeballing.
    pub needs_review: bool,
}

/// A loaded agent: the sidecar metadata plus the parsed opencode.json doc.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub meta: AgentSidecar,
    pub doc: serde_json::Value,
}

/// The OpenCode config schema reference written into generated agent documents.
pub const OPENCODE_SCHEMA: &str = "https://opencode.ai/config.json";

/// The placeholder display name a freshly created agent carries until the
/// operator renames it. Deliberately not the slug, so the UI shows it as an
/// editable label.
pub const DEFAULT_AGENT_NAME: &str = "new agent";

impl Store {
    // -----------------------------------------------------------------
    // Seeds
    // -----------------------------------------------------------------

    /// Create an agent from a ROLE — the only way a new agent comes into
    /// being.
    ///
    /// The document carries description, mode and prompt and NO permission
    /// block: the render derives permissions from the role every time. That
    /// is the whole point of dropping the seed directories — a seed shipped
    /// its own permission block, so every agent had two sources of truth
    /// about what it could do, and the stored one was the one nobody
    /// regenerated.
    pub fn create_agent_with_role(&self, project: &str, slug: &str, role: AgentRole) -> Result<()> {
        validate_slug(slug)?;
        // The project must already exist. `create_dir_all` below would
        // otherwise materialize `projects/<typo>/agents/<slug>/` — a
        // project directory with no `project.yaml`, which every later
        // check reads as "no such project" while the agent sits inside it.
        crate::store::Project::load(self, project)?;
        let dir = self.project_agent_dir(project, slug);
        if dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent already exists: {project}/{slug}"
            )));
        }
        fs::create_dir_all(&dir)?;
        let doc = serde_json::json!({
            "$schema": OPENCODE_SCHEMA,
            "agent": {
                slug: {
                    "description": role.default_description(),
                    "mode": "primary",
                    "prompt": role.default_prompt(),
                }
            }
        });
        write_agent_doc(&dir, &doc)?;
        // Name-neutral at the core: a fresh agent is "unnamed" (name == slug)
        // until something names it. The app's create flow stamps the
        // `DEFAULT_AGENT_NAME` placeholder; keeping it out of here means a
        // core-created agent renders under its slug, not a shared placeholder
        // handle, and the render/role tests stay independent of UI defaults.
        write_sidecar(&dir, slug, None, role, Default::default(), self.actor())?;
        Ok(())
    }

    // -----------------------------------------------------------------
    // CRUD
    // -----------------------------------------------------------------

    /// List a project's agents, sorted by slug.
    pub fn list_agents(&self, project: &str) -> Result<Vec<(String, AgentConfig)>> {
        let mut found = Vec::new();
        let dir = self.project_agents_dir(project);
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() || !path.join("opencode.json").is_file() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Store("non-utf8 agent dir".into()))?;
            if let Ok(agent) = self.load_agent(project, slug) {
                found.push((slug.to_string(), agent));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Load an agent: sidecar + parsed opencode.json.
    pub fn load_agent(&self, project: &str, slug: &str) -> Result<AgentConfig> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let meta = read_sidecar(&dir, slug);
        let raw = fs::read_to_string(dir.join("opencode.json"))
            .map_err(|e| Error::Store(format!("agent {project}/{slug}: {e}")))?;
        let doc = serde_json::from_str(&raw).map_err(|e| {
            Error::Store(format!(
                "agent {project}/{slug}: invalid opencode.json: {e}"
            ))
        })?;
        Ok(AgentConfig { meta, doc })
    }

    /// Validate and save an agent's opencode.json. The document must parse,
    /// hold a non-empty `agent` map with exactly one primary, valid
    /// permission blocks, and resolvable `{file:}` prompt refs. A rejected
    /// document is never written.
    pub fn save_agent(&self, project: &str, slug: &str, doc: &serde_json::Value) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent not found: {project}/{slug} — create it first with agent_new or agent_clone"
            )));
        }
        validate_agent_doc(doc, &dir).map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        let pretty = serde_json::to_string_pretty(doc)?;
        fs::write(dir.join("opencode.json"), pretty)?;
        Ok(())
    }

    /// Create an agent from STRUCTURED content (the management chat's
    /// `agent_new`) — the server builds the opencode.json; the caller never
    /// hand-writes nested JSON (the depbot-session failure mode: the model
    /// serialized the document as a string, twice).
    ///
    /// With `from`, the new agent starts from an existing project agent's
    /// tree (permissions/prompts inherited — "a researcher like X but…")
    /// with description/prompt/model overlaid and the primary key renamed
    /// to `slug`. Without, a minimal doc: description + mode + prompt [+
    /// model], no permission block.
    pub fn create_agent(
        &self,
        project: &str,
        slug: &str,
        description: &str,
        prompt: &str,
        model: Option<&str>,
        from: Option<&str>,
        role: Option<AgentRole>,
    ) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent already exists: {project}/{slug}"
            )));
        }
        fs::create_dir_all(&dir)?;

        let mut cfg = serde_json::Map::new();
        if let Some(from) = from {
            let source = self.project_agent_dir(project, from);
            let src_doc_path = source.join("opencode.json");
            if !src_doc_path.is_file() {
                return Err(Error::Store(format!(
                    "agent not found: {project}/{from} — 'from' must name an existing agent in this project"
                )));
            }
            copy_tree(&source, &dir)?;
            let raw = fs::read_to_string(&src_doc_path)?;
            let doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
                Error::Store(format!(
                    "agent {project}/{from}: invalid opencode.json: {e}"
                ))
            })?;
            // A blank base (seed-less stores) has no primary — nothing to
            // inherit, start from empty.
            cfg = primary_agent_cfg(&doc, project, from).unwrap_or_default();
        }
        cfg.insert("description".into(), description.into());
        cfg.insert("mode".into(), "primary".into());
        if !prompt.is_empty() {
            cfg.insert("prompt".into(), prompt.into());
        }
        if let Some(model) = model {
            cfg.insert("model".into(), model.into());
        }
        // An explicit role wins. Otherwise: with `from`, the inherited
        // permission block decides; without, there is no block, so start at
        // the safest role rather than letting silence mean allow.
        let role = role.unwrap_or_else(|| {
            if from.is_some() {
                infer_role(&cfg)
            } else {
                AgentRole::Researcher
            }
        });
        let doc = serde_json::json!({
            "$schema": OPENCODE_SCHEMA,
            "agent": { slug: cfg },
        });
        validate_agent_doc(&doc, &dir).map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        fs::write(
            dir.join("opencode.json"),
            serde_json::to_string_pretty(&doc)?,
        )?;
        write_sidecar(&dir, slug, from, role, Default::default(), self.actor())?;
        Ok(())
    }

    /// Clone an agent (opencode.json + prompts); the sidecar records the
    /// source, and the config hash is recomputed from the copy. The primary
    /// agent key is renamed to the new slug — a verbatim copy left the old
    /// name inside the new dir (`agent_get depbot` answered with a
    /// "researcher" doc; the depbot session, 2026-08-14).
    pub fn clone_agent(&self, project: &str, from: &str, to: &str) -> Result<()> {
        validate_slug(to)?;
        let source = self.project_agent_dir(project, from);
        if !source.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent not found: {project}/{from} — 'from' must name an existing agent in this project (see agent_list)"
            )));
        }
        let dest = self.project_agent_dir(project, to);
        if dest.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent already exists: {project}/{to}"
            )));
        }
        fs::create_dir_all(&dest)?;
        copy_tree(&source, &dest)?;
        // Rename the primary key (dir slug == opencode name for the clone)
        // while KEEPING every subagent entry: the old code cleared the whole
        // map and reinserted only the primary, silently dropping a clone's
        // subagents along with the `task:` rules that name them.
        let raw = fs::read_to_string(dest.join("opencode.json"))?;
        let mut doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            Error::Store(format!(
                "agent {project}/{from}: invalid opencode.json: {e}"
            ))
        })?;
        if let Ok(cfg) = primary_agent_cfg(&doc, project, from) {
            if let Some(agents) = doc.get_mut("agent").and_then(|a| a.as_object_mut()) {
                // Drop only the OLD primary entry, whatever it was named.
                let old_primary: Vec<String> = agents
                    .iter()
                    .filter(|(_, c)| {
                        c.get("mode").and_then(|m| m.as_str()).unwrap_or("primary") == "primary"
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for name in old_primary {
                    agents.remove(&name);
                }
                agents.insert(to.to_string(), serde_json::Value::Object(cfg));
            }
            fs::write(
                dest.join("opencode.json"),
                serde_json::to_string_pretty(&doc)?,
            )?;
        }
        // A clone INHERITS its source's role: cloning is an operator act on
        // an agent that already holds those powers, so it grants nothing new
        // — and silently downgrading would break "copy this agent" in a way
        // that only shows up as a refused tool mid-mission.
        let src_meta = read_sidecar(&source, from);
        write_sidecar(
            &dest,
            to,
            Some(from),
            src_meta.role(),
            src_meta.subagent_roles,
            self.actor(),
        )?;
        Ok(())
    }

    /// Edit ONE field of one agent-map entry, leaving the rest of the
    /// document byte-identical.
    ///
    /// Why this exists: the only write path used to be `agent_save`, which
    /// takes the WHOLE document — so changing a model meant a model
    /// re-emitting every nested prompt and permission map verbatim. A local
    /// 27B model burned ~25k tokens on exactly that and still failed. Here
    /// the server does the read-modify-write; the caller sends one value.
    ///
    /// `entry` names the map key (the primary is the dir slug); `None`
    /// targets the primary. `value` of `Null` REMOVES the field.
    pub fn set_agent_field(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        field: &str,
        value: serde_json::Value,
    ) -> Result<()> {
        // Structural keys are owned by the document's invariants (exactly
        // one primary; entry name == dir slug) — they are not fields.
        if matches!(field, "mode" | "permission") {
            return Err(Error::Store(format!(
                "{field:?} is not settable here: use set_agent_role / set_agent_permission \
                 (mode is fixed by the entry's position in the document)"
            )));
        }
        let mut config = self.load_agent(project, slug)?;
        let target = entry.unwrap_or(slug).to_string();
        let dir = self.project_agent_dir(project, slug);
        let agents = config
            .doc
            .get_mut("agent")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing agent map")))?;
        let cfg = agents
            .get_mut(&target)
            .and_then(|c| c.as_object_mut())
            .ok_or_else(|| {
                Error::Store(format!(
                    "agent {project}/{slug} has no entry named {target:?}"
                ))
            })?;
        if value.is_null() {
            cfg.remove(field);
        } else {
            cfg.insert(field.to_string(), value);
        }
        validate_agent_doc(&config.doc, &dir)
            .map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        fs::write(
            dir.join("opencode.json"),
            serde_json::to_string_pretty(&config.doc)?,
        )?;
        Ok(())
    }

    /// Merge a permission PATCH into one entry's block (top-level keys
    /// replace; a `null` value removes). Never a wholesale replace, so a
    /// caller changing one rule cannot drop the rest by omission.
    ///
    /// Note the ceiling still wins at render: a patch granting a
    /// `corpus_*` tool outside the agent's role renders as `deny`.
    pub fn patch_agent_permission(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        patch: &serde_json::Value,
    ) -> Result<()> {
        let patch = patch
            .as_object()
            .ok_or_else(|| Error::Store("permission patch must be an object".into()))?;
        let mut config = self.load_agent(project, slug)?;
        let target = entry.unwrap_or(slug).to_string();
        let dir = self.project_agent_dir(project, slug);
        let agents = config
            .doc
            .get_mut("agent")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing agent map")))?;
        let cfg = agents
            .get_mut(&target)
            .and_then(|c| c.as_object_mut())
            .ok_or_else(|| {
                Error::Store(format!(
                    "agent {project}/{slug} has no entry named {target:?}"
                ))
            })?;
        let mut block = cfg
            .get("permission")
            .and_then(|p| p.as_object())
            .cloned()
            .unwrap_or_default();
        for (key, value) in patch {
            if value.is_null() {
                block.remove(key);
            } else {
                block.insert(key.clone(), value.clone());
            }
        }
        cfg.insert("permission".into(), serde_json::Value::Object(block));
        validate_agent_doc(&config.doc, &dir)
            .map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        fs::write(
            dir.join("opencode.json"),
            serde_json::to_string_pretty(&config.doc)?,
        )?;
        Ok(())
    }

    /// Add a subagent entry to an agent's document, and allow the primary
    /// to delegate to it (`task: {<name>: allow}`) — the two halves are
    /// useless apart, so they are one operation.
    pub fn add_subagent(
        &self,
        project: &str,
        slug: &str,
        name: &str,
        description: &str,
        prompt: &str,
        model: Option<&str>,
        role: Option<AgentRole>,
    ) -> Result<()> {
        validate_slug(name)?;
        if name == slug {
            return Err(Error::Store(format!(
                "subagent {name:?} would collide with its primary"
            )));
        }
        let mut config = self.load_agent(project, slug)?;
        let dir = self.project_agent_dir(project, slug);
        let agents = config
            .doc
            .get_mut("agent")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing agent map")))?;
        if agents.contains_key(name) {
            return Err(Error::Store(format!(
                "agent {project}/{slug} already has an entry named {name:?}"
            )));
        }
        let mut cfg = serde_json::Map::new();
        cfg.insert("description".into(), description.into());
        cfg.insert("mode".into(), "subagent".into());
        if !prompt.is_empty() {
            cfg.insert("prompt".into(), prompt.into());
        }
        if let Some(model) = model {
            cfg.insert("model".into(), model.into());
        }
        agents.insert(name.to_string(), serde_json::Value::Object(cfg));
        // Let the primary delegate to it, without widening `task` generally.
        if let Some(primary) = agents.get_mut(slug).and_then(|c| c.as_object_mut()) {
            let mut block = primary
                .get("permission")
                .and_then(|p| p.as_object())
                .cloned()
                .unwrap_or_default();
            let mut task = match block.get("task") {
                Some(serde_json::Value::Object(existing)) => existing.clone(),
                // A scalar `task` becomes a rule map keyed by that action.
                Some(serde_json::Value::String(action)) => {
                    let mut m = serde_json::Map::new();
                    m.insert("*".into(), action.clone().into());
                    m
                }
                _ => {
                    let mut m = serde_json::Map::new();
                    m.insert("*".into(), "deny".into());
                    m
                }
            };
            task.insert(name.to_string(), "allow".into());
            block.insert("task".into(), serde_json::Value::Object(task));
            primary.insert("permission".into(), serde_json::Value::Object(block));
        }
        validate_agent_doc(&config.doc, &dir)
            .map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        fs::write(
            dir.join("opencode.json"),
            serde_json::to_string_pretty(&config.doc)?,
        )?;
        if let Some(role) = role {
            self.set_subagent_role(project, slug, name, role)?;
        }
        Ok(())
    }

    /// Remove a subagent entry, its `task:` allow, and its sidecar role —
    /// leaving no dangling delegation rule pointing at a missing entry.
    pub fn remove_subagent(&self, project: &str, slug: &str, name: &str) -> Result<()> {
        let mut config = self.load_agent(project, slug)?;
        let dir = self.project_agent_dir(project, slug);
        let agents = config
            .doc
            .get_mut("agent")
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing agent map")))?;
        let removed = agents.remove(name);
        if removed.is_none() {
            return Err(Error::Store(format!(
                "agent {project}/{slug} has no entry named {name:?}"
            )));
        }
        if let Some(primary) = agents.get_mut(slug).and_then(|c| c.as_object_mut()) {
            if let Some(task) = primary
                .get_mut("permission")
                .and_then(|p| p.as_object_mut())
                .and_then(|b| b.get_mut("task"))
                .and_then(|t| t.as_object_mut())
            {
                task.remove(name);
            }
        }
        validate_agent_doc(&config.doc, &dir)
            .map_err(|e| Error::Store(format!("agent {slug}: {e}")))?;
        fs::write(
            dir.join("opencode.json"),
            serde_json::to_string_pretty(&config.doc)?,
        )?;
        let mut meta = read_sidecar(&dir, slug);
        if meta.subagent_roles.remove(name).is_some() {
            self.stamp_and_write_sidecar(&dir, &mut meta)?;
        }
        Ok(())
    }

    /// Copy an agent BETWEEN PROJECTS (or within one). The gap that made
    /// "copy these agents into the new project" impossible: `clone_agent`
    /// resolves `from` and `to` in a single project, so a cross-project
    /// copy could not be expressed at all and the management chat burned a
    /// whole session failing to work around it.
    ///
    /// Carries the whole tree — prompts, subagents, and the role.
    pub fn copy_agent(
        &self,
        from_project: &str,
        from: &str,
        to_project: &str,
        to: &str,
    ) -> Result<()> {
        validate_slug(to)?;
        let source = self.project_agent_dir(from_project, from);
        if !source.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent not found: {from_project}/{from} (see agent_list)"
            )));
        }
        if !self.project_dir(to_project).is_dir() {
            return Err(Error::Store(format!("project not found: {to_project}")));
        }
        let dest = self.project_agent_dir(to_project, to);
        if dest.join("opencode.json").is_file() {
            return Err(Error::Store(format!(
                "agent already exists: {to_project}/{to}"
            )));
        }
        fs::create_dir_all(&dest)?;
        copy_tree(&source, &dest)?;
        // Same primary-key rename as a same-project clone, subagents kept.
        let raw = fs::read_to_string(dest.join("opencode.json"))?;
        let mut doc: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
            Error::Store(format!(
                "agent {from_project}/{from}: invalid opencode.json: {e}"
            ))
        })?;
        if let Ok(cfg) = primary_agent_cfg(&doc, from_project, from) {
            if let Some(agents) = doc.get_mut("agent").and_then(|a| a.as_object_mut()) {
                let old: Vec<String> = agents
                    .iter()
                    .filter(|(_, c)| {
                        c.get("mode").and_then(|m| m.as_str()).unwrap_or("primary") == "primary"
                    })
                    .map(|(k, _)| k.clone())
                    .collect();
                for name in old {
                    agents.remove(&name);
                }
                agents.insert(to.to_string(), serde_json::Value::Object(cfg));
            }
            fs::write(
                dest.join("opencode.json"),
                serde_json::to_string_pretty(&doc)?,
            )?;
        }
        let src_meta = read_sidecar(&source, from);
        write_sidecar(
            &dest,
            to,
            Some(&format!("{from_project}/{from}")),
            src_meta.role(),
            src_meta.subagent_roles,
            self.actor(),
        )?;
        Ok(())
    }

    /// Set an agent's display name (the sidecar `name`; the slug — its
    /// identity in every path — is untouched). An empty name falls back to
    /// the slug, so the label is never blank. Preserves the rest of the
    /// sidecar (role, created, provenance).
    pub fn set_agent_name(&self, project: &str, slug: &str, name: &str) -> Result<()> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let mut meta = read_sidecar(&dir, slug);
        let name = name.trim();
        meta.name = if name.is_empty() {
            slug.to_string()
        } else {
            name.to_string()
        };
        self.stamp_and_write_sidecar(&dir, &mut meta)?;
        Ok(())
    }

    /// Set an agent's role — the capability ceiling corpus-mcp enforces.
    /// Preserves the rest of the sidecar (name, created, provenance).
    pub fn set_agent_role(&self, project: &str, slug: &str, role: AgentRole) -> Result<()> {
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        let mut meta = read_sidecar(&dir, slug);
        meta.role = Some(role);
        self.stamp_and_write_sidecar(&dir, &mut meta)?;
        Ok(())
    }

    /// Set a SUBAGENT's role (capped by the primary's at render time).
    /// An unknown entry name is refused — a typo would otherwise sit in
    /// the sidecar doing nothing.
    pub fn set_subagent_role(
        &self,
        project: &str,
        slug: &str,
        subagent: &str,
        role: AgentRole,
    ) -> Result<()> {
        let config = self.load_agent(project, slug)?;
        let known = config
            .doc
            .get("agent")
            .and_then(|a| a.as_object())
            .is_some_and(|m| m.contains_key(subagent));
        if !known {
            return Err(Error::Store(format!(
                "agent {project}/{slug} has no entry named {subagent:?}"
            )));
        }
        let dir = self.project_agent_dir(project, slug);
        let mut meta = read_sidecar(&dir, slug);
        // A Curator primary cannot serve research tools, and a non-Super
        // research primary cannot serve management tools. Super is the union
        // role and may host any project role. The server resolves ONE role per
        // run, from the primary, so every other cross-domain pairing would
        // render an entry advertising tools its own session refuses.
        // `cap_under` already collapses it at render time; refusing here
        // means the operator finds out now instead of wondering why a
        // subagent lost its grants at launch.
        let primary = meta.role();
        let incompatible = (primary == AgentRole::Curator && role != AgentRole::Curator)
            || (role == AgentRole::Curator
                && !matches!(primary, AgentRole::Curator | AgentRole::Super));
        if incompatible {
            return Err(Error::Store(format!(
                "agent {project}/{slug} is a {} and cannot hold a {} subagent — one role is \
                 enforced per session, taken from the primary, so this would render as {}",
                primary.as_str(),
                role.as_str(),
                role.cap_under(primary).as_str()
            )));
        }
        meta.subagent_roles.insert(subagent.to_string(), role);
        self.stamp_and_write_sidecar(&dir, &mut meta)?;
        Ok(())
    }

    /// Assign roles to agents that predate the role system, INFERRING each
    /// from what its permission block already grants so nothing changes
    /// what it can do. Reports every agent (including ones already
    /// assigned, marked `applied: false`) so the operator can review before
    /// committing; `apply: false` is a dry run.
    ///
    /// Idempotent: an agent that already carries an explicit role is left
    /// alone, so re-running can never re-widen a hand-tightened agent.
    pub fn migrate_agent_roles(&self, project: &str, apply: bool) -> Result<Vec<RoleMigration>> {
        let mut out = Vec::new();
        for (slug, config) in self.list_agents(project)? {
            let already = config.meta.has_role();
            let inferred = config
                .doc
                .get("agent")
                .and_then(|a| a.as_object())
                .and_then(|m| {
                    m.iter()
                        .find(|(name, cfg)| {
                            **name == slug
                                || cfg
                                    .get("mode")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("primary")
                                    == "primary"
                        })
                        .and_then(|(_, cfg)| cfg.as_object())
                        .map(infer_role)
                })
                .unwrap_or(AgentRole::Researcher);
            // An entry with no permission block at all infers Super purely
            // because opencode treats silence as allow — far more often an
            // authoring accident than an intent. Flag it for review.
            let silent = config
                .doc
                .get("agent")
                .and_then(|a| a.as_object())
                .and_then(|m| m.get(&slug))
                .map(|cfg| cfg.get("permission").is_none())
                .unwrap_or(false);
            if apply && !already {
                self.set_agent_role(project, &slug, inferred)?;
            }
            out.push(RoleMigration {
                agent: slug,
                current: config.meta.role,
                inferred,
                applied: apply && !already,
                needs_review: silent && inferred == AgentRole::Super,
            });
        }
        Ok(out)
    }

    /// Delete an agent directory.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_agent_dir(project, slug);
        if !dir.join("opencode.json").is_file() {
            return Err(Error::Store(format!("agent not found: {project}/{slug}")));
        }
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// The agent's config hash: FNV-1a over the opencode.json bytes, hex.
    /// Recorded in every run transcript as the agent's provenance.
    pub fn agent_config_hash(&self, project: &str, slug: &str) -> Result<String> {
        let path = self.project_agent_dir(project, slug).join("opencode.json");
        let bytes = fs::read(&path)
            .map_err(|_| Error::Store(format!("agent not found: {project}/{slug}")))?;
        Ok(crate::store::fnv1a_hex(&bytes))
    }

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
                fs::write(&dest, body)?;
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
            fs::write(&dest, body)?;
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
fn entry_role(meta: &AgentSidecar, entry: &str, slug: &str) -> AgentRole {
    if entry == slug {
        return meta.role();
    }
    match meta.subagent_roles.get(entry) {
        Some(sub) => sub.cap_under(meta.role()),
        None => meta.role(),
    }
}

/// A mission's source pin as the renderer shows it: the repo name, the rev
/// label the operator picked, and the RESOLVED sha — the literal tree the
/// research-zone agents are to read (`sources/<name>/<sha>/`). A launch
/// with no pins renders no section (the plugin's defaults then apply).
#[derive(Debug, Clone)]
pub struct SourcePin {
    /// Repository name as the plugin declares it (`cdk`, `nuts`).
    pub name: String,
    /// The rev label the mission pins (`v0.17.0`, `main`).
    pub rev: String,
    /// The commit sha the rev resolved to at launch — the tree under
    /// `sources/<name>/<sha>/`.
    pub sha: String,
}

/// What a render binds an entry to, beyond the entry's own config.
struct RenderCtx<'a> {
    /// The project the rendered agent is bound to.
    project: &'a str,
    /// This entry's capability ceiling.
    role: AgentRole,
    /// Every entry name the project declares — the delegation universe.
    /// A `task:` allow outside it is force-denied, so the artifact cannot
    /// point opencode at an agent the run dir does not contain.
    known_entries: &'a BTreeSet<String>,
    /// The absolute data roots, denied by path. The run cwd's relative
    /// patterns describe only what the run dir links; these close the
    /// absolute route to everything it doesn't.
    roots: DataRoots,
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
    let yaml = serde_yaml::to_string(&canonical_json(&bound))
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
         `store/projects/{project}/corpus/` — categories: `hypotheses/`,\n\
         `techniques/`, `findings/`, `attacks/`, `runs/`. Read and write\n\
         ONLY inside it. Other projects' corpora are denied by\n\
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

/// One permission decision. Ordered by tightness, so a merge that takes
/// the MAXIMUM moves a ceiling in the only direction a stored document is
/// allowed to move it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Action {
    Allow,
    Ask,
    Deny,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }

    /// Read a stored value. `None` if it is not a string at all; anything
    /// that IS a string but not one of opencode's three words reads as
    /// `Deny` — a permission we cannot interpret is not one we can honour.
    /// The predecessor copied such a string into the artifact verbatim and
    /// left opencode to decide what `"maybe"` meant.
    fn parse(value: &serde_json::Value) -> Option<Self> {
        match value.as_str()? {
            "allow" => Some(Self::Allow),
            "ask" => Some(Self::Ask),
            _ => Some(Self::Deny),
        }
    }
}

/// A glob rule family (`read`/`edit`/`write`/`task`): pattern -> action.
///
/// Sorted, because `canonical_json` sorts the block before it is written
/// and opencode evaluates last-match-wins over that order — so a narrow
/// rule only beats the broad one it refines when it sorts AFTER it. Every
/// pattern injected below extends the prefix of the rule it must beat,
/// which is what makes that hold.
type Rules = BTreeMap<String, Action>;

fn rules<const N: usize>(entries: [(&str, Action); N]) -> Rules {
    entries
        .into_iter()
        .map(|(pattern, action)| (pattern.to_string(), action))
        .collect()
}

/// The rendered permission block as a VALUE: computed once from the role
/// and the stored document, then serialized.
///
/// The predecessor derived the same block by mutating a single string-keyed
/// JSON map through nine stages. Every failure that design actually shipped
/// was a rule that silently did not land, and each was invisible in the
/// same way — nothing was wrong with the stage, the stage simply had
/// nowhere to write:
///
/// - a scalar `read: "allow"` is not a map, so the red lines injected into
///   `read` went into a value that discarded them;
/// - agents built from a role carry no permission block at all, so entries
///   whose rules were only ever DEFAULTED rendered with no path rules;
/// - `bash` was defaulted where it meant to be forced, so a stored
///   `"bash": "allow"` survived every render — against a module doc that
///   had claimed the opposite since roles landed.
///
/// A field cannot be absent. That is the whole reason this is a struct:
/// the compiler now asks the question each of those bugs answered wrongly.
struct Policy {
    read: Rules,
    edit: Rules,
    write: Rules,
    /// Delegation, confined to entries the project actually declares.
    /// `render_project_agents` already refuses a dangling name outright;
    /// this keeps the ARTIFACT safe on any path that renders without it.
    task: Rules,
    bash: Action,
    webfetch: Action,
    websearch: Action,
    /// Reaching outside the run dir at all. The run cwd exposes exactly
    /// one project by construction; this is the switch deciding whether
    /// that construction can be stepped around.
    external_directory: Action,
    /// The `corpus_*` switches: 8 research tools and 26 management ones,
    /// every one written explicitly so the artifact never leans on
    /// omission-means-allow. Three come out `corpus_corpus_*` because the
    /// run config names the MCP server `corpus`.
    tools: BTreeMap<String, Action>,
    /// Keys this type does not model — opencode's own `glob`, `grep`,
    /// `list`, and whatever a later version adds. Carried through
    /// unchanged: the render binds and tightens, and silently dropping a
    /// setting it fails to recognize is neither.
    passthrough: serde_json::Map<String, serde_json::Value>,
}

/// Bind a permission document to a concrete project AND a role at render
/// time. The rendered artifact — not the stored JSON — is what opencode
/// obeys, so deriving here means role and document can never contradict in
/// the dangerous direction, however the stored block was edited.
fn bind_permission(permission: &serde_json::Value, ctx: &RenderCtx<'_>) -> serde_json::Value {
    Policy::build(permission, ctx).into_json()
}

impl Policy {
    fn build(permission: &serde_json::Value, ctx: &RenderCtx<'_>) -> Self {
        let role = ctx.role;
        // The project rewrite runs ONCE, over the stored document only.
        // Everything injected below is written with the concrete project
        // already in it — except two deliberate `store/projects/*`
        // wildcards, which mean all projects and must survive as such.
        let mut stored = match bind_project(permission, ctx.project) {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };

        let corpus = format!("store/projects/{}/corpus/**", ctx.project);
        let runs = format!("store/projects/{}/corpus/runs/**", ctx.project);
        // `runs/` sits inside the corpus but is not an agent's to change:
        // those transcripts are what technique cards cite by name, what the
        // cost report counts, and the only provenance a mission leaves.
        let mutable_default = || {
            rules([
                ("*", Action::Deny),
                (corpus.as_str(), Action::Allow),
                (runs.as_str(), Action::Deny),
            ])
        };
        let mut read =
            take_rules(&mut stored, "read").unwrap_or_else(|| rules([("*", Action::Allow)]));
        let mut edit = take_rules(&mut stored, "edit").unwrap_or_else(mutable_default);
        let mut write = take_rules(&mut stored, "write").unwrap_or_else(mutable_default);
        seal_readable(&mut read, ctx);
        seal_mutable(&mut edit, ctx);
        seal_mutable(&mut write, ctx);

        let mut task = take_rules(&mut stored, "task").unwrap_or_default();
        // Omission would inherit opencode's default and let a dangling name
        // resolve against config discovered outside the run dir — the leak
        // that sent one project's scout at another's corpus.
        task.entry("*".to_string()).or_insert(Action::Deny);
        for (name, action) in task.iter_mut() {
            if name != "*" && !ctx.known_entries.contains(name.as_str()) {
                *action = Action::Deny;
            }
        }

        let mut tools = BTreeMap::new();
        for tool in CORPUS_TOOLS {
            let stored = take_action(&mut stored, tool);
            tools.insert(tool.to_string(), ceiling(role.allows(tool), stored));
        }
        let granted = role.admin_tools();
        for tool in PROJECT_MANAGEMENT_TOOLS {
            let key = format!("corpus_{tool}");
            let stored = take_action(&mut stored, &key);
            tools.insert(key, ceiling(granted.contains(&tool), stored));
        }

        // Web is opencode-enforced; the role decides whether to offer it.
        // Written either way — a rendered file must never depend on
        // opencode's default for a capability the role has an opinion on.
        let offered = match role.grants_web() {
            true => Action::Allow,
            false => Action::Deny,
        };
        let web = |stored: Option<Action>| match stored {
            Some(Action::Deny) => Action::Deny,
            _ => offered,
        };
        let webfetch = web(take_action(&mut stored, "webfetch"));
        let websearch = web(take_action(&mut stored, "websearch"));

        // A host shell defeats the whole gate for any role the server
        // restricts: it re-execs corpus-mcp with a forged
        // `CORPUS_OPENCODE_AGENT`, or edits the sidecar through the `store`
        // link the run dir provides.
        let stored_bash = take_action(&mut stored, "bash");
        let bash = match role.shell_would_defeat_gate() {
            true => Action::Deny,
            false => stored_bash.unwrap_or(Action::Deny),
        };

        // Written for EVERY role, `super` included. The exemption that used
        // to skip the key for `super` was meant to let the unrestricted
        // role step outside — but an absent key hands the decision to
        // opencode's default, which denies. So the exemption delivered
        // nothing except an artifact that did not say what it meant.
        let _ = take_action(&mut stored, "external_directory");
        let external_directory = Action::Deny;

        Self {
            read,
            edit,
            write,
            task,
            bash,
            webfetch,
            websearch,
            external_directory,
            tools,
            passthrough: stored,
        }
    }

    fn into_json(self) -> serde_json::Value {
        let mut out = self.passthrough;
        for (key, family) in [
            ("read", self.read),
            ("edit", self.edit),
            ("write", self.write),
            ("task", self.task),
        ] {
            let rules = family
                .into_iter()
                .map(|(pattern, action)| (pattern, scalar(action)))
                .collect();
            out.insert(key.to_string(), serde_json::Value::Object(rules));
        }
        for (key, action) in [
            ("bash", self.bash),
            ("webfetch", self.webfetch),
            ("websearch", self.websearch),
            ("external_directory", self.external_directory),
        ] {
            out.insert(key.to_string(), scalar(action));
        }
        for (tool, action) in self.tools {
            out.insert(tool, scalar(action));
        }
        serde_json::Value::Object(out)
    }
}

fn scalar(action: Action) -> serde_json::Value {
    serde_json::Value::String(action.as_str().to_string())
}

/// The ceiling merge. Outside the role's grant a tool is denied whatever
/// the document said; inside it, a stored `deny`/`ask` TIGHTENS and is
/// kept, so hand-tightening an agent works and hand-widening one does not.
fn ceiling(granted: bool, stored: Option<Action>) -> Action {
    match granted {
        false => Action::Deny,
        true => stored.unwrap_or(Action::Allow),
    }
}

/// Pull a rule family out of the stored document, normalizing a bare
/// scalar to `{"*": scalar}` so the red lines have somewhere to land.
fn take_rules(stored: &mut serde_json::Map<String, serde_json::Value>, key: &str) -> Option<Rules> {
    let value = stored.remove(key)?;
    let mut out = Rules::new();
    match &value {
        serde_json::Value::Object(map) => {
            for (pattern, action) in map {
                if let Some(action) = Action::parse(action) {
                    out.insert(pattern.clone(), action);
                }
            }
        }
        other => {
            if let Some(action) = Action::parse(other) {
                out.insert("*".to_string(), action);
            }
        }
    }
    Some(out)
}

fn take_action(
    stored: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Option<Action> {
    Action::parse(&stored.remove(key)?)
}

/// Emit a string as a YAML scalar that survives a round trip.
///
/// The frontmatter used to interpolate these raw, so any description
/// containing `": "` produced a file whose frontmatter does not parse —
/// and opencode reads the permission block out of that frontmatter, so the
/// whole role gate on the opencode side went with it. The `super` role's
/// own description tripped this, silently, for every agent rendered under
/// it. Quoting is delegated to serde_yaml rather than hand-rolled: a value
/// that needs no quotes still renders bare, so the artifacts of every
/// colon-free description are byte-identical to before.
fn yaml_scalar(value: &str) -> String {
    serde_yaml::to_string(value)
        .map(|s| s.trim_end().to_string())
        .unwrap_or_else(|_| format!("{value:?}"))
}

/// Rewrite `store/projects/*` to the concrete project in every key, at
/// every depth of the stored document.
///
/// Applied to the STORED document only. The wildcards this render injects
/// itself (`store/projects/*` in `read`, `store/projects/*/agents/**` in
/// `edit`/`write`) mean every project and must stay wildcards — which is
/// why binding runs first and injection second, rather than the two being
/// interleaved as they were when one function did both.
fn bind_project(value: &serde_json::Value, project: &str) -> serde_json::Value {
    let serde_json::Value::Object(map) = value else {
        return value.clone();
    };
    let bound = format!("store/projects/{project}");
    serde_json::Value::Object(
        map.iter()
            .map(|(key, value)| {
                (
                    key.replace("store/projects/*", &bound),
                    bind_project(value, project),
                )
            })
            .collect(),
    )
}

/// The red lines on READING, injected rather than trusted.
fn seal_readable(rules: &mut Rules, ctx: &RenderCtx<'_>) {
    // Contamination rule: the answer key and harness internals stay
    // unreadable even if edited out of a config.
    for red in ["benchmarks/**", "plugins/**"] {
        rules.entry(red.to_string()).or_insert(Action::Deny);
    }
    // The project boundary, RELATIVE — what the run cwd exposes. Narrowed
    // to the corpus and mission records: the project's `agents/` holds the
    // sidecars this gate trusts and `var/` its chat scope, neither of which
    // is research material. Only applied to a document that opened
    // everything and drew no boundary of its own.
    let opens_everything = rules.get("*") == Some(&Action::Allow);
    let has_boundary = rules.keys().any(|k| k.starts_with("store/projects/"));
    if opens_everything && !has_boundary {
        rules.insert("store/projects/*".to_string(), Action::Deny);
        for allowed in ["corpus", "missions"] {
            rules.insert(
                format!("store/projects/{}/{allowed}/**", ctx.project),
                Action::Allow,
            );
        }
    }
    seal_data_roots(rules, ctx, false);
}

/// The red lines on WRITING. The agent tree holds the role sidecars this
/// gate trusts, and the run cwd links the project — no agent writes there.
fn seal_mutable(rules: &mut Rules, ctx: &RenderCtx<'_>) {
    rules.insert("store/projects/*/agents/**".to_string(), Action::Deny);
    seal_data_roots(rules, ctx, true);
}

/// Deny the data roots by ABSOLUTE path, then re-allow exactly one
/// project's corpus.
///
/// The relative patterns above describe the run cwd and say nothing about
/// `/Users/…/.corpus/store/...`; the run dir links only one project, so an
/// absolute path is the one route left to name another's corpus.
///
/// Ordering matters and is not incidental: the block is sorted before it is
/// written and opencode evaluates last-match-wins, so the narrow allow must
/// sort AFTER the broad deny. `<data>/**` < `<data>/store/projects/<p>/…`
/// holds because the allow extends the deny's prefix — every allow emitted
/// here must keep that property.
fn seal_data_roots(rules: &mut Rules, ctx: &RenderCtx<'_>, mutating: bool) {
    let roots = &ctx.roots;
    if !roots.data.is_empty() {
        let data = roots.data.trim_end_matches('/');
        // Denied by name rather than by denying the whole data root: the
        // agent's own run dir lives under that root too, and a blanket deny
        // would take its cwd (and the `sources` link) with it if opencode
        // resolves paths before matching.
        //
        // `chat` is the operator's notes across every project. `audit` and
        // `refusals` are the records OF this agent — deny-by-default
        // already covers them for writes, but a subject editing its own
        // entry is the one failure those logs cannot tolerate, so it is
        // stated rather than inferred. Reading them is denied too: between
        // them they are a map of every gate and the exact wording that
        // trips it.
        for log in ["audit", "chat", "refusals"] {
            rules.insert(format!("{data}/var/{log}/**"), Action::Deny);
        }
    }
    if roots.store.is_empty() {
        return;
    }
    let store = roots.store.trim_end_matches('/');
    rules.insert(format!("{store}/**"), Action::Deny);
    rules.insert(
        format!("{store}/projects/{}/corpus/**", ctx.project),
        Action::Allow,
    );
    // The absolute half of the `runs/` rule. Reading a transcript is fine —
    // an agent may want its own — but changing one is not.
    if mutating {
        rules.insert(
            format!("{store}/projects/{}/corpus/runs/**", ctx.project),
            Action::Deny,
        );
    }
}

/// The absolute roots a render denies by path. Held as strings because
/// they only ever become permission-rule keys.
#[derive(Debug, Clone, Default)]
struct DataRoots {
    /// Everything the operator owns (`~/.corpus`) — chat scopes and run
    /// dirs included, not just the store.
    data: String,
    /// The store root, whose one allowed project subtree is re-opened.
    store: String,
}

impl DataRoots {
    /// Derived from the STORE, never from the environment: a render must
    /// produce the same bytes for the same store regardless of what
    /// `CORPUS_STORE` happens to say in this process. The store's parent
    /// is denied too — that is where `var/run` and `var/chat` live, so the
    /// deny covers run dirs and management-chat transcripts as well.
    fn for_store(store: &Store) -> Self {
        Self {
            data: store
                .root()
                .parent()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default(),
            store: store.root().to_string_lossy().into_owned(),
        }
    }
}

/// Recursively sort object keys so rendered bytes are identical no matter
/// how feature unification ordered serde_json's map (`preserve_order`
/// leaks in via sibling deps; without this, which binary rendered last
/// decides the byte order and the checked-in agent files flip-flop).
fn canonical_json(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(&String, &serde_json::Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical_json(v)))
                    .collect(),
            )
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical_json).collect())
        }
        other => other.clone(),
    }
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
        let path = dir.join(rel);
        let body = fs::read_to_string(&path)
            .map_err(|e| Error::Store(format!("prompt ref {rel:?}: {e}")))?;
        out.push_str(&body);
    }
    out.push_str(rest);
    Ok(out)
}

/// Validate an agent opencode.json document. JSON already-parsed; checks the
/// structural rules the plan mandates before a save is allowed.
/// A JSON value's kind, for teaching error messages ("got a string — pass
/// the object itself").
fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// The PRIMARY agent's config object from an opencode.json document (exactly
/// one primary is required — the same rule the validator enforces). Used by
/// the create/clone paths to inherit a base config.
fn primary_agent_cfg(
    doc: &serde_json::Value,
    project: &str,
    slug: &str,
) -> Result<serde_json::Map<String, serde_json::Value>> {
    let agents = doc
        .get("agent")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::Store(format!("agent {project}/{slug}: missing \"agent\" map")))?;
    let primary = agents.iter().find(|(_, cfg)| {
        cfg.get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("primary")
            == "primary"
    });
    let Some((_, cfg)) = primary else {
        return Err(Error::Store(format!(
            "agent {project}/{slug}: no primary agent in opencode.json"
        )));
    };
    Ok(cfg.as_object().cloned().unwrap_or_default())
}

fn validate_agent_doc(doc: &serde_json::Value, dir: &Path) -> Result<()> {
    let obj = doc.as_object().ok_or_else(|| {
        Error::Store(format!(
            "opencode.json must be a JSON object, got {} — pass the object itself, not its string serialization",
            json_kind(doc)
        ))
    })?;
    let agents = obj
        .get("agent")
        .and_then(|v| v.as_object())
        .ok_or_else(|| Error::Store("missing \"agent\" map".into()))?;
    if agents.is_empty() {
        return Err(Error::Store("agent map is empty".into()));
    }
    let primaries = agents
        .values()
        .filter(|cfg| {
            cfg.get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("primary")
                == "primary"
        })
        .count();
    if primaries != 1 {
        return Err(Error::Store("exactly one primary agent is required".into()));
    }
    for (name, cfg_value) in agents {
        let cfg = cfg_value
            .as_object()
            .ok_or_else(|| Error::Store(format!("agent {name}: must be an object")))?;
        if let Some(permission) = cfg.get("permission") {
            validate_permission(permission)
                .map_err(|e| Error::Store(format!("agent {name}: {e}")))?;
        }
        if let Some(prompt) = cfg.get("prompt").and_then(|v| v.as_str()) {
            // Every {file:} ref must resolve against the agent dir.
            let mut rest = prompt;
            while let Some(start) = rest.find("{file:") {
                rest = &rest[start + 6..];
                let Some(end) = rest.find('}') else {
                    return Err(Error::Store(format!(
                        "agent {name}: unterminated {{file:}} ref"
                    )));
                };
                let rel = &rest[..end];
                if !dir.join(rel).is_file() {
                    return Err(Error::Store(format!(
                        "agent {name}: {{file:{rel}}} does not resolve against the agent dir"
                    )));
                }
                rest = &rest[end + 1..];
            }
        }
    }
    Ok(())
}

/// A permission block is valid when it is either a plain scalar (ask/allow/
/// deny) or an object (rule map) whose values are all valid actions.
fn validate_permission(permission: &serde_json::Value) -> Result<()> {
    fn ok_action(action: &str) -> bool {
        ["ask", "allow", "deny"].contains(&action)
    }
    match permission {
        serde_json::Value::String(action) if ok_action(action) => Ok(()),
        serde_json::Value::String(action) => Err(Error::Store(format!(
            "invalid permission action {action:?} (ask|allow|deny)"
        ))),
        serde_json::Value::Object(map) => {
            for (_, value) in map {
                validate_permission(value)?;
            }
            Ok(())
        }
        _ => Err(Error::Store(
            "permission must be an action or a rule map".into(),
        )),
    }
}

/// The blank opencode.json: a schema-valid config with an empty agent map.
fn write_agent_doc(dir: &Path, doc: &serde_json::Value) -> Result<()> {
    fs::write(
        dir.join("opencode.json"),
        serde_json::to_string_pretty(doc)?,
    )?;
    Ok(())
}

/// Write the sidecar. The role is EXPLICIT because this runs after
/// `copy_tree` on the create/clone paths and would otherwise silently
/// reset a copied `agent.yaml` back to the default — turning a clone of a
/// `super` agent into a `researcher` (or worse, the reverse) with no
/// diagnostic.
fn write_sidecar(
    dir: &Path,
    name: &str,
    cloned_from: Option<&str>,
    role: AgentRole,
    subagent_roles: std::collections::BTreeMap<String, AgentRole>,
    actor: &str,
) -> Result<()> {
    let sidecar = AgentSidecar {
        name: name.to_string(),
        created: now_epoch(),
        cloned_from: cloned_from.map(str::to_string),
        // Always written explicitly: a newly created agent has a decided
        // role, not an inherited absence.
        role: Some(role),
        subagent_roles,
        modified: Some(now_epoch()),
        modified_by: Some(actor.to_string()),
    };
    fs::write(dir.join("agent.yaml"), serde_yaml::to_string(&sidecar)?)?;
    Ok(())
}

impl Store {
    /// Write a sidecar back, stamping who changed it and when. EVERY
    /// in-place sidecar rewrite goes through here, so a future mutator
    /// cannot forget provenance by writing the file itself — which is the
    /// failure mode that made "the operator can audit it" untrue for the
    /// whole life of the role system.
    fn stamp_and_write_sidecar(&self, dir: &Path, meta: &mut AgentSidecar) -> Result<()> {
        meta.modified = Some(now_epoch());
        meta.modified_by = Some(self.actor().to_string());
        fs::write(dir.join("agent.yaml"), serde_yaml::to_string(meta)?)?;
        Ok(())
    }
}

/// Read the sidecar, falling back to the SAFEST role when it is missing or
/// unparseable — a capability ceiling must never be widened by a damaged
/// file. `corpus agents migrate-roles` assigns real roles to legacy agents.
fn read_sidecar(dir: &Path, slug: &str) -> AgentSidecar {
    fs::read_to_string(dir.join("agent.yaml"))
        .ok()
        .and_then(|raw| serde_yaml::from_str(&raw).ok())
        .unwrap_or(AgentSidecar {
            name: slug.to_string(),
            created: 0,
            cloned_from: None,
            // Unreadable/absent sidecar: NEVER assigned, so it reads as
            // the safest role via `role()` and migration can still see it.
            role: None,
            subagent_roles: std::collections::BTreeMap::new(),
            modified: None,
            modified_by: None,
        })
}

/// Recursively copy a directory tree (agent and prompt directories).
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        fs::create_dir_all(dst)?;
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store in its own world — see the note in `launch::tests`: run
    /// dirs are siblings of the store, so each test store needs its own
    /// parent or they share `<parent>/var/run/<project>`.
    fn tmp_store(tag: &str) -> Store {
        let world =
            std::env::temp_dir().join(format!("corpus-agents-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    fn doc(agent: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "$schema": OPENCODE_SCHEMA, "agent": agent })
    }

    /// Parse the rendered frontmatter's permission block.
    fn rendered_permission(text: &str) -> serde_yaml::Value {
        let fm = text.split("---\n").nth(1).unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(fm).unwrap();
        yaml["permission"].clone()
    }

    /// Every corpus tool is classified by every role — a tool added to
    /// corpus-mcp without a role decision must fail here, not silently
    /// default to allowed somewhere.
    #[test]
    fn every_corpus_tool_is_classified_by_every_role() {
        for role in AgentRole::ALL {
            for tool in CORPUS_TOOLS {
                // `allows` accepts both the bare and the corpus_-prefixed
                // spelling; they must agree.
                let bare = tool.strip_prefix("corpus_").unwrap();
                assert_eq!(
                    role.allows(tool),
                    role.allows(bare),
                    "{role:?} disagrees about {tool} vs {bare}"
                );
            }
            // Roles form a chain: researcher ⊆ tester ⊆ super.
            for tool in role.tools() {
                assert!(
                    AgentRole::Super.allows(tool),
                    "super must cover {tool} held by {role:?}"
                );
            }
        }
        assert!(AgentRole::Researcher.tools().len() < CORPUS_TOOLS.len());
        assert_eq!(AgentRole::Super.tools().len(), CORPUS_TOOLS.len());
        // Every role is current-project scoped; a host shell could forge a
        // different project even when the role already holds every local tool.
        for role in AgentRole::ALL {
            assert!(role.shell_would_defeat_gate(), "{role:?}");
        }
        // Round-trip every name.
        for role in AgentRole::ALL {
            assert_eq!(AgentRole::parse(role.as_str()), Some(role));
        }
        assert_eq!(AgentRole::parse("root"), None);
    }

    /// The render DERIVES the permission block from the role, so a stored
    /// block that grants beyond the role cannot take effect — this is the
    /// property the whole role system rests on.
    #[test]
    fn render_denies_corpus_tools_outside_the_role() {
        let store = tmp_store("role-deny");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        // The stored doc tries to grant EVERYTHING.
        let mut perm = serde_json::Map::new();
        for tool in CORPUS_TOOLS {
            perm.insert(tool.to_string(), "allow".into());
        }
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": { "mode": "primary", "permission": perm },
                })),
            )
            .unwrap();
        // ...but the sidecar says researcher (the role it was created with).
        let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
        let perm = rendered_permission(&text);
        assert_eq!(perm["corpus_target_info"].as_str(), Some("allow"));
        assert_eq!(perm["corpus_technique_save"].as_str(), Some("allow"));
        for denied in [
            "corpus_sandbox_exec",
            "corpus_oracle_list",
            "corpus_oracle_run",
            "corpus_faucet",
            "corpus_wallet_fund",
            "corpus_attack_save",
            "corpus_finding_write",
        ] {
            assert_eq!(
                perm[denied].as_str(),
                Some("deny"),
                "a stored allow must not raise a researcher's ceiling: {denied}\n{text}"
            );
        }
    }

    /// Hand-tightening still works (deny-wins), and an entry authored with
    /// NO permission block still gets a full explicit block — silence must
    /// never mean allow in the rendered artifact.
    #[test]
    fn render_keeps_tightening_and_never_relies_on_omission() {
        let store = tmp_store("role-tighten");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    // No permission key at all.
                    "a": { "mode": "primary", "prompt": "hi" },
                })),
            )
            .unwrap();
        let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
        let perm = rendered_permission(&text);
        for tool in CORPUS_TOOLS {
            assert!(
                perm[tool].as_str().is_some(),
                "{tool} must be written explicitly even with no stored block\n{text}"
            );
        }
    }

    /// A scalar `read`/`edit`/`write` used to skip red-line injection
    /// entirely; it must be normalized to a map so the denies always land.
    /// The agent tree itself is unwritable — it holds the role sidecars.
    #[test]
    fn red_lines_survive_scalar_permissions() {
        let store = tmp_store("role-scalar");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": {
                        "mode": "primary",
                        // Scalars, the shape that bypassed injection.
                        "permission": { "read": "allow", "write": "allow" },
                    },
                })),
            )
            .unwrap();
        let text = fs::read_to_string(&store.render_project_agents("alpha").unwrap()[0]).unwrap();
        let perm = rendered_permission(&text);
        assert_eq!(
            perm["read"]["benchmarks/**"].as_str(),
            Some("deny"),
            "{text}"
        );
        assert_eq!(perm["read"]["plugins/**"].as_str(), Some("deny"), "{text}");
        assert_eq!(
            perm["write"]["store/projects/*/agents/**"].as_str(),
            Some("deny"),
            "no agent may rewrite the sidecars the role gate trusts\n{text}"
        );
    }

    /// Inference reproduces the seeds' current behaviour, so migrating a
    /// legacy agent changes nothing about what it can do.
    #[test]
    fn infer_role_matches_the_seed_permissions() {
        // The researcher seed: only target_info + technique_save allowed.
        let researcher = serde_json::json!({
            "permission": {
                "corpus_sandbox_exec": "deny", "corpus_faucet": "deny",
                "corpus_wallet_fund": "deny", "corpus_oracle_run": "deny",
                "corpus_oracle_list": "deny",
                "corpus_finding_write": "deny", "corpus_attack_save": "deny",
                "corpus_target_info": "allow", "corpus_technique_save": "allow",
                "webfetch": "allow", "websearch": "allow",
            }
        });
        assert_eq!(
            infer_role(researcher.as_object().unwrap()),
            AgentRole::Researcher
        );
        // The operator seed names no corpus_* key: silence means allow,
        // so it infers the full role rather than silently losing powers.
        let operator = serde_json::json!({ "permission": { "bash": "deny", "read": "deny" } });
        assert_eq!(infer_role(operator.as_object().unwrap()), AgentRole::Super);
        // No block at all: opencode allows everything.
        let bare = serde_json::json!({ "mode": "primary" });
        assert_eq!(infer_role(bare.as_object().unwrap()), AgentRole::Super);
    }

    /// A subagent can be narrower than its parent but never wider — the
    /// server cannot tell them apart at runtime, so the parent's ceiling
    /// binds the whole session.
    #[test]
    fn subagent_role_is_capped_by_the_primary() {
        let mut meta = AgentSidecar {
            name: "discover".into(),
            created: 0,
            cloned_from: None,
            role: Some(AgentRole::Researcher),
            subagent_roles: Default::default(),
            modified: None,
            modified_by: None,
        };
        meta.subagent_roles.insert("scout".into(), AgentRole::Super);
        assert_eq!(
            entry_role(&meta, "scout", "discover"),
            AgentRole::Researcher,
            "a subagent must not exceed its parent"
        );
        meta.role = Some(AgentRole::Super);
        meta.subagent_roles
            .insert("scout".into(), AgentRole::Researcher);
        assert_eq!(
            entry_role(&meta, "scout", "discover"),
            AgentRole::Researcher,
            "a narrower subagent stays narrow"
        );
        assert_eq!(entry_role(&meta, "discover", "discover"), AgentRole::Super);
    }

    /// The subagent cap, as a table rather than as a consequence of
    /// declaration order. It used to be `min` over a derived `Ord`, so the
    /// answer for any pair was decided by where the variants happened to sit
    /// in the enum body; this pins the relation the roles actually have, so
    /// that adding a role that is NOT part of the chain shows up here as a
    /// diff to read rather than as a number that silently changed.
    #[test]
    fn cap_under_reproduces_the_research_chain() {
        use AgentRole::{Researcher, Super, Tester};
        let table = [
            // (primary, subagent, capped)
            (Researcher, Researcher, Researcher),
            (Researcher, Tester, Researcher),
            (Researcher, Super, Researcher),
            (Tester, Researcher, Researcher),
            (Tester, Tester, Tester),
            (Tester, Super, Tester),
            (Super, Researcher, Researcher),
            (Super, Tester, Tester),
            (Super, Super, Super),
        ];
        for (primary, sub, want) in table {
            assert_eq!(
                sub.cap_under(primary),
                want,
                "{:?} under {:?}",
                sub.as_str(),
                primary.as_str()
            );
        }
        // The property the table encodes: a subagent never renders wider
        // than the session its parent's role bought.
        for primary in AgentRole::ALL {
            for sub in AgentRole::ALL {
                let capped = sub.cap_under(primary);
                for tool in capped.tools() {
                    assert!(
                        primary.allows(tool),
                        "{} under {} kept {tool}, which the primary cannot call",
                        sub.as_str(),
                        primary.as_str()
                    );
                }
            }
        }
    }

    /// Curator and Tester remain different domains, but Super contains both.
    #[test]
    fn super_contains_curator_while_other_research_roles_do_not() {
        use AgentRole::{Curator, Researcher, Super, Tester};
        assert_eq!(Curator.cap_under(Curator), Curator);
        // A curator primary curates its whole session.
        for sub in [Researcher, Tester, Super] {
            assert_eq!(
                sub.cap_under(Curator),
                Curator,
                "{} under a curator",
                sub.as_str()
            );
        }
        // Curator remains incompatible with the narrower research roles.
        for primary in [Researcher, Tester] {
            assert_eq!(
                Curator.cap_under(primary),
                Researcher,
                "a curator under {}",
                primary.as_str()
            );
        }
        assert_eq!(Curator.cap_under(Super), Curator);
        // The invariant the whole table exists to protect, restated over the
        // management namespace this time.
        for primary in AgentRole::ALL {
            for sub in AgentRole::ALL {
                for tool in sub.cap_under(primary).admin_tools() {
                    assert!(
                        primary.admin_tools().contains(tool),
                        "{} under {} kept {tool}, which the primary cannot call",
                        sub.as_str(),
                        primary.as_str()
                    );
                }
            }
        }
    }

    /// Picker order and legacy inference order are independent. Curator grants
    /// no corpus tools and would match empty requirements if it moved earlier
    /// in inference, while putting Super first would widen every migration.
    #[test]
    fn a_new_role_cannot_relabel_legacy_agents() {
        assert_eq!(
            AgentRole::ALL,
            [
                AgentRole::Super,
                AgentRole::Curator,
                AgentRole::Tester,
                AgentRole::Researcher
            ],
            "operator-facing authority/risk order"
        );
        assert_eq!(
            AgentRole::LEGACY_INFERENCE_ORDER[0],
            AgentRole::Researcher,
            "legacy inference stays safest-covering-first"
        );
        assert_eq!(
            AgentRole::LEGACY_INFERENCE_ORDER.last().copied(),
            Some(AgentRole::Curator),
            "a zero-corpus-tool role must infer last"
        );
        // An agent with no permission block at all, and one that grants
        // nothing: neither is a curator, whatever the tool arithmetic says.
        for doc in [
            serde_json::json!({}),
            serde_json::json!({ "permission": { "bash": "deny" } }),
        ] {
            let cfg = doc.as_object().unwrap().clone();
            assert_ne!(
                infer_role(&cfg),
                AgentRole::Curator,
                "inference must never invent a curator: {doc}"
            );
        }
    }

    /// The module doc has promised since the roles landed that a shell and a
    /// restricted role cannot coexist. It was never enforced: `bash: deny`
    /// was only a DEFAULT, so a stored allow rode straight through the
    /// render — and a shell re-execs corpus-mcp with a forged identity.
    #[test]
    fn a_stored_bash_allow_cannot_survive_a_restricted_role() {
        let store = tmp_store("bash-hole");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        for (slug, role) in [
            ("res", AgentRole::Researcher),
            ("cur", AgentRole::Curator),
            ("sup", AgentRole::Super),
        ] {
            store.create_agent_with_role("alpha", slug, role).unwrap();
            store
                .save_agent(
                    "alpha",
                    slug,
                    &doc(serde_json::json!({
                        slug: { "mode": "primary", "permission": { "bash": "allow" } },
                    })),
                )
                .unwrap();
        }
        store.render_project_agents("alpha").unwrap();
        let read = |slug: &str| {
            fs::read_to_string(store.opencode_agent_dir("alpha").join(format!("{slug}.md")))
                .unwrap()
        };
        for slug in ["res", "cur", "sup"] {
            assert_eq!(
                rendered_permission(&read(slug))["bash"].as_str(),
                Some("deny"),
                "{slug}: a server-restricted role cannot hold a shell"
            );
        }
    }

    /// The management namespace is written for every role, denied wherever
    /// it is not granted — the same discipline the corpus tools get, so the
    /// artifact never depends on opencode's default for a tool the role has
    /// an opinion about.
    #[test]
    fn render_derives_the_admin_namespace_for_curator_and_super() {
        let store = tmp_store("admin-ns");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "res", AgentRole::Researcher)
            .unwrap();
        store
            .create_agent_with_role("alpha", "cur", AgentRole::Curator)
            .unwrap();
        store
            .create_agent_with_role("alpha", "sup", AgentRole::Super)
            .unwrap();
        store.render_project_agents("alpha").unwrap();
        let read = |slug: &str| {
            rendered_permission(
                &fs::read_to_string(store.opencode_agent_dir("alpha").join(format!("{slug}.md")))
                    .unwrap(),
            )
        };
        let (res, cur, sup) = (read("res"), read("cur"), read("sup"));
        for tool in PROJECT_MANAGEMENT_TOOLS {
            let key = format!("corpus_{tool}");
            assert_eq!(res[&key].as_str(), Some("deny"), "researcher: {key}");
            let expected = if CURATOR_TOOLS.contains(&tool) {
                "allow"
            } else {
                "deny"
            };
            assert_eq!(cur[&key].as_str(), Some(expected), "curator: {key}");
            assert_eq!(sup[&key].as_str(), Some("allow"), "super: {key}");
        }
        // And the reverse: a curator holds no sandbox tools at all.
        for tool in CORPUS_TOOLS {
            assert_eq!(cur[tool].as_str(), Some("deny"), "curator: {tool}");
        }
        assert_eq!(cur["webfetch"].as_str(), Some("deny"));
        assert_eq!(cur["bash"].as_str(), Some("deny"));
    }

    /// One role is enforced per session, taken from the primary — so a
    /// curator and a research subagent cannot share one. Refused at set
    /// time, where the operator can see it, rather than silently collapsed
    /// at render time.
    #[test]
    fn a_curator_cannot_hold_a_research_subagent() {
        let store = tmp_store("mixed-subs");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "cur", AgentRole::Curator)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "cur",
                &doc(serde_json::json!({
                    "cur": { "mode": "primary" },
                    "cur-helper": { "mode": "subagent", "description": "d" },
                })),
            )
            .unwrap();
        let error = store
            .set_subagent_role("alpha", "cur", "cur-helper", AgentRole::Tester)
            .unwrap_err()
            .to_string();
        assert!(error.contains("curator"), "{error}");
        assert!(error.contains("tester"), "{error}");
        // A curator subagent under a curator is fine.
        store
            .set_subagent_role("alpha", "cur", "cur-helper", AgentRole::Curator)
            .unwrap();

        // And the mirror: a research primary refuses a curator subagent.
        store
            .create_agent_with_role("alpha", "res", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "res",
                &doc(serde_json::json!({
                    "res": { "mode": "primary" },
                    "res-scout": { "mode": "subagent", "description": "d" },
                })),
            )
            .unwrap();
        assert!(store
            .set_subagent_role("alpha", "res", "res-scout", AgentRole::Curator)
            .is_err());

        // Super is the cross-domain union and may host any project role.
        store
            .create_agent_with_role("alpha", "sup", AgentRole::Super)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "sup",
                &doc(serde_json::json!({
                    "sup": { "mode": "primary" },
                    "sup-curator": { "mode": "subagent", "description": "d" },
                })),
            )
            .unwrap();
        store
            .set_subagent_role("alpha", "sup", "sup-curator", AgentRole::Curator)
            .unwrap();
    }

    /// Every role name survives a round trip. This is the ONLY thing that
    /// catches a variant missing from `parse` — which is why `parse` is now
    /// derived from `as_str` rather than written as a second match.
    #[test]
    fn every_role_name_round_trips() {
        for role in AgentRole::ALL {
            assert_eq!(AgentRole::parse(role.as_str()), Some(role));
            assert_eq!(AgentRole::parse(&role.as_str().to_uppercase()), Some(role));
            assert_eq!(
                AgentRole::parse(&format!("  {}  ", role.as_str())),
                Some(role)
            );
            assert!(
                AgentRole::names().contains(role.as_str()),
                "{} is missing from the usage line",
                role.as_str()
            );
        }
        assert_eq!(AgentRole::parse("root"), None);
        assert_eq!(AgentRole::parse(""), None);
    }

    /// Cloning preserves subagents (it used to drop them) and carries the
    /// role across.
    #[test]
    fn clone_preserves_subagents_and_role() {
        let store = tmp_store("clone-subs");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "src", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "src",
                &doc(serde_json::json!({
                    "src": {
                        "mode": "primary",
                        "permission": { "task": { "*": "deny", "src-scout": "allow" } },
                    },
                    "src-scout": { "mode": "subagent", "description": "scout" },
                })),
            )
            .unwrap();
        store.clone_agent("alpha", "src", "dst").unwrap();
        let cloned = store.load_agent("alpha", "dst").unwrap();
        let map = cloned.doc["agent"].as_object().unwrap();
        assert!(map.contains_key("dst"), "primary renamed: {map:?}");
        assert!(!map.contains_key("src"), "old primary removed: {map:?}");
        assert!(
            map.contains_key("src-scout"),
            "subagents must survive a clone: {map:?}"
        );
    }

    /// A field edit touches ONE key and leaves the rest of the document
    /// byte-identical — the whole point of the granular tools.
    #[test]
    fn set_agent_field_is_surgical() {
        let store = tmp_store("field");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": {
                        "mode": "primary",
                        "description": "before",
                        "prompt": "keep me",
                        "permission": { "bash": "deny" },
                    },
                    "a-scout": { "mode": "subagent", "description": "scout" },
                })),
            )
            .unwrap();
        store
            .set_agent_field(
                "alpha",
                "a",
                None,
                "model",
                "maple/deepseek-v4-flash".into(),
            )
            .unwrap();
        store
            .set_agent_field("alpha", "a", None, "description", "after".into())
            .unwrap();
        let after = store.load_agent("alpha", "a").unwrap();
        let primary = &after.doc["agent"]["a"];
        assert_eq!(primary["model"].as_str(), Some("maple/deepseek-v4-flash"));
        assert_eq!(primary["description"].as_str(), Some("after"));
        // Untouched neighbours survive verbatim.
        assert_eq!(primary["prompt"].as_str(), Some("keep me"));
        assert_eq!(primary["permission"]["bash"].as_str(), Some("deny"));
        assert!(
            after.doc["agent"]["a-scout"].is_object(),
            "subagent untouched"
        );

        // A subagent can be targeted by name.
        store
            .set_agent_field("alpha", "a", Some("a-scout"), "model", "ollama/x".into())
            .unwrap();
        let after = store.load_agent("alpha", "a").unwrap();
        assert_eq!(
            after.doc["agent"]["a-scout"]["model"].as_str(),
            Some("ollama/x")
        );

        // Null removes; structural keys are refused.
        store
            .set_agent_field("alpha", "a", None, "model", serde_json::Value::Null)
            .unwrap();
        assert!(store.load_agent("alpha", "a").unwrap().doc["agent"]["a"]
            .get("model")
            .is_none());
        let err = store
            .set_agent_field("alpha", "a", None, "mode", "subagent".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("not settable"), "{err}");
        // An unknown entry is refused rather than silently created.
        let err = store
            .set_agent_field("alpha", "a", Some("ghost"), "model", "x".into())
            .unwrap_err()
            .to_string();
        assert!(err.contains("no entry named"), "{err}");
    }

    /// A permission patch merges — a caller changing one rule must not
    /// drop the others by omission.
    #[test]
    fn permission_patch_merges_and_removes() {
        let store = tmp_store("perm-patch");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": {
                        "mode": "primary",
                        "permission": { "bash": "deny", "webfetch": "allow" },
                    },
                })),
            )
            .unwrap();
        store
            .patch_agent_permission(
                "alpha",
                "a",
                None,
                &serde_json::json!({ "websearch": "allow", "webfetch": null }),
            )
            .unwrap();
        let perm = &store.load_agent("alpha", "a").unwrap().doc["agent"]["a"]["permission"];
        assert_eq!(
            perm["bash"].as_str(),
            Some("deny"),
            "untouched key survives"
        );
        assert_eq!(perm["websearch"].as_str(), Some("allow"), "added");
        assert!(perm.get("webfetch").is_none(), "null removes");
    }

    /// Adding a subagent also wires the primary's `task:` allow; removing
    /// it takes the rule and the sidecar role back out.
    #[test]
    fn subagent_add_and_remove_keep_delegation_consistent() {
        let store = tmp_store("subagent");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({ "a": { "mode": "primary" } })),
            )
            .unwrap();
        store
            .add_subagent(
                "alpha",
                "a",
                "a-scout",
                "scouts ahead",
                "You scout.",
                Some("ollama/x"),
                Some(AgentRole::Researcher),
            )
            .unwrap();
        let after = store.load_agent("alpha", "a").unwrap();
        assert_eq!(
            after.doc["agent"]["a-scout"]["mode"].as_str(),
            Some("subagent")
        );
        let task = &after.doc["agent"]["a"]["permission"]["task"];
        assert_eq!(
            task["a-scout"].as_str(),
            Some("allow"),
            "delegation wired: {task}"
        );
        assert_eq!(
            task["*"].as_str(),
            Some("deny"),
            "others still denied: {task}"
        );
        assert_eq!(
            after.meta.subagent_roles.get("a-scout"),
            Some(&AgentRole::Researcher)
        );
        // Duplicates and self-collisions refused.
        assert!(store
            .add_subagent("alpha", "a", "a-scout", "d", "p", None, None)
            .is_err());
        assert!(store
            .add_subagent("alpha", "a", "a", "d", "p", None, None)
            .is_err());

        store.remove_subagent("alpha", "a", "a-scout").unwrap();
        let after = store.load_agent("alpha", "a").unwrap();
        assert!(after.doc["agent"].get("a-scout").is_none(), "entry gone");
        assert!(
            after.doc["agent"]["a"]["permission"]["task"]
                .get("a-scout")
                .is_none(),
            "no dangling delegation rule"
        );
        assert!(
            after.meta.subagent_roles.is_empty(),
            "sidecar role cleaned up"
        );
    }

    /// Cross-project copy — the operation that did not exist, and whose
    /// absence cost a whole management-chat session.
    #[test]
    fn copy_agent_across_projects_carries_tree_and_role() {
        let store = tmp_store("copy");
        store.create_project("src", "S", "cdk-regtest").unwrap();
        store.create_project("dst", "D", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("src", "hunter", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "src",
                "hunter",
                &doc(serde_json::json!({
                    "hunter": { "mode": "primary", "prompt": "hunt" },
                    "hunter-scout": { "mode": "subagent", "description": "s" },
                })),
            )
            .unwrap();
        store
            .set_agent_role("src", "hunter", AgentRole::Tester)
            .unwrap();

        store.copy_agent("src", "hunter", "dst", "hunter").unwrap();
        let copied = store.load_agent("dst", "hunter").unwrap();
        assert_eq!(
            copied.doc["agent"]["hunter"]["prompt"].as_str(),
            Some("hunt")
        );
        assert!(
            copied.doc["agent"]["hunter-scout"].is_object(),
            "subagents cross with it"
        );
        assert_eq!(copied.meta.role(), AgentRole::Tester, "role carries over");
        assert_eq!(copied.meta.cloned_from.as_deref(), Some("src/hunter"));
        // The source is untouched, and a second copy is refused.
        assert!(store.load_agent("src", "hunter").is_ok());
        assert!(store.copy_agent("src", "hunter", "dst", "hunter").is_err());
        let err = store
            .copy_agent("src", "hunter", "ghost", "hunter")
            .unwrap_err()
            .to_string();
        assert!(err.contains("project not found"), "{err}");
    }

    /// Migration assigns a role only to agents that never had one, and is
    /// safe to re-run: an agent the operator tightened by hand must never
    /// be silently re-widened by a second pass.
    #[test]
    fn migrate_roles_is_idempotent_and_never_rewidens() {
        let store = tmp_store("migrate");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        // An agent whose permissions grant everything, with NO role yet:
        // strip the sidecar's role to simulate a pre-roles install.
        let mut perm = serde_json::Map::new();
        for tool in CORPUS_TOOLS {
            perm.insert(tool.to_string(), "allow".into());
        }
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": { "mode": "primary", "permission": perm },
                })),
            )
            .unwrap();
        let dir = store.project_agent_dir("alpha", "a");
        fs::write(
            dir.join("agent.yaml"),
            "name: a\ncreated: 0\n", // legacy sidecar: no role key
        )
        .unwrap();
        assert!(
            !read_sidecar(&dir, "a").has_role(),
            "precondition: unassigned"
        );

        // Dry run reports without writing.
        let preview = store.migrate_agent_roles("alpha", false).unwrap();
        let row = preview.iter().find(|r| r.agent == "a").unwrap();
        assert_eq!(row.current, None);
        assert_eq!(
            row.inferred,
            AgentRole::Super,
            "permissions grant everything"
        );
        assert!(!row.applied);
        assert!(
            !read_sidecar(&dir, "a").has_role(),
            "a dry run writes nothing"
        );

        // Apply.
        let applied = store.migrate_agent_roles("alpha", true).unwrap();
        assert!(applied.iter().find(|r| r.agent == "a").unwrap().applied);
        assert_eq!(read_sidecar(&dir, "a").role(), AgentRole::Super);

        // The operator now tightens it by hand...
        store
            .set_agent_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        // ...and a second migration must LEAVE IT ALONE, even though the
        // permission block still says "allow everything".
        let again = store.migrate_agent_roles("alpha", true).unwrap();
        let row = again.iter().find(|r| r.agent == "a").unwrap();
        assert!(!row.applied, "an assigned role is never re-inferred");
        assert_eq!(
            read_sidecar(&dir, "a").role(),
            AgentRole::Researcher,
            "re-running migration must not undo a hand tightening"
        );
    }

    /// Entry names are flat across a project's agents, so a collision must
    /// be refused rather than silently resolved by slug order.
    #[test]
    fn render_refuses_colliding_entry_names() {
        let store = tmp_store("collide");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        for slug in ["one", "two"] {
            store
                .create_agent_with_role("alpha", slug, AgentRole::Researcher)
                .unwrap();
            store
                .save_agent(
                    "alpha",
                    slug,
                    &doc(serde_json::json!({
                        slug: { "mode": "primary" },
                        "shared-scout": { "mode": "subagent", "description": "d" },
                    })),
                )
                .unwrap();
        }
        let error = store
            .render_project_agents("alpha")
            .unwrap_err()
            .to_string();
        assert!(error.contains("shared-scout"), "{error}");
        assert!(error.contains("rename one"), "{error}");
    }

    /// The live leak, reduced: a primary that delegates to a subagent its
    /// project never declares. opencode resolves such a name from whatever
    /// config it discovers ABOVE the run dir — which is how project
    /// `local-runner`'s discover reached `cloud-runner`'s scout. The
    /// rendered set must be closed under delegation or the launch is
    /// refused.
    #[test]
    fn render_refuses_dangling_delegation() {
        let store = tmp_store("dangling");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "discover", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "discover",
                &doc(serde_json::json!({
                    "discover": {
                        "mode": "primary",
                        "permission": { "task": { "*": "deny", "discover-scout": "allow" } },
                    },
                })),
            )
            .unwrap();
        let error = store
            .render_project_agents("alpha")
            .unwrap_err()
            .to_string();
        assert!(error.contains("discover-scout"), "{error}");
        assert!(error.contains("alpha"), "names the project: {error}");
        assert!(error.contains("closed"), "{error}");
        // The standalone checker agrees, so the app can flag it pre-launch.
        assert!(store.check_project_delegation("alpha").is_err());
    }

    /// Delegation ACROSS a project's agents is legal — the entry namespace
    /// is flat, so `one` may call an entry `two` declares. This is why the
    /// check cannot live in `validate_agent_doc`, which sees one document.
    #[test]
    fn delegation_across_agents_in_a_project_is_allowed() {
        let store = tmp_store("cross-delegate");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "one", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "one",
                &doc(serde_json::json!({
                    "one": {
                        "mode": "primary",
                        "permission": { "task": { "*": "deny", "two-scout": "allow" } },
                    },
                })),
            )
            .unwrap();
        store
            .create_agent_with_role("alpha", "two", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "two",
                &doc(serde_json::json!({
                    "two": { "mode": "primary" },
                    "two-scout": { "mode": "subagent", "description": "d" },
                })),
            )
            .unwrap();
        store.check_project_delegation("alpha").unwrap();
        let written = store.render_project_agents("alpha").unwrap();
        assert_eq!(written.len(), 3);
        let one = fs::read_to_string(store.opencode_agent_dir("alpha").join("one.md")).unwrap();
        assert_eq!(
            rendered_permission(&one)["task"]["two-scout"].as_str(),
            Some("allow")
        );
    }

    /// Glob matcher mirroring the enforcement layer's, for evaluating a
    /// rendered rule map the way opencode would.
    fn glob_matches(pattern: &str, text: &str) -> bool {
        fn go(p: &[u8], t: &[u8]) -> bool {
            match (p.split_first(), t.split_first()) {
                (None, None) => true,
                (Some((b'*', rest)), _) => go(rest, t) || (!t.is_empty() && go(p, &t[1..])),
                (Some((b'?', rest)), Some((_, t_rest))) => go(rest, t_rest),
                (Some((pat, rest)), Some((txt, t_rest))) if pat == txt => go(rest, t_rest),
                _ => false,
            }
        }
        go(pattern.as_bytes(), text.as_bytes())
    }

    /// Last match wins, over the keys in the order the rendered file lists
    /// them — which is lexicographic, because `canonical_json` sorts.
    fn evaluate(rules: &serde_yaml::Value, path: &str) -> Option<String> {
        let map = rules.as_mapping()?;
        let mut action = None;
        for (k, v) in map {
            let (Some(pattern), Some(act)) = (k.as_str(), v.as_str()) else {
                continue;
            };
            if glob_matches(pattern, path) {
                action = Some(act.to_string());
            }
        }
        action
    }

    /// The relative patterns describe the run cwd. They say nothing about
    /// an ABSOLUTE path, and after the store moved out of the repo an
    /// absolute path is the one remaining way to name another project's
    /// corpus — the run dir links only one project. Both spellings must
    /// deny, and the agent's own corpus must still be reachable by both.
    #[test]
    fn rendered_permissions_deny_other_projects_absolutely() {
        let store = tmp_store("absolute");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "a": {
                        "mode": "primary",
                        "permission": { "read": { "*": "allow" }, "write": { "*": "deny" } },
                    },
                })),
            )
            .unwrap();
        store.render_project_agents("alpha").unwrap();
        let text = fs::read_to_string(store.opencode_agent_dir("alpha").join("a.md")).unwrap();
        let read = &rendered_permission(&text)["read"];

        let root = store.root().display().to_string();
        let cases = [
            // (path, expected, why)
            (
                "store/projects/other/corpus/findings/x.md",
                "deny",
                "relative, another project",
            ),
            (
                "store/projects/alpha/corpus/findings/x.md",
                "allow",
                "relative, own corpus",
            ),
            (
                "store/projects/alpha/agents/a/agent.yaml",
                "deny",
                "own sidecars are not material",
            ),
            (
                &format!("{root}/projects/other/corpus/findings/x.md"),
                "deny",
                "absolute, another project",
            ),
            (
                &format!("{root}/projects/alpha/corpus/findings/x.md"),
                "allow",
                "absolute, own corpus",
            ),
        ];
        for (path, expected, why) in cases {
            assert_eq!(
                evaluate(read, path).as_deref(),
                Some(expected),
                "{why}: {path}\nrules: {read:?}"
            );
        }
    }

    /// An entry that says nothing about delegation must not inherit
    /// opencode's default: `task` is always written, and an allow naming an
    /// entry outside the project is force-denied in the ARTIFACT even if a
    /// render path skipped the closure check.
    #[test]
    fn rendered_entries_deny_task_by_default() {
        let store = tmp_store("task-default");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({ "a": { "mode": "primary" } })),
            )
            .unwrap();
        store.render_project_agents("alpha").unwrap();
        let text = fs::read_to_string(store.opencode_agent_dir("alpha").join("a.md")).unwrap();
        assert_eq!(
            rendered_permission(&text)["task"]["*"].as_str(),
            Some("deny")
        );

        // And the artifact-level force-deny, exercised directly: a stray
        // allow for an entry the project does not declare renders as deny.
        let known = BTreeSet::new();
        let ctx = RenderCtx {
            project: "alpha",
            role: AgentRole::Researcher,
            known_entries: &known,
            roots: DataRoots::default(),
        };
        let bound = bind_permission(
            &serde_json::json!({ "task": { "*": "deny", "ghost-scout": "allow" } }),
            &ctx,
        );
        assert_eq!(bound["task"]["ghost-scout"].as_str(), Some("deny"));
    }

    #[test]
    fn render_binds_permission_and_scope_to_project() {
        let store = tmp_store("bind");
        store.create_project("alpha", "A", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("alpha", "a", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "alpha",
                "a",
                &doc(serde_json::json!({
                    "one": {
                        "mode": "primary",
                        "prompt": "hello",
                        "permission": {
                            "read": { "*": "allow", "benchmarks/**": "deny" },
                            "write": { "*": "deny", "store/projects/*/corpus/**": "allow" },
                            "bash": "deny"
                        }
                    },
                })),
            )
            .unwrap();
        let written = store.render_project_agents("alpha").unwrap();
        let text = fs::read_to_string(&written[0]).unwrap();
        // Wildcard store paths bound to the concrete project.
        assert!(text.contains("store/projects/alpha/corpus/**"), "{text}");
        assert!(!text.contains("store/projects/*/corpus/**"), "{text}");
        // Read-allow gains the corpus boundary: other projects denied,
        // own project allowed (appended last so it wins evaluation).
        let fm = &text.split("---\n").nth(1).unwrap();
        let yaml: serde_yaml::Value = serde_yaml::from_str(&format!("{{{fm}}}").replace("---", ""))
            .unwrap_or_else(|_| serde_yaml::from_str(fm).unwrap());
        let read = &yaml["permission"]["read"];
        assert_eq!(read["store/projects/*"].as_str(), Some("deny"));
        // Narrowed to research material: the corpus and the mission
        // records, NOT the project's `agents/` sidecars (the role gate
        // trusts those) or its `var/`.
        assert_eq!(
            read["store/projects/alpha/corpus/**"].as_str(),
            Some("allow")
        );
        assert_eq!(
            read["store/projects/alpha/missions/**"].as_str(),
            Some("allow")
        );
        assert_eq!(read["store/projects/alpha/**"].as_str(), None);
        // Scalar permissions untouched.
        assert_eq!(yaml["permission"]["bash"].as_str(), Some("deny"));
        // The scope section names the project corpus.
        assert!(text.contains("## Corpus scope (bound at launch)"));
        assert!(text.contains("You are bound to project `alpha`"));
        // The pin section points at the TOOL and names no revision: the
        // render is a pure function of the project, so a second mission's
        // launch cannot rewrite the trees under a live one.
        assert!(text.contains("## Pinned sources"), "{text}");
        assert!(
            text.contains("Call `target_info` before you read any source"),
            "{text}"
        );
        assert!(
            !text.contains("sources/cdk/"),
            "a literal tree path is a per-RUN fact and must not reach a per-project file: {text}"
        );

        // A curator manages the project and reads no source, so it holds no
        // `target_info` and gets no pin section at all.
        store
            .create_agent_with_role("alpha", "keeper", AgentRole::Curator)
            .unwrap();
        store.render_project_agents("alpha").unwrap();
        let keeper =
            fs::read_to_string(store.opencode_agent_dir("alpha").join("keeper.md")).unwrap();
        assert!(!keeper.contains("Pinned sources"), "{keeper}");

        // THE property that lets two missions run at once: the render is a
        // pure function of the project, so a second mission's launch
        // rewrites the first's agent files with identical bytes. When the
        // pins lived in here, launching B changed the tree paths under a
        // live A — the same class of bug as telling an agent to read a
        // path that does not exist.
        let first: Vec<String> = store
            .render_project_agents("alpha")
            .unwrap()
            .iter()
            .map(|p| fs::read_to_string(p).unwrap())
            .collect();
        let second: Vec<String> = store
            .render_project_agents("alpha")
            .unwrap()
            .iter()
            .map(|p| fs::read_to_string(p).unwrap())
            .collect();
        assert_eq!(
            first, second,
            "a re-render must be byte-identical, or launching one mission \
             disturbs another that is already live"
        );
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn save_agent_refuses_invalid_and_persists_valid() {
        let store = tmp_store("save");
        // Saving edits an existing agent, so create one explicitly first.
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "a", AgentRole::Researcher)
            .unwrap();

        // No agent map.
        assert!(store
            .save_agent("p", "a", &serde_json::json!({ "$schema": "x" }))
            .is_err());
        // Empty agent map.
        assert!(store
            .save_agent("p", "a", &doc(serde_json::json!({})))
            .is_err());
        // Exactly one primary.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "x"},
                    "two": {"mode": "primary", "prompt": "y"},
                })),
            )
            .is_err());
        // Bad permission action.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "x", "permission": "hax"},
                })),
            )
            .is_err());
        // Unresolved {file:} prompt ref.
        assert!(store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "see {file:nope.md}"},
                })),
            )
            .is_err());
        // A valid document persists and reloads.
        store
            .save_agent(
                "p",
                "a",
                &doc(serde_json::json!({
                    "one": {"mode": "primary", "prompt": "hello"},
                    "two": {"mode": "subagent", "prompt": "hi", "permission": {"task": "allow"}},
                })),
            )
            .unwrap();
        let agent = store.load_agent("p", "a").unwrap();
        let map = agent.doc.get("agent").unwrap().as_object().unwrap();
        assert!(map.contains_key("one") && map.contains_key("two"));
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn create_agent_builds_a_valid_doc_from_structured_fields() {
        let store = tmp_store("create");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        // Blank path: minimal doc, validator passes, key == slug.
        store
            .create_agent(
                "p",
                "depbot",
                "scans deps",
                "you scan deps",
                None,
                None,
                None,
            )
            .unwrap();
        let agent = store.load_agent("p", "depbot").unwrap();
        let map = agent.doc.get("agent").unwrap().as_object().unwrap();
        let cfg = map.get("depbot").expect("primary key is the slug");
        assert_eq!(cfg.get("description").unwrap(), "scans deps");
        assert_eq!(cfg.get("prompt").unwrap(), "you scan deps");
        assert_eq!(cfg.get("mode").unwrap(), "primary");
        assert!(
            cfg.get("permission").is_none(),
            "blank path has no permission block"
        );
        // Duplicate refused.
        assert!(store
            .create_agent("p", "depbot", "x", "y", None, None, None)
            .is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn create_agent_from_inherits_and_overlays() {
        let store = tmp_store("createfrom");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent("p", "base", "base agent", "base prompt", None, None, None)
            .unwrap();
        // Give the base a permission block to inherit.
        let mut doc = store.load_agent("p", "base").unwrap().doc;
        doc["agent"]["base"]["permission"] = serde_json::json!({"bash": "deny"});
        store.save_agent("p", "base", &doc).unwrap();
        store
            .create_agent(
                "p",
                "child",
                "child desc",
                "child prompt",
                Some("ollama/x"),
                Some("base"),
                None,
            )
            .unwrap();
        let agent = store.load_agent("p", "child").unwrap();
        let map = agent.doc.get("agent").unwrap().as_object().unwrap();
        // The key is RENAMED to the new slug (the depbot-session lie:
        // agent_get depbot answered with a "researcher" doc).
        assert!(
            !map.contains_key("base"),
            "the inherited key must be renamed"
        );
        let cfg = map.get("child").expect("primary key is the new slug");
        assert_eq!(cfg.get("description").unwrap(), "child desc");
        assert_eq!(cfg.get("prompt").unwrap(), "child prompt");
        assert_eq!(cfg.get("model").unwrap(), "ollama/x");
        assert_eq!(
            cfg["permission"]["bash"],
            serde_json::json!("deny"),
            "permissions inherited"
        );
        // Missing 'from' names the rule.
        let err = store
            .create_agent("p", "orphan", "d", "p", None, Some("ghost"), None)
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("'from' must name an existing agent"),
            "{err}"
        );
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn clone_agent_renames_the_primary_key() {
        let store = tmp_store("clonerename");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        // Create a researcher with a real document, then verify the clone
        // renames its primary key.
        store
            .create_agent_with_role("p", "researcher", AgentRole::Researcher)
            .unwrap();
        store
            .save_agent(
                "p",
                "researcher",
                &doc(serde_json::json!({
                    "researcher": {"mode": "primary", "description": "r", "prompt": "x"},
                })),
            )
            .unwrap();
        store.clone_agent("p", "researcher", "depbot").unwrap();
        let agent = store.load_agent("p", "depbot").unwrap();
        let map = agent.doc.get("agent").unwrap().as_object().unwrap();
        assert!(
            map.contains_key("depbot"),
            "clone must rename the primary key"
        );
        assert!(
            !map.contains_key("researcher"),
            "clone must not keep the old key"
        );
        // The not-found error teaches the create path.
        let err = store.clone_agent("p", "ghost", "x").unwrap_err();
        assert!(err.to_string().contains("agent_list"), "{err}");
        let err = store
            .save_agent(
                "p",
                "ghost",
                &doc(serde_json::json!({"a": {"prompt": "x"}})),
            )
            .unwrap_err();
        assert!(err.to_string().contains("agent_new"), "{err}");
        let _ = fs::remove_dir_all(store.root());
    }
}
