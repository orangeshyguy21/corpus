//! The scoped corpus store and persisted records.
//!
//! On-disk layout (data-model plan v2 — teamless):
//!
//! ```text
//! store/
//!   projects/<project-slug>/
//!     project.yaml                # name, plugin binding, created/cloned-from,
//!                                 #   corpus_generation
//!     corpus/                     # THE corpus (hypotheses/ techniques/
//!                                 #   findings/ attacks/ retro/ runs/) — the
//!                                 #   ONLY corpus scope
//!     agents/<agent-slug>/        # agent configs: agent.yaml, opencode.json,
//!                                 #   prompts/
//!     missions/<mission>.md       # mission records (agent ref, pins, budget,
//!                                 #   created, sessions) + brief body
//!     usage/<session>.json        # compact cumulative accounting snapshots
//! ```
//!
//! The old flat `store/{hypotheses,techniques,findings,attacks,runs}`
//! becomes `store/projects/<default>/corpus/` via a migration that relocates
//! files verbatim. The team concept is gone: the corpus is project-level only.
//! Wiki-truth: markdown + YAML frontmatter is truth, this is all filesystem
//! plumbing, no DB.

use std::path::{Path, PathBuf};

pub use crate::accounting::{
    corpus_cost, corpus_cost_cached, CorpusCostCache, CostReport, CostRow, UsageSnapshot,
    USAGE_SNAPSHOT_VERSION,
};
pub use crate::corpus_entries::EntryAccess;
pub use crate::corpus_stats::{corpus_stats, CategoryStat, CorpusStats};
use crate::error::{Error, Result};
pub use crate::missions::{
    Mission, MissionCompletion, MissionControl, MissionDeleteRequest, MissionDispatch,
    MissionDispatchIdentity, MissionLaunchRequest, MissionRunRef,
};
pub use crate::preferences::AppPrefs;
pub use crate::projects::Project;
pub use crate::run_records::{
    mission_logs, MissionLog, MISSION_ENV, RUNS, RUN_ID_ENV, RUN_LOG_ENV,
};

/// Environment variables overriding the default scope. The store root is
/// resolved in [`crate::paths`], which owns every root the app has.
pub use crate::paths::STORE_ENV;
pub const PROJECT_ENV: &str = "CORPUS_PROJECT";
/// The launched mission's resolved source pins (`{"<repo>": "<sha>"}`
/// JSON) — corpus-mcp forwards these to the plugin so the sandbox mounts
/// the revs the mission recorded, not config.toml's defaults.
pub const SOURCE_PINS_ENV: &str = "CORPUS_SOURCE_PINS";
pub const ENVIRONMENT_SESSION_ENV: &str = "CORPUS_ENVIRONMENT_SESSION";

/// The slug of the agent this run was launched as — the run's IDENTITY.
/// Exported by BOTH launch paths into the opencode process, which
/// corpus-mcp inherits: the server resolves the agent's role from it and
/// gates its tool catalog accordingly. Without it the server cannot tell
/// a researcher from an operator and can only fail closed.
pub const AGENT_ENV: &str = "CORPUS_OPENCODE_AGENT";

/// The opencode `--agent` handle for the run: the launched agent's
/// project-unique, name-derived identifier (see `agents::primary_handles`).
/// SEPARATE from [`AGENT_ENV`] on purpose — opencode shows this (so the
/// operator sees a name, not the opaque dir slug), while the server still
/// resolves the role from [`AGENT_ENV`] (the dir slug, the agent's true
/// identity). For an unnamed agent the two coincide (both the slug).
pub const HANDLE_ENV: &str = "CORPUS_OPENCODE_HANDLE";

/// The corpus category layout.
pub const CATEGORIES: [&str; 6] = [
    "hypotheses",
    "techniques",
    "findings",
    "attacks",
    "retro",
    "runs",
];

/// Resolve the store root: `CORPUS_STORE`, else `~/.corpus/store`.
pub fn store_root_env() -> PathBuf {
    crate::paths::store_root()
}

/// The current project scope, or why there isn't one. There is NO default:
/// a silently-defaulted project resolves every read and write against the
/// wrong subtree and looks like it worked.
pub fn project_slug_env() -> std::result::Result<String, String> {
    let slug = std::env::var(PROJECT_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{PROJECT_ENV} is unset — every launch path sets it"))?;
    validate_slug(&slug).map_err(|e| format!("{PROJECT_ENV}={slug:?}: {e}"))?;
    Ok(slug)
}

/// The store: path plumbing over the scoped layout.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    /// Who this process acts AS, stamped onto everything it changes.
    /// `None` reads as `operator`. Ambient rather than a parameter on six
    /// mutating methods: the answer is a property of the process, and
    /// threading it through every signature would mean every future
    /// mutator has to remember to carry it.
    actor: Option<String>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root, actor: None }
    }

    pub fn from_env() -> Self {
        Self::new(store_root_env())
    }

    /// Act as someone in particular — the MCP server sets this from the
    /// identity it already resolved for the role gate, so a curator's edits
    /// carry its name.
    pub fn with_actor(mut self, actor: impl Into<String>) -> Self {
        self.actor = Some(actor.into());
        self
    }

    /// Who is acting. `operator` when nobody said otherwise: a human at the
    /// CLI or in the management chat.
    pub fn actor(&self) -> &str {
        self.actor.as_deref().unwrap_or("operator")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.projects_dir().join(slug)
    }

    /// The project-local corpus (the ONLY corpus scope).
    pub fn project_corpus_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("corpus")
    }

    pub fn project_agents_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("agents")
    }

    pub fn project_agent_dir(&self, slug: &str, agent: &str) -> PathBuf {
        self.project_agents_dir(slug).join(agent)
    }

    pub fn project_missions_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("missions")
    }
}

/// The current write scope: which project's corpus writes land in.
/// Unscoped tools resolve here. There is no team dimension.
#[derive(Debug, Clone)]
pub struct Scope {
    pub project: String,
}

impl Scope {
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
        }
    }

    /// Resolve the scope from the environment, or refuse. The project must
    /// be named AND exist: a wrong-but-plausible slug would otherwise
    /// materialize a brand-new corpus tree on first write and report
    /// success (see `Store::create_project` for the only sanctioned way a
    /// project comes into being).
    pub fn from_env_strict(store: &Store) -> std::result::Result<Self, String> {
        let slug = project_slug_env()?;
        if !store.project_dir(&slug).join("project.yaml").is_file() {
            return Err(format!(
                "{PROJECT_ENV}={slug:?} names no project in {} — projects are created deliberately, \
                 never by a write landing in them",
                store.root().display()
            ));
        }
        Ok(Self::new(slug))
    }

    /// The project corpus directory this scope writes to.
    pub fn corpus_dir(&self, store: &Store) -> PathBuf {
        store.project_corpus_dir(&self.project)
    }

    /// Runs directories for the run_log gate: the project corpus (single
    /// element — there is no team corpus anymore).
    pub fn runs_dirs(&self, store: &Store) -> [PathBuf; 1] {
        [store.project_corpus_dir(&self.project).join("runs")]
    }
}

/// kebab-case a free-form name ("Dep Bot!" → "dep-bot"). Returns "" when
/// nothing alphanumeric survives — callers fall back to an id.
pub fn slugify(name: &str) -> String {
    name.to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Validate a project or agent/mission slug: kebab-case, no path escapes.
pub fn validate_slug(slug: &str) -> Result<()> {
    if slug.is_empty()
        || slug.len() > 64
        || slug == "."
        || slug == ".."
        || !slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::Store(format!(
            "invalid slug {slug:?}: kebab-case alphanumerics only (a-z0-9-)"
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(Error::Store(format!(
            "invalid slug {slug:?}: no leading/trailing dashes"
        )));
    }
    Ok(())
}

/// A FNV-1a 64-bit content hash, hex-encoded. Deliberately dependency-free:
/// provenance needs a stable, collision-resistant-enough attribution string,
/// not a cryptographic signature (that lands with #17 store hardening).
pub fn fnv1a_hex(bytes: &[u8]) -> String {
    format!("{:016x}", fnv1a(bytes))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Epoch seconds now (pub(crate) so the agents module stamps sidecars).
pub(crate) fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
