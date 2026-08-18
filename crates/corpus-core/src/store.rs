//! The scoped corpus store.
//!
//! On-disk layout (data-model plan v2 — teamless):
//!
//! ```text
//! store/
//!   templates/agents/<seed>/      # CORE seed agents (versioned with the app):
//!                                 #   agent.yaml, opencode.json, prompts/
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
//! ```
//!
//! The old flat `store/{hypotheses,techniques,findings,attacks,runs}`
//! becomes `store/projects/<default>/corpus/` via a migration that relocates
//! files verbatim. The team concept is gone: the corpus is project-level only.
//! Wiki-truth: markdown + YAML frontmatter is truth, this is all filesystem
//! plumbing, no DB.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::frontmatter;

/// Environment variables overriding the default scope. The store root is
/// resolved in [`crate::paths`], which owns every root the app has.
pub use crate::paths::STORE_ENV;
pub const PROJECT_ENV: &str = "CORPUS_PROJECT";
/// The launched mission's resolved source pins (`{"<repo>": "<sha>"}`
/// JSON) — corpus-mcp forwards these to the plugin so the sandbox mounts
/// the revs the mission recorded, not config.toml's defaults.
pub const SOURCE_PINS_ENV: &str = "CORPUS_SOURCE_PINS";

/// The basename of the current run's transcript file in the project
/// corpus `runs/` (e.g. `1786891368-verify.raw`). Set by the launcher
/// so `technique_save`/`finding_write` can cite it without the agent
/// guessing — the sandbox has no host FS and cannot enumerate `runs/`.
pub const RUN_LOG_ENV: &str = "CORPUS_RUN_LOG";

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
pub const CATEGORIES: [&str; 6] =
    ["hypotheses", "techniques", "findings", "attacks", "retro", "runs"];

/// The mission-log category: a corpus dir like any other on disk, but
/// summarized on its own (see `CorpusStats`) — run transcripts dwarf the
/// knowledge categories and would swamp any shared byte breakdown.
pub const RUNS: &str = "runs";

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

    /// The project's opencode RUN directory (`<var>/run/<slug>/`): the
    /// working directory missions launch in, so each project owns its
    /// `.opencode/agent/` set and its opencode session pool (opencode keys
    /// sessions by cwd) — one project never overwrites another's
    /// materialized agents.
    ///
    /// A SIBLING of the store, never inside the project it serves: the run
    /// dir links the project's own subtree into itself, and a run dir
    /// nested in that subtree would make the link a cycle.
    pub fn project_run_dir(&self, slug: &str) -> PathBuf {
        self.var_dir().join("run").join(slug)
    }

    /// This store's mutable side-tree (`<store parent>/var`): run dirs and
    /// chat scopes. Derived from THIS store rather than the environment —
    /// a `Store` built over a temp dir must keep its runs in that temp dir,
    /// not wherever `CORPUS_STORE` happens to point in this process.
    pub fn var_dir(&self) -> PathBuf {
        self.root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone())
            .join("var")
    }

    /// This store's goose management-chat scope for a project. Outside the
    /// project subtree on purpose: chat transcripts range over every
    /// project, and a launched agent can read its own project tree.
    pub fn project_chat_dir(&self, slug: &str) -> PathBuf {
        self.var_dir().join("chat").join(slug)
    }

    /// Provision a project's run directory — the whole boundary, expressed
    /// as a directory:
    ///
    /// ```text
    /// <var>/run/<slug>/
    ///   store/projects/<slug> -> <store>/projects/<slug>   ONLY this project
    ///   sources               -> <resources>/sources
    ///   .opencode/
    ///     opencode.json         generated (carries CORPUS_PROJECT)
    ///     skills              -> <resources>/.opencode/skills
    ///     agent/*.md            the rendered set
    /// ```
    ///
    /// The single-project link is the point. Every rendered permission
    /// pattern is cwd-relative (`store/projects/<slug>/corpus/**`,
    /// `sources/<name>/<sha>`), so they all resolve exactly as before —
    /// but no other project is reachable to deny in the first place, and
    /// `benchmarks/`, `plugins/` and the repo's own `.opencode/` are not
    /// present at all rather than being deny-listed.
    ///
    /// Idempotent; safe to call on every launch.
    pub fn provision_run_dir(&self, slug: &str) -> Result<PathBuf> {
        // A run dir is a project's, so the project must exist.
        let project = Project::load(self, slug)?;
        let run_dir = self.project_run_dir(slug);
        let opencode = run_dir.join(".opencode");
        fs::create_dir_all(opencode.join("agent"))?;
        fs::create_dir_all(run_dir.join("store").join("projects"))?;

        relink(
            &self.project_dir(slug),
            &run_dir.join("store").join("projects").join(slug),
        )?;

        // Pinned source trees: REQUIRED. A run without them reads nothing
        // and quietly falls back to whatever the prompt says about
        // sources.toml, auditing a tree nobody chose. The directory itself
        // is a fetch cache (`srcrev` fills it), so create it rather than
        // refusing a machine that simply hasn't fetched yet — but the
        // RESOURCE ROOT must resolve, because not knowing where sources go
        // is a different problem from not having fetched them.
        let resources = crate::paths::resource_root()?;
        let sources = resources.join("sources");
        fs::create_dir_all(&sources)?;
        relink(&sources, &run_dir.join("sources"))?;
        // Skills are optional — not every install ships them.
        let skills = resources.join(".opencode").join("skills");
        if skills.exists() {
            relink(&skills, &opencode.join("skills"))?;
        }
        // NOT node_modules: opencode creates its own in the run dir at
        // runtime, and a symlink placed first would win and then rot.

        self.write_run_opencode_config(slug, &project, &opencode)?;
        Ok(run_dir)
    }

    /// Write the run dir's own `opencode.json`. A REAL file, regenerated on
    /// every provision, never a symlink to a shared one: it carries
    /// `CORPUS_PROJECT`, so the project scope survives every path into the
    /// MCP server — a hand-run `opencode`, a fresh tmux pane, a server
    /// respawn. The shared repo-level config had no project in it at all,
    /// which is why the scope depended on inherited environment and
    /// silently fell back to a default when that was lost.
    ///
    /// Per-RUN identity (`CORPUS_OPENCODE_AGENT`, `CORPUS_RUN_LOG`,
    /// `CORPUS_SOURCE_PINS`) deliberately stays out: it is not a property
    /// of the project. A server started outside a launch therefore gets a
    /// correctly scoped store with NO agent identity, so its role fails to
    /// resolve and every tool refuses — the right answer.
    fn write_run_opencode_config(&self, slug: &str, project: &Project, opencode: &Path) -> Result<()> {
        let mcp_bin = crate::paths::corpus_mcp_bin()?;
        let plugins = crate::registry::plugins_dir();
        let config = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": {
                "corpus": {
                    "type": "local",
                    "enabled": true,
                    "timeout": 180000,
                    "command": [mcp_bin.to_string_lossy()],
                    "environment": {
                        STORE_ENV: self.root().to_string_lossy(),
                        PROJECT_ENV: slug,
                        crate::registry::PLUGINS_DIR_ENV: plugins.to_string_lossy(),
                        "CORPUS_PLUGIN_DIR": plugins.join(&project.plugin).to_string_lossy(),
                    }
                }
            }
        });
        let body = serde_json::to_string_pretty(&config)
            .map_err(|e| Error::Store(format!("run config: {e}")))?;
        fs::write(opencode.join("opencode.json"), body + "\n")?;
        Ok(())
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
        Self { project: project.into() }
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

/// A project (`store/projects/<slug>/project.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    /// The environment plugin this project is bound to.
    pub plugin: String,
    /// Epoch seconds of creation.
    pub created: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloned_from: Option<String>,
    /// Bumped on every corpus wipe so old run logs stay attributable.
    #[serde(default)]
    pub corpus_generation: u64,
    /// The project's chosen source revs (`repo -> rev`, edited in the top
    /// bar): the revs available come from the plugin, the SELECTION is the
    /// project's. Missions stamp these at creation. Empty = every source
    /// at its default rev.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pins: BTreeMap<String, String>,
}

impl Project {
    pub fn load(store: &Store, slug: &str) -> Result<Self> {
        let path = store.project_dir(slug).join("project.yaml");
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("project not found: {slug}")))?;
        let project: Project = serde_yaml::from_str(&raw)
            .map_err(|e| Error::Store(format!("project {slug}: {e}")))?;
        Ok(project)
    }

    fn save(&self, store: &Store, slug: &str) -> Result<()> {
        let path = store.project_dir(slug).join("project.yaml");
        let raw = serde_yaml::to_string(self)?;
        fs::write(path, raw)?;
        Ok(())
    }
}

/// The app's remembered UI choices (`store/app.yaml`) — the operator's
/// settings, not corpus data. Kept beside the store it describes (and so
/// scoped by `CORPUS_STORE` like everything else) rather than in egui's
/// memory blob, which the app deliberately does not persist.
///
/// Every field is optional-by-default: an older file, a hand-edit, or a
/// field added later must never make the app fail to start.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppPrefs {
    /// The model last chosen in the chat panel's picker. Restored on launch
    /// so the chat comes back where it was left; empty = never chosen, and
    /// the panel stays idle until one is picked.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub chat_model: String,
}

impl Store {
    fn prefs_path(&self) -> PathBuf {
        self.root().join("app.yaml")
    }

    /// Read the remembered UI choices. NEVER fails: a missing, unreadable or
    /// malformed file yields defaults, because losing a preference must not
    /// keep the app from opening.
    pub fn load_prefs(&self) -> AppPrefs {
        fs::read_to_string(self.prefs_path())
            .ok()
            .and_then(|raw| serde_yaml::from_str(&raw).ok())
            .unwrap_or_default()
    }

    /// Persist the remembered UI choices.
    pub fn save_prefs(&self, prefs: &AppPrefs) -> Result<()> {
        let path = self.prefs_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, serde_yaml::to_string(prefs)?)?;
        Ok(())
    }
}

/// The result of a corpus walk: file count + total bytes under
/// `store/projects/<p>/corpus/` (every file in every category, attack
/// directories included), broken down per category for the project
/// view's corpus visual.
///
/// `runs/` is kept OUT of `categories` and reported separately as
/// `logs`: mission transcripts run orders of magnitude larger than the
/// knowledge categories, so folding them in would leave every other
/// category an invisible sliver of the strip. `files`/`bytes` stay the
/// grand totals (knowledge + logs); use `knowledge_files`/
/// `knowledge_bytes` for the corpus-only summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CorpusStats {
    pub files: u64,
    pub bytes: u64,
    /// Per-category file/byte totals, in CATEGORIES order minus `runs`,
    /// plus an `other` bucket for files outside a category dir. Empty
    /// categories are not reported.
    pub categories: Vec<CategoryStat>,
    /// The `runs/` bucket — mission logs. Zeroed when there are none.
    pub logs: CategoryStat,
}

impl CorpusStats {
    /// Files excluding mission logs (the Corpus section's count).
    pub fn knowledge_files(&self) -> u64 {
        self.files.saturating_sub(self.logs.files)
    }

    /// Bytes excluding mission logs (the Corpus section's size).
    pub fn knowledge_bytes(&self) -> u64 {
        self.bytes.saturating_sub(self.logs.bytes)
    }
}

/// One corpus category's share of the summary.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CategoryStat {
    pub name: String,
    pub files: u64,
    pub bytes: u64,
}

/// Walk a project's corpus counting files and summing their sizes. Cheap
/// (one `read_dir` pass) — the UI calls it on selection change and manual
/// refresh, never per-frame. Mirrors `find store/projects/<p>/corpus -type f`
/// plus the byte total: only regular files count, directories (including
/// attack dirs) are descended into; each file is attributed to its
/// top-level category dir.
pub fn corpus_stats(store: &Store, project: &str) -> Result<CorpusStats> {
    let root = store.project_corpus_dir(project);
    let mut stats = CorpusStats::default();
    let mut by_name: std::collections::BTreeMap<String, CategoryStat> = CATEGORIES
        .iter()
        .map(|c| {
            (
                c.to_string(),
                CategoryStat { name: c.to_string(), files: 0, bytes: 0 },
            )
        })
        .collect();
    if root.is_dir() {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    stack.push(path);
                } else if kind.is_file() {
                    let Ok(meta) = entry.metadata() else { continue };
                    stats.files += 1;
                    stats.bytes += meta.len();
                    // Top-level dir the file sits under; a file loose at
                    // the corpus root belongs to no category, so it lands
                    // in `other` rather than becoming a bucket of one.
                    let rel = path.strip_prefix(&root).ok();
                    let category = rel
                        .filter(|rel| rel.components().count() > 1)
                        .and_then(|rel| rel.components().next())
                        .and_then(|c| c.as_os_str().to_str())
                        .unwrap_or("other");
                    let slot = by_name
                        .entry(category.to_string())
                        .or_insert_with(|| CategoryStat {
                            name: category.to_string(),
                            files: 0,
                            bytes: 0,
                        });
                    slot.files += 1;
                    slot.bytes += meta.len();
                }
            }
        }
    }
    // `runs/` is reported on its own (mission logs), never as a category.
    stats.logs = by_name
        .remove(RUNS)
        .unwrap_or_else(|| CategoryStat { name: RUNS.to_string(), ..CategoryStat::default() });
    // CATEGORIES order first, then any extra bucket (e.g. "other");
    // empty categories are not reported.
    let mut categories: Vec<CategoryStat> = CATEGORIES
        .iter()
        .filter(|c| **c != RUNS)
        .map(|c| by_name.remove(*c).expect("seeded above"))
        .collect();
    categories.extend(by_name.into_values());
    categories.retain(|c| c.files > 0);
    stats.categories = categories;
    Ok(stats)
}

/// One mission transcript in the project corpus `runs/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionLog {
    /// File name as it sits in `runs/` (e.g. `1786891368-verify.raw`) —
    /// the value `CORPUS_RUN_LOG` carries and findings cite.
    pub name: String,
    /// The agent/mission slug parsed out of `<epoch>-<name>.<ext>`;
    /// the whole stem when the name predates that convention.
    pub mission: String,
    /// Run-start epoch seconds from the name prefix (0 when absent).
    pub started: u64,
    pub bytes: u64,
    /// Extension: `raw` (piped transcript), `json` (opencode export).
    pub kind: String,
}

/// List a project's mission logs, newest first. Cheap (one `read_dir`,
/// no parsing) — the project view calls it alongside `corpus_stats`.
/// Only regular files directly under `runs/` count, matching what the
/// stats walk attributes to the logs bucket.
pub fn mission_logs(store: &Store, project: &str) -> Result<Vec<MissionLog>> {
    let runs = store.project_corpus_dir(project).join(RUNS);
    let mut logs = Vec::new();
    if !runs.is_dir() {
        return Ok(logs);
    }
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if !kind.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let kind = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
        // `<epoch>-<mission>`: split once, and only when the prefix really
        // is a number (a mission named `2fa-probe` must not lose its head).
        let (started, mission) = match stem.split_once('-') {
            Some((head, rest)) if !rest.is_empty() => match head.parse::<u64>() {
                Ok(epoch) => (epoch, rest.to_string()),
                Err(_) => (0, stem.to_string()),
            },
            _ => (0, stem.to_string()),
        };
        logs.push(MissionLog { name: name.to_string(), mission, started, bytes, kind });
    }
    logs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| a.name.cmp(&b.name)));
    Ok(logs)
}

/// Usage aggregation for one (provider, model) pair, summed over every
/// exported run transcript in the project corpus.
#[derive(Debug, Clone, Default)]
pub struct CostRow {
    pub provider: String,
    pub model: String,
    /// Assistant messages counted (each carries one usage record).
    pub messages: u64,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_reasoning: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// USD, as reported by opencode's export.
    pub cost: f64,
}

/// The project view's Cost section: per-model rows (cost desc) + totals.
/// Source data: `runs/<epoch>-<agent>.json` opencode exports (piped
/// `.log` transcripts carry no usage; they are simply not counted).
#[derive(Debug, Clone, Default)]
pub struct CostReport {
    pub rows: Vec<CostRow>,
    pub tokens: u64,
    pub cost: f64,
}

#[derive(Debug, Clone)]
struct CachedCostFile {
    modified: Option<std::time::SystemTime>,
    len: u64,
    report: CostReport,
}

/// Parsed usage records keyed by the durable file identity relevant to the
/// UI: path plus metadata change signals. A rewritten export invalidates only
/// itself, not every other transcript in the project.
#[derive(Debug, Clone, Default)]
pub struct CorpusCostCache {
    files: std::collections::BTreeMap<PathBuf, CachedCostFile>,
}

/// Aggregate token/cost usage across a project's exported run
/// transcripts. Cheap (one parse per runs/*.json) and best-effort: an
/// unparseable file is skipped, never fatal — a corrupt export must not
/// blank the view.
pub fn corpus_cost(store: &Store, project: &str) -> Result<CostReport> {
    corpus_cost_cached(store, project, &mut CorpusCostCache::default())
}

pub fn corpus_cost_cached(
    store: &Store,
    project: &str,
    cache: &mut CorpusCostCache,
) -> Result<CostReport> {
    let runs = store.project_corpus_dir(project).join("runs");
    if !runs.is_dir() {
        cache.files.clear();
        return Ok(CostReport::default());
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().ok();
        let len = meta.len();
        seen.insert(path.clone());
        let current = cache.files.get(&path).is_some_and(|cached| {
            cached.modified == modified && cached.len == len
        });
        if !current {
            let report = parse_cost_file(&path);
            cache.files.insert(path, CachedCostFile { modified, len, report });
        }
    }
    cache.files.retain(|path, _| seen.contains(path));
    Ok(merge_cost_reports(cache.files.values().map(|cached| &cached.report)))
}

fn parse_cost_file(path: &Path) -> CostReport {
    let mut report = CostReport::default();
    let mut rows = std::collections::BTreeMap::<(String, String), CostRow>::new();
    if let Ok(raw) = fs::read_to_string(path) {
        if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) {
            for message in doc
                .get("messages")
                .and_then(|messages| messages.as_array())
                .into_iter()
                .flatten()
            {
            let info = message.get("info").cloned().unwrap_or_default();
            if info.get("role").and_then(|r| r.as_str()) != Some("assistant") {
                continue;
            }
            let provider = info
                .get("providerID")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();
            let model = info
                .get("modelID")
                .and_then(|v| v.as_str())
                .and_then(|m| m.rsplit('/').next())
                .unwrap_or("unknown")
                .to_string();
            let row = rows.entry((provider.clone(), model.clone())).or_insert_with(|| {
                CostRow { provider, model, ..CostRow::default() }
            });
            let tokens = info.get("tokens").cloned().unwrap_or_default();
            let take = |v: &serde_json::Value, key: &str| {
                v.get(key).and_then(|n| n.as_u64()).unwrap_or(0)
            };
            let cache = tokens.get("cache").cloned().unwrap_or_default();
            row.messages += 1;
            row.tokens_input += take(&tokens, "input");
            row.tokens_output += take(&tokens, "output");
            row.tokens_reasoning += take(&tokens, "reasoning");
            row.cache_read += take(&cache, "read");
            row.cache_write += take(&cache, "write");
            row.cost += info.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0);
            report.tokens += take(&tokens, "total");
            }
        }
    }
    report.rows = rows.into_values().collect();
    report.cost = report.rows.iter().map(|row| row.cost).sum();
    report
}

fn merge_cost_reports<'a>(reports: impl Iterator<Item = &'a CostReport>) -> CostReport {
    let mut report = CostReport::default();
    let mut rows = std::collections::BTreeMap::<(String, String), CostRow>::new();
    for source in reports {
        report.tokens += source.tokens;
        for source_row in &source.rows {
            let row = rows
                .entry((source_row.provider.clone(), source_row.model.clone()))
                .or_insert_with(|| CostRow {
                    provider: source_row.provider.clone(),
                    model: source_row.model.clone(),
                    ..CostRow::default()
                });
            row.messages += source_row.messages;
            row.tokens_input += source_row.tokens_input;
            row.tokens_output += source_row.tokens_output;
            row.tokens_reasoning += source_row.tokens_reasoning;
            row.cache_read += source_row.cache_read;
            row.cache_write += source_row.cache_write;
            row.cost += source_row.cost;
        }
    }
    report.rows = rows.into_values().collect();
    report
        .rows
        .sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
    report.cost = report.rows.iter().map(|r| r.cost).sum();
    report
}

/// A mission record (`store/projects/<p>/missions/<slug>.md`): the
/// launch unit. Frontmatter carries the agent ref + source pins + budget +
/// created + the session bindings; the markdown body is the mission brief.
///
/// There is deliberately no lifecycle `status` field. Whether a mission is
/// up is DERIVED from the tmux session listing and the run's capture (see
/// `AppState::mission_activity`), which is the only account that survives
/// the app being killed mid-run. A persisted status could only drift out of
/// agreement with it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mission {
    /// Slug of the agent to launch.
    pub agent: String,
    /// Per-source pinned revisions (`<repo> -> <rev>`).
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// Per-mission execution budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<String>,
    /// Epoch seconds of creation.
    #[serde(default)]
    pub created: u64,
    /// The operator-facing display name (sidebar label). Falls back to the
    /// slug in the corpus store; the app renders `new` when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The tmux session the mission's run is attached to (`corpus-<agent>-<ts>`),
    /// when live — re-attach after an app relaunch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// The opencode session id (transcript of record) — export-on-stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_session: Option<String>,
    /// A launch the CURATOR requested but the app has not yet honored
    /// (epoch seconds of the request). The curator (an MCP client) cannot
    /// spawn a run itself — run spawning is the app's alone — so it flags
    /// the record here and the app's poll beat picks it up, spawns a
    /// detached session with the brief as the kickoff prompt, and clears
    /// the flag. `None` on every mission the curator did not ask to launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_requested: Option<u64>,
}

impl Mission {
    pub fn load(store: &Store, project: &str, slug: &str) -> Result<Self> {
        let path = store.project_missions_dir(project).join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("mission not found: {project}/{slug}")))?;
        Mission::parse(&raw)
    }

    fn parse(raw: &str) -> Result<Self> {
        let (fm, _body) = frontmatter::split(raw)?;
        let fm = fm.ok_or_else(|| Error::Store("mission has no frontmatter".into()))?;
        let mission: Mission = serde_yaml::from_str(
            &serde_yaml::to_string(&fm).map_err(|e| Error::Store(format!("mission: {e}")))?,
        )
        .map_err(|e| Error::Store(format!("mission: {e}")))?;
        if mission.agent.is_empty() {
            return Err(Error::Store("mission missing agent ref".into()));
        }
        Ok(mission)
    }

    fn save(&self, store: &Store, project: &str, slug: &str, brief: &str) -> Result<()> {
        let dir = store.project_missions_dir(project);
        fs::create_dir_all(&dir)?;
        let fm = serde_yaml::to_string(self)?;
        let text = format!("---\n{fm}---\n\n{brief}");
        fs::write(dir.join(format!("{slug}.md")), text)?;
        Ok(())
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
pub fn validate_slug(slug: &str) -> Result<()> {    if slug.is_empty()
        || slug.len() > 64
        || slug == "."
        || slug == ".."
        || !slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(Error::Store(format!(
            "invalid slug {slug:?}: kebab-case alphanumerics only (a-z0-9-)"
        )));
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err(Error::Store(format!("invalid slug {slug:?}: no leading/trailing dashes")));
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

// -------------------------------------------------------------------------
// Projects
// -------------------------------------------------------------------------

impl Store {
    /// Create a project: `store/projects/<slug>/` with the corpus category
    /// dirs, an empty missions dir, and the core seed agent pair.
    pub fn create_project(&self, slug: &str, name: &str, plugin: &str) -> Result<Project> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if dir.exists() {
            return Err(Error::Store(format!("project already exists: {slug}")));
        }
        fs::create_dir_all(&dir)?;
        let project = Project {
            name: name.to_string(),
            plugin: plugin.to_string(),
            created: now_epoch(),
            cloned_from: None,
            corpus_generation: 0,
            pins: BTreeMap::new(),
        };
        fs::create_dir_all(self.project_agents_dir(slug))?;
        fs::create_dir_all(self.project_missions_dir(slug))?;
        for category in CATEGORIES {
            fs::create_dir_all(self.project_corpus_dir(slug).join(category))?;
        }
        project.save(self, slug)?;
        // A new project has NO agents. It used to be seeded with an
        // operator/researcher pair, which meant every project answered to
        // those two names — so a mis-scoped server naming one of them
        // resolved cleanly against the wrong project and looked correct.
        // Agents are created deliberately, from a role.
        Ok(project)
    }

    /// List projects, sorted by slug.
    pub fn list_projects(&self) -> Result<Vec<(String, Project)>> {
        let mut found = Vec::new();
        let dir = self.projects_dir();
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if !path.is_dir() {
                continue;
            }
            let slug = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Store("non-utf8 project dir".into()))?;
            if let Ok(project) = Project::load(self, slug) {
                found.push((slug.to_string(), project));
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Delete a project (removes its whole subtree). No slug is privileged:
    /// `default` used to be undeletable because an unset `CORPUS_PROJECT`
    /// silently resolved there, so it had to exist. Nothing defaults now.
    pub fn delete_project(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if !dir.is_dir() {
            return Err(Error::Store(format!("project not found: {slug}")));
        }
        fs::remove_dir_all(&dir)?;
        // The run dir is a sibling of the store now, so deleting the
        // project no longer takes it with it. A left-behind run dir holds
        // a dangling project link and a generated opencode.json naming a
        // project that is gone.
        let _ = fs::remove_dir_all(self.project_run_dir(slug));
        Ok(())
    }

    /// Clone a project: config + agents + missions, corpus copy optional.
    /// The clone MIRRORS its source — `create_project` seeds a fresh
    /// `operator`/`researcher` pair, so those are cleared before the
    /// source's agent tree is copied in. Without that, cloning a project
    /// whose agents are named anything else left the clone holding the
    /// source's agents PLUS two seeded strays that were never in it.
    pub fn clone_project(
        &self,
        from: &str,
        to: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<Project> {
        let source = Project::load(self, from)?;
        self.create_project(to, name.unwrap_or(&source.name), &source.plugin)?;
        let project = Project {
            name: name.unwrap_or(&source.name).to_string(),
            cloned_from: Some(from.to_string()),
            ..source
        };
        project.save(self, to)?;
        // Drop the seeded pair so the clone is a mirror, not a merge.
        let _ = fs::remove_dir_all(self.project_agents_dir(to));
        copy_tree(&self.project_agents_dir(from), &self.project_agents_dir(to))?;
        copy_tree(&self.project_missions_dir(from), &self.project_missions_dir(to))?;
        if with_corpus {
            copy_tree(
                &self.project_corpus_dir(from),
                &self.project_corpus_dir(to),
            )?;
        }
        Ok(project)
    }

    /// Rename a project's DISPLAY name. The slug is the project's identity —
    /// it names the directory and is what agents, missions, run dirs, pins
    /// and chat sessions are keyed by — so a rename touches the label only,
    /// never the path.
    pub fn rename_project(&self, slug: &str, name: &str) -> Result<Project> {
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Store("project name must not be empty".into()));
        }
        let mut project = Project::load(self, slug)?;
        project.name = name.to_string();
        project.save(self, slug)?;
        Ok(project)
    }

    /// Rebind a project's environment plugin (the one mutable binding a
    /// project carries).
    pub fn rebind_project(&self, slug: &str, plugin: &str) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        project.plugin = plugin.to_string();
        project.save(self, slug)?;
        Ok(project)
    }

    /// Persist a project's source-rev selection (the top-bar dropdowns —
    /// the plugin defines the revs available, the project owns the pick).
    pub fn set_project_pins(
        &self,
        slug: &str,
        pins: BTreeMap<String, String>,
    ) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        project.pins = pins;
        project.save(self, slug)?;
        Ok(project)
    }

    /// Wipe a project's corpus: delete the working subtree, keep the
    /// project + agents, bump `corpus_generation`. Fresh runs lose no
    /// provenance — the wipe is a working-tree operation and the generation
    /// keeps old logs attributable.
    pub fn wipe_project_corpus(&self, slug: &str) -> Result<Project> {
        let mut project = Project::load(self, slug)?;
        let corpus = self.project_corpus_dir(slug);
        if corpus.is_dir() {
            fs::remove_dir_all(&corpus)?;
        }
        ensure_corpus_categories(&corpus)?;
        project.corpus_generation += 1;
        project.save(self, slug)?;
        Ok(project)
    }

    /// Resolve a caller-supplied RELATIVE path to an absolute one inside a
    /// project's corpus, or refuse.
    ///
    /// The shared guard for reading, moving and deleting corpus entries.
    /// Textual checks come first because they are cheap and unambiguous;
    /// `canonicalize` comes last because it is the only one that catches a
    /// SYMLINK planted inside the corpus — and the corpus is precisely
    /// where an agent is allowed to write with its own file tools, so that
    /// is not a theoretical concern.
    ///
    /// [`EntryAccess`] decides how strict it is: reading reaches anything
    /// inside the corpus, changing one refuses `runs/` and whole
    /// categories, and a destination need not exist yet.
    pub fn resolve_corpus_entry(
        &self,
        project: &str,
        rel: &str,
        access: EntryAccess,
    ) -> Result<PathBuf> {
        let rel = rel.trim();
        if rel.is_empty() {
            return Err(Error::Store("path is empty".into()));
        }
        let path = Path::new(rel);
        if path.is_absolute() {
            return Err(Error::Store(format!(
                "path must be relative to the project corpus: {rel}"
            )));
        }
        let mut components = Vec::new();
        for component in path.components() {
            match component {
                std::path::Component::Normal(part) => components.push(part),
                // `..` is the obvious escape, but a stray `/` or a Windows
                // prefix would also re-root the join.
                other => {
                    return Err(Error::Store(format!(
                        "path component {other:?} is not allowed inside a corpus: {rel}"
                    )))
                }
            }
        }
        let category = components
            .first()
            .and_then(|c| c.to_str())
            .ok_or_else(|| Error::Store(format!("path names no category: {rel}")))?;
        if !CATEGORIES.contains(&category) {
            return Err(Error::Store(format!(
                "{category:?} is not a corpus category (one of {})",
                CATEGORIES.join(", ")
            )));
        }
        if category == RUNS && access.is_mutation() {
            return Err(Error::Store(format!(
                "{RUNS}/ holds the mission transcripts: technique cards cite them by name, the \
                 cost report counts them, and they are what an operator reads to audit a run. \
                 They can be read, never changed: {rel}"
            )));
        }
        if components.len() == 1 && access.is_mutation() {
            return Err(Error::Store(format!(
                "{rel} is a whole category, not an entry — removing one wholesale is a corpus \
                 wipe under another name"
            )));
        }
        // The root must canonicalize: comparing a canonical path against a
        // non-canonical root refuses legal paths whenever the store sits
        // behind a symlink, which it does whenever a run dir is involved.
        let root = self.project_corpus_dir(project).canonicalize().map_err(|e| {
            Error::Store(format!(
                "project {project} has no corpus directory ({e}) — create the project first"
            ))
        })?;
        let joined = root.join(path);
        let resolved = match access != EntryAccess::Destination {
            true => joined
                .canonicalize()
                .map_err(|e| Error::Store(format!("{rel}: {e}")))?,
            false => {
                // The destination need not exist, and neither need its
                // parents: moving into `hypotheses/2026/` should create the
                // year. Canonicalize the deepest ancestor that DOES exist,
                // prove that is inside the corpus, then re-join the rest.
                // Every remaining component was checked to be `Normal`
                // above, so nothing in the tail can climb back out.
                let mut existing = joined.clone();
                let mut tail: Vec<std::ffi::OsString> = Vec::new();
                while !existing.exists() {
                    let name = existing
                        .file_name()
                        .ok_or_else(|| Error::Store(format!("{rel} names no file")))?
                        .to_os_string();
                    tail.push(name);
                    existing = existing
                        .parent()
                        .ok_or_else(|| Error::Store(format!("{rel} has no parent")))?
                        .to_path_buf();
                }
                let mut resolved = existing
                    .canonicalize()
                    .map_err(|e| Error::Store(format!("{rel}: {e}")))?;
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                resolved
            }
        };
        if !resolved.starts_with(&root) {
            return Err(Error::Store(format!(
                "{rel} resolves outside the project corpus — a link inside a corpus does not \
                 widen it"
            )));
        }
        Ok(resolved)
    }

    /// Delete ONE entry from a project's corpus, returning the bytes freed.
    /// A directory needs `recursive`: attacks are stored as directories, so
    /// without it a one-word slip takes a whole artifact.
    pub fn delete_corpus_entry(&self, project: &str, rel: &str, recursive: bool) -> Result<u64> {
        let path = self.resolve_corpus_entry(project, rel, EntryAccess::Mutate)?;
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() {
            if !recursive {
                return Err(Error::Store(format!(
                    "{rel} is a directory — pass recursive to remove it and everything under it"
                )));
            }
            let bytes = dir_bytes(&path);
            fs::remove_dir_all(&path)?;
            return Ok(bytes);
        }
        let bytes = meta.len();
        fs::remove_file(&path)?;
        Ok(bytes)
    }

    /// Move or rename ONE entry within a project's corpus. Both ends stay
    /// inside the same corpus; an existing destination is refused unless
    /// `overwrite`.
    pub fn move_corpus_entry(
        &self,
        project: &str,
        from: &str,
        to: &str,
        overwrite: bool,
    ) -> Result<()> {
        let src = self.resolve_corpus_entry(project, from, EntryAccess::Mutate)?;
        let dst = self.resolve_corpus_entry(project, to, EntryAccess::Destination)?;
        if src == dst {
            return Ok(());
        }
        if dst.symlink_metadata().is_ok() {
            if !overwrite {
                return Err(Error::Store(format!(
                    "{to} already exists — pass overwrite to replace it"
                )));
            }
            match dst.is_dir() {
                true => fs::remove_dir_all(&dst)?,
                false => fs::remove_file(&dst)?,
            }
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&src, &dst)?;
        Ok(())
    }

    /// Write ONE entry into a project's corpus, creating it or replacing it
    /// in place. Returns the bytes written.
    ///
    /// This is the tool-shaped write path: the caller names a corpus-
    /// relative entry (`techniques/foo.md`), never a host path, so an agent
    /// never has to know where the corpus physically lives or reason about
    /// the run's cwd — the trap that made writing with raw file tools so
    /// error-prone. `resolve_corpus_entry` with `Destination` access does
    /// the whole guard: it refuses an absolute path, `runs/`, a bare
    /// category, and any symlink that resolves back out of the corpus, and
    /// it materializes parent directories that do not exist yet.
    ///
    /// Creating a NEW category dir under the corpus is not this method's
    /// job — the resolver rejects a first component that is not one of
    /// `CATEGORIES`, so a typo lands as a clear error, not a stray tree.
    pub fn write_corpus_entry(&self, project: &str, rel: &str, content: &str) -> Result<u64> {
        let path = self.resolve_corpus_entry(project, rel, EntryAccess::Destination)?;
        if path.symlink_metadata().map(|m| m.is_dir()).unwrap_or(false) {
            return Err(Error::Store(format!(
                "{rel} is a directory — entry_write replaces a file, not a tree"
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, content)?;
        Ok(content.len() as u64)
    }

    // --- missions ---

    /// Create (or overwrite) a mission record at
    /// `store/projects/<p>/missions/<slug>.md`.
    pub fn write_mission(
        &self,
        project: &str,
        slug: &str,
        mission: &Mission,
        brief: &str,
    ) -> Result<()> {
        validate_slug(slug)?;
        if Project::load(self, project).is_err() {
            return Err(Error::Store(format!("project not found: {project}")));
        }
        // The agent ref must resolve on the project.
        self.load_agent(project, &mission.agent).map_err(|e| {
            Error::Store(format!("mission {slug}: agent {:?}: {e}", mission.agent))
        })?;
        mission.save(self, project, slug, brief)
    }

    /// List a project's missions, sorted by slug.
    pub fn list_missions(&self, project: &str) -> Result<Vec<(String, Mission)>> {
        let mut found = Vec::new();
        let dir = self.project_missions_dir(project);
        if !dir.is_dir() {
            return Ok(found);
        }
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
                if let Some(slug) = path.file_stem().and_then(|s| s.to_str()) {
                    if let Ok(mission) = Mission::load(self, project, slug) {
                        found.push((slug.to_string(), mission));
                    }
                }
            }
        }
        found.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(found)
    }

    /// Load a mission record.
    pub fn load_mission(&self, project: &str, slug: &str) -> Result<Mission> {
        Mission::load(self, project, slug)
    }

    /// Read a mission's brief body.
    pub fn mission_brief(&self, project: &str, slug: &str) -> Result<String> {
        let path = self.project_missions_dir(project).join(format!("{slug}.md"));
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("mission not found: {project}/{slug}")))?;
        let (fm, body) = frontmatter::split(&raw)?;
        if fm.is_none() {
            return Err(Error::Store("mission has no frontmatter".into()));
        }
        Ok(body.to_string())
    }

    /// Delete a mission record.
    pub fn delete_mission(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let path = self.project_missions_dir(project).join(format!("{slug}.md"));
        if !path.is_file() {
            return Err(Error::Store(format!("mission not found: {project}/{slug}")));
        }
        fs::remove_file(path)?;
        Ok(())
    }

    /// Rewrite a mission record's frontmatter from an updated `Mission`,
    /// preserving its brief body byte-for-byte. Used by the app to persist
    /// run bookkeeping (session / opencode_session) and the display name.
    pub fn update_mission(&self, project: &str, slug: &str, mission: &Mission) -> Result<()> {
        let brief = self.mission_brief(project, slug)?;
        self.write_mission(project, slug, mission, &brief)
    }
}

/// Point `link` at `target`, or say why not. Every failure mode is an
/// error rather than a shrug: this is the mechanism the project boundary
/// is made of, so "it didn't work and nobody mentioned it" is the one
/// outcome that must be impossible.
#[cfg(unix)]
fn relink(target: &Path, link: &Path) -> Result<()> {
    if !target.exists() {
        // The predecessor returned Ok here. That single line is what would
        // have turned moving the store out of the repo into a silent
        // no-op: no MCP config, no sources, and permission patterns
        // matching nothing — a run that looks fine and is unbounded.
        return Err(Error::Store(format!(
            "cannot link {}: it does not exist",
            target.display()
        )));
    }
    match link.symlink_metadata() {
        // Already a symlink: repoint it if it aims somewhere stale. The
        // predecessor never replaced an existing link, so a run dir kept
        // whichever target it was first given.
        Ok(meta) if meta.file_type().is_symlink() => {
            if fs::read_link(link).ok().as_deref() == Some(target) {
                return Ok(());
            }
            fs::remove_file(link)?;
        }
        // A real file or directory: never silently accept it as the link.
        Ok(_) => {
            return Err(Error::Store(format!(
                "{} is a real path, not a link to {} — refusing to provision over it",
                link.display(),
                target.display()
            )));
        }
        Err(_) => {}
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn relink(_target: &Path, _link: &Path) -> Result<()> {
    Err(Error::Store(
        "run directories need symlinks; this platform has none".into(),
    ))
}

/// What a caller intends to do with a corpus entry. Reading is permitted
/// anywhere inside the corpus — a run transcript is legitimate material,
/// and reading one changes nothing — while changing one is refused for
/// `runs/` (cards cite those by name) and for a bare category (that is a
/// corpus wipe wearing a different name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryAccess {
    /// Read an existing entry.
    Read,
    /// Change or remove an existing entry.
    Mutate,
    /// A move destination, which need not exist yet.
    Destination,
}

impl EntryAccess {
    fn is_mutation(self) -> bool {
        matches!(self, Self::Mutate | Self::Destination)
    }
}

/// Total bytes under a directory, best-effort (an unreadable entry counts
/// as zero rather than failing a delete that is about to remove it anyway).
fn dir_bytes(dir: &Path) -> u64 {
    let mut total = 0;
    let Ok(entries) = fs::read_dir(dir) else {
        return total;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match path.is_dir() {
            true => total += dir_bytes(&path),
            false => total += entry.metadata().map(|m| m.len()).unwrap_or(0),
        }
    }
    total
}

/// Create the category directories under a corpus root.
fn ensure_corpus_categories(corpus: &Path) -> Result<()> {
    for category in CATEGORIES {
        fs::create_dir_all(corpus.join(category))?;
    }
    Ok(())
}

/// Recursively copy a directory tree, preserving file permissions.
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

    /// A store in its own world — run dirs are siblings of the store, so
    /// sharing a parent means sharing `<parent>/var/run/<project>`.
    fn tmp_store(tag: &str) -> Store {
        let world =
            std::env::temp_dir().join(format!("corpus-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn validate_slug_rejects_bad_names() {
        for bad in ["", "..", "a/b", "Upper", "under_score", "-lead", "trail-", "a b"] {
            assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
        }
        for good in ["default", "cdk-regtest", "red-alpha-2"] {
            assert!(validate_slug(good).is_ok(), "should accept {good:?}");
        }
    }

    /// The boundary, as a directory. A run dir must expose EXACTLY its own
    /// project: this is what makes the rendered permission globs a second
    /// line of defence rather than the only one, and what puts the run cwd
    /// outside the git repo so opencode cannot discover another project's
    /// agents above it.
    #[test]
    fn a_run_dir_exposes_only_its_own_project() {
        let store = tmp_store("run-scope");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        store.create_project("b", "B", "cdk-regtest").unwrap();
        let run = store.provision_run_dir("a").unwrap();

        let projects = run.join("store").join("projects");
        let visible: Vec<String> = fs::read_dir(&projects)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(visible, vec!["a".to_string()], "only its own project");
        assert!(!projects.join("b").exists(), "project b is not reachable");
        assert_eq!(
            fs::read_link(projects.join("a")).unwrap(),
            store.project_dir("a"),
            "the link points at the real project"
        );

        // The cycle guard: a run dir INSIDE the project it links would make
        // that link infinitely recursive (<p>/var/run/store/projects/<p>/...).
        assert!(
            !run.starts_with(store.project_dir("a")),
            "the run dir must not live inside the project it links: {}",
            run.display()
        );

        // Nothing else from the resource tree leaks in: the benchmark
        // answer key and the harness internals are ABSENT, not deny-listed.
        assert!(!run.join("benchmarks").exists());
        assert!(!run.join("plugins").exists());
    }

    /// Provisioning is idempotent AND corrective: a link left pointing at
    /// the wrong project must be repaired, not accepted. The predecessor
    /// never replaced an existing link, so a run dir kept whichever target
    /// it was first given.
    #[test]
    fn provisioning_repoints_a_stale_link() {
        let store = tmp_store("run-stale");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        store.create_project("b", "B", "cdk-regtest").unwrap();
        let run = store.provision_run_dir("a").unwrap();
        let link = run.join("store").join("projects").join("a");

        // Point it at the wrong project behind our back.
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(store.project_dir("b"), &link).unwrap();

        store.provision_run_dir("a").unwrap();
        assert_eq!(fs::read_link(&link).unwrap(), store.project_dir("a"));
    }

    /// The generated run config is what carries the project scope into
    /// every corpus-mcp spawn — a hand-run opencode, a fresh tmux pane, a
    /// server respawn. A SHARED config (the old symlink to the repo's
    /// opencode.json) named no project at all, so the scope depended on
    /// inherited environment and silently defaulted when that was lost.
    #[test]
    fn the_run_config_names_its_own_project() {
        let store = tmp_store("run-config");
        store.create_project("a", "A", "cdk-regtest").unwrap();
        let run = store.provision_run_dir("a").unwrap();
        let config = run.join(".opencode").join("opencode.json");

        assert!(
            !config.symlink_metadata().unwrap().file_type().is_symlink(),
            "a real file per project, never a link to a shared one"
        );
        let doc: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        let env = &doc["mcp"]["corpus"]["environment"];
        assert_eq!(env[PROJECT_ENV].as_str(), Some("a"));
        assert_eq!(
            env[STORE_ENV].as_str(),
            Some(store.root().to_string_lossy().as_ref())
        );
        // Per-RUN identity must NOT be here: it is not a property of the
        // project, and a server that inherits a scope without an agent
        // identity is supposed to fail closed.
        assert!(env.get(AGENT_ENV).is_none(), "no per-run identity: {env}");
        assert!(env.get(RUN_LOG_ENV).is_none(), "no per-run identity: {env}");
    }

    /// A run dir for a project that does not exist is a contradiction —

    /// A walk of an empty corpus: totals zeroed, no categories, and the
    /// logs bucket present-but-empty.
    fn empty_stats() -> CorpusStats {
        CorpusStats {
            logs: CategoryStat { name: RUNS.to_string(), files: 0, bytes: 0 },
            ..CorpusStats::default()
        }
    }

    #[test]
    fn corpus_stats_counts_files_and_bytes() {
        let store = tmp_store("stats");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        assert_eq!(corpus_stats(&store, "p").unwrap(), empty_stats());
        write(&store.project_corpus_dir("p").join("findings/1.md"), "hello world\n");
        write(&store.project_corpus_dir("p").join("techniques/quote.md"), "abcd");
        // attack dirs are directories; their FILE contents count.
        write(&store.project_corpus_dir("p").join("attacks/attack-a/attack.md"), "body bytes\n");
        write(&store.project_corpus_dir("p").join("attacks/attack-a/run.sh"), "#!/bin/sh\n");
        let stats = corpus_stats(&store, "p").unwrap();
        assert_eq!(stats.files, 4);
        assert_eq!(
            stats.bytes,
            (12 + 4 + 11 + 10) as u64,
            "sum of exact byte lengths",
        );
        // Category attribution, CATEGORIES order, attack dirs descended.
        let names: Vec<&str> = stats.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["techniques", "findings", "attacks"]);
        assert_eq!(stats.categories[0].files, 1);
        assert_eq!(stats.categories[1].bytes, 12);
        assert_eq!(stats.categories[2].files, 2, "both attack files count");
        // a missing project corpus is empty, not an error
        assert_eq!(corpus_stats(&store, "ghost").unwrap(), empty_stats());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn mission_logs_are_split_out_of_the_categories() {
        let store = tmp_store("stats-logs");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let corpus = store.project_corpus_dir("p");
        write(&corpus.join("findings/1.md"), "hello world\n"); // 12
        write(&corpus.join("runs/1786891368-verify.raw"), "transcript\n"); // 11
        write(&corpus.join("runs/1786856299-discover.json"), "{}"); // 2
        // A file loose at the corpus root belongs to no category.
        write(&corpus.join("triage-report.md"), "note\n"); // 5

        let stats = corpus_stats(&store, "p").unwrap();
        assert_eq!(stats.files, 4, "grand total still counts every file");
        assert_eq!(stats.bytes, 30);
        assert_eq!(stats.knowledge_files(), 2, "logs excluded from the corpus count");
        assert_eq!(stats.knowledge_bytes(), 17);
        let names: Vec<&str> = stats.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["findings", "other"], "runs is never a category");
        assert_eq!(stats.logs.files, 2);
        assert_eq!(stats.logs.bytes, 13);

        // Listing: newest first, epoch/mission/kind parsed off the name.
        let logs = mission_logs(&store, "p").unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].name, "1786891368-verify.raw");
        assert_eq!(logs[0].mission, "verify");
        assert_eq!(logs[0].started, 1_786_891_368);
        assert_eq!(logs[0].kind, "raw");
        assert_eq!(logs[0].bytes, 11);
        assert_eq!(logs[1].mission, "discover");
        assert!(mission_logs(&store, "ghost").unwrap().is_empty(), "no runs dir is not an error");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn corpus_cost_aggregates_exports_per_model() {
        let store = tmp_store("cost");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let export = |cost: f64, input: u64, model: &str| {
            serde_json::json!({
                "info": {},
                "messages": [{"info": {
                    "role": "assistant",
                    "providerID": "openrouter",
                    "modelID": model,
                    "cost": cost,
                    "tokens": {
                        "total": input + 8,
                        "input": input,
                        "output": 7,
                        "reasoning": 1,
                        "cache": {"read": 3, "write": 0}
                    }
                }}]
            })
            .to_string()
        };
        let runs = store.project_corpus_dir("p").join("runs");
        write(&runs.join("1-operator.json"), &export(0.5, 100, "deepseek/deepseek-v4-flash"));
        write(&runs.join("2-operator.json"), &export(0.25, 50, "deepseek/deepseek-v4-flash"));
        write(&runs.join("3-operator.json"), &export(1.5, 200, "moonshotai/kimi-k3"));
        write(&runs.join("4-operator.log"), "not json — skipped");
        write(&runs.join("5-operator.json"), "{corrupt");
        let report = corpus_cost(&store, "p").unwrap();
        assert_eq!(report.rows.len(), 2);
        // Cost-desc order: kimi first.
        assert_eq!(report.rows[0].model, "kimi-k3");
        assert_eq!(report.rows[0].provider, "openrouter");
        assert!((report.rows[0].cost - 1.5).abs() < 1e-9);
        assert_eq!(report.rows[1].messages, 2);
        assert_eq!(report.rows[1].tokens_input, 150);
        assert_eq!(report.rows[1].cache_read, 6);
        assert!((report.cost - 2.25).abs() < 1e-9);
        assert_eq!(report.tokens, 108 + 58 + 208);
        assert!(corpus_cost(&store, "ghost").unwrap().rows.is_empty());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn corpus_cost_reexport_overwrites_not_doubles() {
        // A live conversation is re-exported every turn to the SAME
        // session-keyed file (`runs/<session-id>.json`), so its cumulative
        // usage must REPLACE the prior read, never stack on top of it.
        let store = tmp_store("cost-reexport");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let export = |total: u64, input: u64| {
            serde_json::json!({
                "info": {},
                "messages": [{"info": {
                    "role": "assistant",
                    "providerID": "ollama",
                    "modelID": "qwen/qwen3",
                    "cost": 0.0,
                    "tokens": {"total": total, "input": input, "output": 0,
                               "reasoning": 0, "cache": {"read": 0, "write": 0}}
                }}]
            })
            .to_string()
        };
        let runs = store.project_corpus_dir("p").join("runs");
        let file = runs.join("ses_abc.json");
        // Turn 1.
        write(&file, &export(100, 100));
        let r1 = corpus_cost(&store, "p").unwrap();
        assert_eq!(r1.tokens, 100);
        assert_eq!(r1.rows.len(), 1);
        // Turn 2: same session, cumulative totals, same filename → overwrite.
        write(&file, &export(250, 250));
        let r2 = corpus_cost(&store, "p").unwrap();
        assert_eq!(r2.tokens, 250, "re-export overwrote — must not be 100+250");
        assert_eq!(r2.rows.len(), 1);
        assert_eq!(r2.rows[0].tokens_input, 250);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn wipe_project_corpus_bumps_generation() {
        let store = tmp_store("wipe");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", crate::agents::AgentRole::Tester)
            .unwrap();
        write(&store.project_corpus_dir("p").join("findings/1.md"), "x\n");
        let p = store.wipe_project_corpus("p").unwrap();
        assert_eq!(p.corpus_generation, 1);
        assert!(!store.project_corpus_dir("p").join("findings/1.md").exists());
        assert!(store.project_corpus_dir("p").join("findings").is_dir());
        // agents survive a wipe
        assert!(store.project_agent_dir("p", "operator").join("opencode.json").is_file());
        let _ = fs::remove_dir_all(store.root());
    }
}
