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
//!                                 #   findings/ attacks/ runs/) — the ONLY
//!                                 #   corpus scope
//!     agents/<agent-slug>/        # agent configs: agent.yaml, opencode.json,
//!                                 #   prompts/
//!     missions/<mission>.md       # mission records (agent ref, pins, budget,
//!                                 #   status, created) + brief body
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

/// Project the flat store migrates into, and the unscoped default.
pub const DEFAULT_PROJECT_SLUG: &str = "default";

/// Environment variables overriding the default scope.
pub const STORE_ENV: &str = "CORPUS_STORE";
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

/// The corpus category layout.
pub const CATEGORIES: [&str; 5] = ["hypotheses", "techniques", "findings", "attacks", "runs"];

/// The mission-log category: a corpus dir like any other on disk, but
/// summarized on its own (see `CorpusStats`) — run transcripts dwarf the
/// knowledge categories and would swamp any shared byte breakdown.
pub const RUNS: &str = "runs";

/// Resolve the store root: `CORPUS_STORE`, else `~/Sites/corpus/store`.
pub fn store_root_env() -> PathBuf {
    std::env::var(STORE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            PathBuf::from(format!("{home}/Sites/corpus/store"))
        })
}

/// The current project scope: `CORPUS_PROJECT` else `default`.
pub fn project_slug_env() -> String {
    std::env::var(PROJECT_ENV).unwrap_or_else(|_| DEFAULT_PROJECT_SLUG.to_string())
}

/// The store: path plumbing over the scoped layout.
#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_env() -> Self {
        Self::new(store_root_env())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The core seed-agent set (read-only, versioned with the app).
    pub fn seed_agents_dir(&self) -> PathBuf {
        self.root.join("templates").join("agents")
    }

    pub fn projects_dir(&self) -> PathBuf {
        self.root.join("projects")
    }

    pub fn project_dir(&self, slug: &str) -> PathBuf {
        self.projects_dir().join(slug)
    }

    /// The project's opencode RUN directory (`store/projects/<p>/var/run/`):
    /// the working directory missions launch in, so each project owns its
    /// `.opencode/agent/` set and its opencode session pool (opencode keys
    /// sessions by cwd) — one project never overwrites another's
    /// materialized agents.
    pub fn project_run_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("var").join("run")
    }

    /// Provision a project's run directory: the real `.opencode/agent/`
    /// dir, symlinks to the repo-level opencode config (`opencode.json` —
    /// the MCP server block), `node_modules`/`skills` when present, and
    /// top-level `store` + `sources` symlinks so cwd-relative prompt
    /// paths and permission patterns (`store/projects/<p>/corpus/**`,
    /// `sources/<name>/<sha>`) resolve exactly as they do at repo root.
    /// Idempotent; safe to call on every launch.
    pub fn provision_run_dir(&self, slug: &str) -> Result<PathBuf> {
        let run_dir = self.project_run_dir(slug);
        let opencode = run_dir.join(".opencode");
        fs::create_dir_all(opencode.join("agent"))?;
        let Some(repo_root) = self.root().parent().map(Path::to_path_buf) else {
            return Ok(run_dir);
        };
        let repo_opencode = repo_root.join(".opencode");
        for name in ["opencode.json", "node_modules", "skills"] {
            link_if_useful(&repo_opencode.join(name), &opencode.join(name))?;
        }
        for name in ["store", "sources"] {
            link_if_useful(&repo_root.join(name), &run_dir.join(name))?;
        }
        Ok(run_dir)
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

    pub fn from_env() -> Self {
        Self::new(project_slug_env())
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
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(meta) = fs::metadata(&path) {
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
        if path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
        let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
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

/// Aggregate token/cost usage across a project's exported run
/// transcripts. Cheap (one parse per runs/*.json) and best-effort: an
/// unparseable file is skipped, never fatal — a corrupt export must not
/// blank the view.
pub fn corpus_cost(store: &Store, project: &str) -> Result<CostReport> {
    let runs = store.project_corpus_dir(project).join("runs");
    let mut report = CostReport::default();
    if !runs.is_dir() {
        return Ok(report);
    }
    let mut rows: std::collections::BTreeMap<(String, String), CostRow> =
        std::collections::BTreeMap::new();
    for entry in fs::read_dir(&runs)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = fs::read_to_string(&path) else { continue };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let Some(messages) = doc.get("messages").and_then(|m| m.as_array()) else {
            continue;
        };
        for message in messages {
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
    report.rows = rows.into_values().collect();
    report
        .rows
        .sort_by(|a, b| b.cost.partial_cmp(&a.cost).unwrap_or(std::cmp::Ordering::Equal));
    report.cost = report.rows.iter().map(|r| r.cost).sum();
    Ok(report)
}

/// A mission record (`store/projects/<p>/missions/<slug>.md`): the
/// launch unit. Frontmatter carries the agent ref + source pins + budget +
/// status + created; the markdown body is the mission brief.
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
    /// Lifecycle status (queued | running | done | stopped).
    #[serde(default)]
    pub status: String,
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
        self.seed_core_agents(slug)?;
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

    /// Delete a project (removes its whole subtree).
    pub fn delete_project(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let dir = self.project_dir(slug);
        if !dir.is_dir() {
            return Err(Error::Store(format!("project not found: {slug}")));
        }
        if slug == DEFAULT_PROJECT_SLUG {
            return Err(Error::Store("refusing to delete the default project".into()));
        }
        fs::remove_dir_all(&dir)?;
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

/// Symlink `target` to `link` when the target exists and the link does
/// not — used to wire a project's run dir back into the repo. A real dir
/// at `link` (e.g. the rendered `.opencode/agent`) is never replaced.
#[cfg(unix)]
fn link_if_useful(target: &Path, link: &Path) -> Result<()> {
    if !target.exists() || link.symlink_metadata().is_ok() {
        return Ok(());
    }
    std::os::unix::fs::symlink(target, link)?;
    Ok(())
}

#[cfg(not(unix))]
fn link_if_useful(_target: &Path, _link: &Path) -> Result<()> {
    Ok(()) // no symlinks — the run dir works without repo-relative paths
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

// -------------------------------------------------------------------------
// Flat-store migration
// -------------------------------------------------------------------------

/// What the flat-store migration did.
#[derive(Debug, Clone, Default)]
pub struct MigrationReport {
    /// Entries relocated and checksum-verified after the rename.
    pub moved: Vec<PathBuf>,
    /// Source entries whose scoped destination already held a copy — never
    /// overwritten, left where they were.
    pub skipped: Vec<PathBuf>,
    /// Entries that survived the rename but FAILED post-move checksum
    /// verification. Fetch these back up before anything is declared done.
    pub unverified: Vec<PathBuf>,
    /// Entries that would move in a dry run (nothing changed).
    pub would_move: Vec<PathBuf>,
    /// Legacy category directories removed — only ever after every entry
    /// they held moved with a verifiable checksum.
    pub removed_categories: Vec<String>,
    /// True when the report came from a dry run (no mutations).
    pub dry_run: bool,
}

/// Migration knobs.
#[derive(Debug, Clone, Default)]
pub struct MigrateOptions {
    /// Report only; make no changes to the store.
    pub dry_run: bool,
}

impl Store {
    /// See [`Store::migrate_legacy_flat_opt`].
    pub fn migrate_legacy_flat(&self, project: &str) -> Result<MigrationReport> {
        self.migrate_legacy_flat_opt(project, MigrateOptions::default())
    }

    /// Migrate the legacy flat `store/{categories}` layout into
    /// `store/projects/<project>/corpus/`. Hardened so this class of
    /// accident is impossible:
    ///
    /// - files move with a same-filesystem `rename` (atomic, byte-exact);
    /// - every moved entry is checksummed BEFORE the move; the destination
    ///   is checksummed AFTER it and compared — mismatches are reported in
    ///   `unverified`, never silently accepted;
    /// - a legacy category directory is removed only when EVERY entry it
    ///   held relocated with a verified checksum (or the dir was already
    ///   empty) — never when a move went unverified or an entry was skipped;
    /// - a pre-populated destination is never overwritten: the source entry
    ///   is left in place and reported as skipped;
    /// - `dry_run` reports every decision without changing the store.
    ///
    /// Also ensures the default project exists (skipped in dry run).
    pub fn migrate_legacy_flat_opt(
        &self,
        project: &str,
        opts: MigrateOptions,
    ) -> Result<MigrationReport> {
        let mut report = MigrationReport {
            dry_run: opts.dry_run,
            ..Default::default()
        };
        let project_missing = Project::load(self, project).is_err();
        if !opts.dry_run && project_missing {
            self.create_project(project, "Default corpus project", "cdk-regtest")?;
        }
        for category in CATEGORIES {
            let legacy_dir = self.root.join(category);
            if !legacy_dir.is_dir() {
                continue;
            }
            let mut entries: Vec<PathBuf> = fs::read_dir(&legacy_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            entries.sort();
            if entries.is_empty() {
                // Nothing to verify; the dir may go. Dry runs never mutate.
                if !opts.dry_run {
                    fs::remove_dir(&legacy_dir).ok();
                    report.removed_categories.push(category.to_string());
                }
                continue;
            }
            let dest_dir = self.project_corpus_dir(project).join(category);
            if !opts.dry_run {
                fs::create_dir_all(&dest_dir)?;
            }
            // The gate: only this dir may be dropped if EVERY entry it held
            // moved and checksum-verified (or was left in place as skipped).
            let mut category_verified = true;
            for entry in entries {
                let name = entry
                    .file_name()
                    .ok_or_else(|| Error::Store("bad entry name".into()))?;
                let target = dest_dir.join(&name);
                if target.exists() {
                    // Never overwrite: leave the source entry in place.
                    report.skipped.push(entry);
                    continue;
                }
                if opts.dry_run {
                    report.would_move.push(entry);
                    continue;
                }
                let expected = checksum(&entry)?;
                fs::rename(&entry, &target)?;
                let actual = checksum(&target)?;
                if actual != expected {
                    category_verified = false;
                    report.unverified.push(target);
                } else {
                    report.moved.push(target);
                }
            }
            if !opts.dry_run
                && category_verified
                && fs::read_dir(&legacy_dir)?.next().is_none()
            {
                fs::remove_dir(&legacy_dir).ok();
                report.removed_categories.push(category.to_string());
            }
        }
        Ok(report)
    }
}

// -------------------------------------------------------------------------
// Checksums (migration + clone verification)
// -------------------------------------------------------------------------

/// A content hash over an ENTIRE entry: a file's bytes, or — for attack
/// directories — every file beneath it (sorted, name-length-prefixed so
/// renames cannot paste different files onto the same checksum).
pub fn checksum(path: &Path) -> Result<String> {
    if !path.is_dir() {
        return Ok(fnv1a_hex(&fs::read(path)?));
    }
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files(path, path, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for (rel, abs) in &files {
        for b in rel.len().to_be_bytes() {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        for b in rel.as_bytes() {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let bytes = fs::read(abs)?;
        for b in &bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    Ok(format!("{hash:016x}"))
}

/// Collect `(relative_path, absolute_path)` for every file under `root`.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let abs = entry.path();
        let rel = abs
            .strip_prefix(root)
            .expect("collect_files dir is under root")
            .to_string_lossy()
            .into_owned();
        if abs.is_dir() {
            collect_files(root, &abs, out)?;
        } else {
            out.push((rel, abs));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_store(tag: &str) -> Store {
        let dir = std::env::temp_dir().join(format!("corpus-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        Store::new(dir)
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// A seed-less store: the temp root has no templates/agents to copy.
    /// create_project seeds the core pair, so tests that just need a project
    /// are fine; tests that need the seed pair present write seed dirs first.
    fn seed_sample(store: &Store) {
        let dir = store.seed_agents_dir();
        for slug in ["operator", "researcher"] {
            let d = dir.join(slug);
            fs::create_dir_all(&d).unwrap();
            fs::write(
                d.join("opencode.json"),
                format!(
                    "{{\"$schema\":\"https://opencode.ai/config.json\",\"agent\":{{\"{slug}\":{{\"description\":\"{slug}\",\"mode\":\"primary\",\"prompt\":\"You are {slug}.\\n\"}}}}}}"
                ),
            )
            .unwrap();
        }
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

    #[test]
    fn project_crud() {
        let store = tmp_store("proj");
        seed_sample(&store);
        let project = store.create_project("cdk-a", "CDK team A", "cdk-regtest").unwrap();
        assert_eq!(project.plugin, "cdk-regtest");
        assert_eq!(project.corpus_generation, 0);
        assert!(store.project_dir("cdk-a").join("project.yaml").is_file());
        assert!(store.project_corpus_dir("cdk-a").join("findings").is_dir());
        // create_project seeds the core agent pair.
        assert!(store.project_agent_dir("cdk-a", "operator").join("opencode.json").is_file());
        assert!(store.project_agent_dir("cdk-a", "researcher").join("opencode.json").is_file());
        // duplicate create fails
        assert!(store.create_project("cdk-a", "x", "y").is_err());
        // list
        let projects = store.list_projects().unwrap();
        assert_eq!(projects.len(), 1);
        // clone without corpus carries agents + missions.
        store
            .write_mission(
                "cdk-a",
                "m1",
                &Mission {
                    agent: "operator".to_string(),
                    pins: BTreeMap::new(),
                    budget: None,
                    status: "queued".to_string(),
                    created: 1,
                    name: None,
                    session: None,
                    opencode_session: None,
                },
                "probe",
            )
            .unwrap();
        // A distinctly-named agent, so the clone's agent set is checkable
        // against the source's rather than against the seeded pair.
        store.create_agent_from_seed("cdk-a", "hunter", "researcher").unwrap();
        store.clone_project("cdk-a", "cdk-b", None, false).unwrap();
        let b = Project::load(&store, "cdk-b").unwrap();
        assert_eq!(b.cloned_from.as_deref(), Some("cdk-a"));
        assert!(store.project_agent_dir("cdk-b", "operator").join("opencode.json").is_file());
        assert!(store.load_mission("cdk-b", "m1").is_ok(), "clone carries missions");
        // A clone MIRRORS its source: same agents, no seeded strays.
        let names = |p: &str| -> Vec<String> {
            store.list_agents(p).unwrap().into_iter().map(|(s, _)| s).collect()
        };
        assert_eq!(names("cdk-b"), names("cdk-a"), "clone carries exactly the source's agents");
        assert!(names("cdk-b").contains(&"hunter".to_string()));
        // delete
        store.delete_project("cdk-b").unwrap();
        assert!(Project::load(&store, "cdk-b").is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn app_prefs_round_trip_and_never_fail_the_launch() {
        let store = tmp_store("prefs");
        // Nothing on disk yet: defaults, no error — a first launch has no
        // remembered model and the panel simply stays idle.
        assert_eq!(store.load_prefs().chat_model, "");
        store
            .save_prefs(&AppPrefs { chat_model: "qwen3.8:27b-mlx".into() })
            .unwrap();
        assert_eq!(store.load_prefs().chat_model, "qwen3.8:27b-mlx");
        // An unknown field (a newer app's file) still loads.
        write(&store.root().join("app.yaml"), "chat_model: m\nfuture_thing: 3\n");
        assert_eq!(store.load_prefs().chat_model, "m");
        // Garbage degrades to defaults rather than refusing to open.
        write(&store.root().join("app.yaml"), "\t not: [yaml");
        assert_eq!(store.load_prefs().chat_model, "");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn rename_project_moves_the_label_not_the_slug() {
        let store = tmp_store("rename-proj");
        seed_sample(&store);
        store.create_project("cdk-a", "CDK team A", "cdk-regtest").unwrap();
        store.create_agent_from_seed("cdk-a", "hunter", "researcher").unwrap();
        let renamed = store.rename_project("cdk-a", "  Local Runner  ").unwrap();
        assert_eq!(renamed.name, "Local Runner", "the name is trimmed");
        // The slug — and therefore every path filed under it — is untouched.
        assert!(store.project_dir("cdk-a").is_dir());
        assert!(store.project_agent_dir("cdk-a", "hunter").join("opencode.json").is_file());
        let reloaded = Project::load(&store, "cdk-a").unwrap();
        assert_eq!(reloaded.name, "Local Runner");
        assert_eq!(reloaded.plugin, "cdk-regtest", "rename touches nothing else");
        // An empty (or whitespace) name is refused rather than blanking the
        // label — the sidebar would fall back to showing the raw slug.
        assert!(store.rename_project("cdk-a", "   ").is_err());
        assert!(store.rename_project("no-such-project", "X").is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn mission_roundtrip() {
        let store = tmp_store("mission");
        seed_sample(&store);
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let mut pins = BTreeMap::new();
        pins.insert("cdk".to_string(), "main".to_string());
        store
            .write_mission(
                "p",
                "m1",
                &Mission {
                    agent: "operator".to_string(),
                    pins: pins.clone(),
                    budget: Some("40m / 10k$".to_string()),
                    status: "running".to_string(),
                    created: 42,
                    name: None,
                    session: None,
                    opencode_session: None,
                },
                "# Probe the environment\nPlan: map surfaces.\n",
            )
            .unwrap();
        let m = store.load_mission("p", "m1").unwrap();
        assert_eq!(m.agent, "operator");
        assert_eq!(m.budget.as_deref(), Some("40m / 10k$"));
        assert_eq!(m.pins.get("cdk").map(String::as_str), Some("main"));
        assert!(store.mission_brief("p", "m1").unwrap().contains("Probe the environment"));
        let list = store.list_missions("p").unwrap();
        assert_eq!(list.len(), 1);
        // dangling agent ref is refused.
        let err = store
            .write_mission(
                "p",
                "bad",
                &Mission {
                    agent: "ghost".to_string(),
                    pins: BTreeMap::new(),
                    budget: None,
                    status: "queued".to_string(),
                    created: 0,
                    name: None,
                    session: None,
                    opencode_session: None,
                },
                "x",
            )
            .unwrap_err();
        assert!(err.to_string().contains("ghost"), "{err}");
        store.delete_mission("p", "m1").unwrap();
        assert!(store.load_mission("p", "m1").is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn migrate_relocates_bytes_verbatim() {
        let store = tmp_store("migrate");
        seed_sample(&store);
        // legacy flat layout with bytes that matter
        write(&store.root().join("findings/1786000000-theft.md"), "---\ntitle: T\nseverity: high\n---\n\nsecret bytes\n");
        write(&store.root().join("techniques/quote-race.md"), "# quote race\nwith leading em-dash — and trailing.\n");
        write(&store.root().join("attacks/quote-id-front-run/attack.md"), "# Attack\n\nbody\n");
        write(&store.root().join("attacks/quote-id-front-run/run.sh"), "#!/bin/sh\necho hi\n");
        write(&store.root().join("runs/1700000000-op-run.log"), "# run\npayload\n");
        write(&store.root().join("hypotheses/lead.md"), "---\nstatus: open\n---\n\nbody\n");

        let mut before = std::collections::HashMap::new();
        for (cat, name) in [
            ("findings", "1786000000-theft.md"),
            ("techniques", "quote-race.md"),
            ("attacks", "quote-id-front-run/attack.md"),
            ("attacks", "quote-id-front-run/run.sh"),
            ("runs", "1700000000-op-run.log"),
            ("hypotheses", "lead.md"),
        ] {
            let bytes = fs::read(store.root().join(cat).join(name)).unwrap();
            before.insert(format!("{cat}/{name}"), bytes);
        }

        let report = store.migrate_legacy_flat(DEFAULT_PROJECT_SLUG).unwrap();
        assert_eq!(report.moved.len(), 5);
        assert!(report.unverified.is_empty(), "no unverified moves: {:?}", report.unverified);
        assert_eq!(report.removed_categories.len(), 5, "all legacy dirs dropped once verified");
        assert!(!report.dry_run);
        for cat in CATEGORIES {
            assert!(!store.root().join(cat).exists(), "{cat} should be empty/gone");
        }
        let target = store.project_corpus_dir(DEFAULT_PROJECT_SLUG);
        for (rel, expected) in &before {
            let path = target.join(rel);
            assert!(path.is_file(), "missing {}", path.display());
            assert_eq!(&fs::read(&path).unwrap(), expected, "byte identity of {rel}");
        }
        // idempotent second run
        let second = store.migrate_legacy_flat(DEFAULT_PROJECT_SLUG).unwrap();
        assert_eq!(second.moved.len(), 0);
        let _ = fs::remove_dir_all(store.root());
    }

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
        seed_sample(&store);
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
        seed_sample(&store);
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
        seed_sample(&store);
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
    fn wipe_project_corpus_bumps_generation() {
        let store = tmp_store("wipe");
        seed_sample(&store);
        store.create_project("p", "P", "cdk-regtest").unwrap();
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