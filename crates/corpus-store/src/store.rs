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
pub const ENVIRONMENT_SESSION_ENV: &str = "CORPUS_ENVIRONMENT_SESSION";
/// The mission slug for this exact run. Paired with [`RUN_ID_ENV`] so a
/// project-management call can record which Curator mission dispatched work.
pub const MISSION_ENV: &str = "CORPUS_MISSION";
/// Exact launcher session identity. TUI runs use their persisted tmux session
/// name; the no-tmux fallback uses its unique transcript basename.
pub const RUN_ID_ENV: &str = "CORPUS_RUN_ID";

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
pub const CATEGORIES: [&str; 6] = [
    "hypotheses",
    "techniques",
    "findings",
    "attacks",
    "retro",
    "runs",
];

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

    /// Corpus-owned source cache paired with this store instance. Tests and
    /// alternate stores stay in their own world instead of touching the
    /// operator's default home.
    pub fn source_cache_dir(&self) -> PathBuf {
        if let Some(explicit) = std::env::var(crate::paths::SOURCES_DIR_ENV)
            .ok()
            .filter(|value| !value.is_empty())
        {
            return PathBuf::from(explicit);
        }
        self.root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| self.root.clone())
            .join("cache/sources")
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
        Project::load(self, slug)?;
        let run_dir = self.project_run_dir(slug);
        let opencode = run_dir.join(".opencode");
        fs::create_dir_all(opencode.join("agent"))?;
        fs::create_dir_all(run_dir.join("store").join("projects"))?;

        relink(
            &self.project_dir(slug),
            &run_dir.join("store").join("projects").join(slug),
        )?;

        // Pinned source trees: REQUIRED. A run without them reads nothing
        // and quietly falls back to whatever the prompt says about source
        // paths, auditing a tree nobody chose. The directory itself
        // is a fetch cache (`srcrev` fills it), so create it rather than
        // refusing a machine that simply hasn't fetched yet — but the
        // Source custody is corpus data, not a shipped-resource concern.
        let sources = self.source_cache_dir();
        fs::create_dir_all(&sources)?;
        relink(&sources, &run_dir.join("sources"))?;
        // Skills are optional — not every install ships them.
        if let Some(resources) = crate::paths::resource_root_opt() {
            let skills = resources.join(".opencode").join("skills");
            if skills.exists() {
                relink(&skills, &opencode.join("skills"))?;
            }
        }
        // NOT node_modules: opencode creates its own in the run dir at
        // runtime, and a symlink placed first would win and then rot.

        self.write_run_opencode_config(slug, &opencode)?;
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
    fn write_run_opencode_config(
        &self,
        slug: &str,
        opencode: &Path,
    ) -> Result<()> {
        let mcp_bin = crate::paths::corpus_mcp_bin()?;
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

    /// Derived accounting state. Kept outside `corpus/`: usage snapshots are
    /// application bookkeeping, not curator-authored research artifacts.
    pub fn project_usage_dir(&self, slug: &str) -> PathBuf {
        self.project_dir(slug).join("usage")
    }

    pub fn write_usage_snapshot(&self, project: &str, snapshot: &UsageSnapshot) -> Result<PathBuf> {
        validate_slug(project)?;
        if snapshot.session_id.is_empty()
            || snapshot.session_id.contains('/')
            || snapshot.session_id.contains('\\')
        {
            return Err(Error::Store("usage snapshot has an invalid session id".into()));
        }
        let dir = self.project_usage_dir(project);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", snapshot.session_id));
        let temporary = dir.join(format!(".{}.json.tmp", snapshot.session_id));
        let bytes = serde_json::to_vec_pretty(snapshot)
            .map_err(|error| Error::Store(format!("usage snapshot: {error}")))?;
        fs::write(&temporary, bytes)?;
        fs::rename(&temporary, &path)?;
        Ok(path)
    }

    /// One-time compatibility migration. Existing transcript exports are
    /// reduced to compact snapshots; message data is never needed again for
    /// those sessions after this succeeds.
    pub fn backfill_usage_snapshots(&self, project: &str) -> Result<usize> {
        let runs = self.project_corpus_dir(project).join("runs");
        if !runs.is_dir() {
            return Ok(0);
        }
        let mut written = 0;
        for entry in fs::read_dir(runs)? {
            let entry = entry?;
            let path = entry.path();
            let Some(session_id) = path.file_stem().and_then(|value| value.to_str()) else {
                continue;
            };
            if path.extension().and_then(|value| value.to_str()) != Some("json")
                || self.project_usage_dir(project).join(format!("{session_id}.json")).is_file()
            {
                continue;
            }
            let report = parse_cost_file(&path);
            if report.rows.is_empty() {
                continue;
            }
            let captured_at = entry.metadata().ok()
                .and_then(|meta| meta.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            self.write_usage_snapshot(project, &UsageSnapshot {
                version: USAGE_SNAPSHOT_VERSION,
                session_id: session_id.to_string(),
                captured_at,
                source: "legacy-transcript".into(),
                rows: report.rows,
            })?;
            written += 1;
        }
        Ok(written)
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
    /// Durable lifecycle intent consumed by the desktop app. The project
    /// remains present until every mission environment has been torn down.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_requested: Option<MissionDeleteRequest>,
}

impl Project {
    pub fn load(store: &Store, slug: &str) -> Result<Self> {
        let path = store.project_dir(slug).join("project.yaml");
        let raw = fs::read_to_string(&path)
            .map_err(|_| Error::Store(format!("project not found: {slug}")))?;
        let project: Project =
            serde_yaml::from_str(&raw).map_err(|e| Error::Store(format!("project {slug}: {e}")))?;
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
                CategoryStat {
                    name: c.to_string(),
                    files: 0,
                    bytes: 0,
                },
            )
        })
        .collect();
    if root.is_dir() {
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let path = entry.path();
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
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
                    let slot =
                        by_name
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
    stats.logs = by_name.remove(RUNS).unwrap_or_else(|| CategoryStat {
        name: RUNS.to_string(),
        ..CategoryStat::default()
    });
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
    /// The agent slug parsed out of `<epoch>-<agent>.<ext>` or resolved from
    /// a mission whose OpenCode session owns a session-keyed JSON export.
    /// `None` for legacy/unlinked filenames that carry no agent identity.
    pub agent: Option<String>,
    /// Run-start epoch seconds from the name prefix (0 when absent).
    pub started: u64,
    pub bytes: u64,
    /// Extension: `raw` (piped transcript), `json` (opencode export).
    pub kind: String,
}

/// List a project's mission logs, newest first. Mission records are read once
/// to map session-keyed exports back to their agent; run contents are never
/// parsed. Only regular files directly under `runs/` count, matching what the
/// stats walk attributes to the logs bucket.
pub fn mission_logs(store: &Store, project: &str) -> Result<Vec<MissionLog>> {
    let runs = store.project_corpus_dir(project).join(RUNS);
    let mut logs = Vec::new();
    if !runs.is_dir() {
        return Ok(logs);
    }
    let session_agents: BTreeMap<String, String> = store
        .list_missions(project)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(_, mission)| {
            mission
                .opencode_session
                .map(|session| (session, mission.agent))
        })
        .collect();
    for entry in fs::read_dir(&runs)? {
        let entry = entry?;
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let kind = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or(name);
        // `<epoch>-<mission>`: split once, and only when the prefix really
        // is a number (a mission named `2fa-probe` must not lose its head).
        let (started, agent) = match stem.split_once('-') {
            Some((head, rest)) if !rest.is_empty() => match head.parse::<u64>() {
                Ok(epoch) => (epoch, Some(rest.to_string())),
                Err(_) => (0, session_agents.get(stem).cloned()),
            },
            _ => (0, session_agents.get(stem).cloned()),
        };
        logs.push(MissionLog {
            name: name.to_string(),
            agent,
            started,
            bytes,
            kind,
        });
    }
    logs.sort_by(|a, b| b.started.cmp(&a.started).then_with(|| a.name.cmp(&b.name)));
    Ok(logs)
}

/// Usage aggregation for one (provider, model) pair, summed over every
/// exported run transcript in the project corpus.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    /// Wall-clock milliseconds spent awaiting the model/provider. Derived
    /// from the assistant message span with tool execution intervals removed.
    pub inference_ms: u64,
    /// Assistant messages with complete timing data. Kept separate from
    /// `messages` because historical exports may not carry timestamps.
    pub timed_messages: u64,
    /// USD, as reported by opencode's export.
    pub cost: f64,
}

pub const USAGE_SNAPSHOT_VERSION: u32 = 1;

/// Compact cumulative accounting for one OpenCode session. It is replaced at
/// every completed turn and is sufficient to calculate project spend without
/// retaining or parsing message text.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub version: u32,
    pub session_id: String,
    pub captured_at: u64,
    pub source: String,
    pub rows: Vec<CostRow>,
}

impl UsageSnapshot {
    pub fn report(&self) -> CostReport {
        let mut report = CostReport::default();
        report.rows = self.rows.clone();
        report.tokens = report.rows.iter().map(|row| {
            row.tokens_input.saturating_add(row.tokens_output).saturating_add(row.tokens_reasoning)
        }).sum();
        report.inference_ms = report.rows.iter().map(|row| row.inference_ms).sum();
        report.timed_messages = report.rows.iter().map(|row| row.timed_messages).sum();
        report.cost = report.rows.iter().map(|row| row.cost).sum();
        report
    }
}

/// The project view's Cost section: per-model rows (cost desc) + totals.
/// Source data: `runs/<epoch>-<agent>.json` opencode exports (piped
/// `.log` transcripts carry no usage; they are simply not counted).
#[derive(Debug, Clone, Default)]
pub struct CostReport {
    pub rows: Vec<CostRow>,
    pub tokens: u64,
    pub inference_ms: u64,
    pub timed_messages: u64,
    pub cost: f64,
    pub accounted_sessions: u64,
    pub legacy_sessions: u64,
    pub last_updated: Option<u64>,
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
    let usage = store.project_usage_dir(project);
    let runs = store.project_corpus_dir(project).join("runs");
    if !usage.is_dir() && !runs.is_dir() {
        cache.files.clear();
        return Ok(CostReport::default());
    }
    let mut seen = std::collections::BTreeSet::new();
    // Snapshots are authoritative per session. Legacy exports remain a
    // compatibility fallback only for session ids not yet backfilled.
    let mut snapshotted = std::collections::BTreeSet::new();
    let usage_entries = usage.is_dir().then(|| fs::read_dir(&usage)).transpose()?;
    for entry in usage_entries.into_iter().flatten() {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|value| value.to_str()) {
            snapshotted.insert(stem.to_string());
        }
        cache_cost_path(cache, &mut seen, path, parse_snapshot_file);
    }
    let run_entries = runs.is_dir().then(|| fs::read_dir(&runs)).transpose()?;
    for entry in run_entries.into_iter().flatten() {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if path
            .file_stem()
            .and_then(|value| value.to_str())
            .is_some_and(|id| snapshotted.contains(id))
        {
            continue;
        }
        cache_cost_path(cache, &mut seen, path, parse_cost_file);
    }
    cache.files.retain(|path, _| seen.contains(path));
    Ok(merge_cost_reports(cache.files.values().map(|cached| &cached.report)))
}

fn cache_cost_path(
    cache: &mut CorpusCostCache,
    seen: &mut std::collections::BTreeSet<PathBuf>,
    path: PathBuf,
    parse: fn(&Path) -> CostReport,
) {
    let Ok(meta) = fs::metadata(&path) else { return };
    let modified = meta.modified().ok();
    let len = meta.len();
    seen.insert(path.clone());
    let current = cache
        .files
        .get(&path)
        .is_some_and(|cached| cached.modified == modified && cached.len == len);
    if !current {
        let report = parse(&path);
        cache.files.insert(
            path,
            CachedCostFile {
                modified,
                len,
                report,
            },
        );
    }
}

fn parse_snapshot_file(path: &Path) -> CostReport {
    fs::read(path).ok()
        .and_then(|raw| serde_json::from_slice::<UsageSnapshot>(&raw).ok())
        .filter(|snapshot| snapshot.version == USAGE_SNAPSHOT_VERSION)
        .map(|snapshot| {
            let legacy = snapshot.source == "legacy-transcript";
            let captured_at = snapshot.captured_at;
            let mut report = snapshot.report();
            report.accounted_sessions = 1;
            report.legacy_sessions = u64::from(legacy);
            report.last_updated = Some(captured_at);
            report
        })
        .unwrap_or_default()
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
                let row = rows
                    .entry((provider.clone(), model.clone()))
                    .or_insert_with(|| CostRow {
                        provider,
                        model,
                        ..CostRow::default()
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
                if let Some(inference_ms) = message_inference_ms(message) {
                    row.inference_ms = row.inference_ms.saturating_add(inference_ms);
                    row.timed_messages += 1;
                    report.inference_ms = report.inference_ms.saturating_add(inference_ms);
                    report.timed_messages += 1;
                }
                row.cost += info.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0);
                report.tokens = report.tokens.saturating_add(
                    take(&tokens, "input")
                        .saturating_add(take(&tokens, "output"))
                        .saturating_add(take(&tokens, "reasoning")),
                );
            }
        }
    }
    report.rows = rows.into_values().collect();
    report.cost = report.rows.iter().map(|row| row.cost).sum();
    if !report.rows.is_empty() {
        report.accounted_sessions = 1;
        report.legacy_sessions = 1;
    }
    report
}

/// OpenCode's assistant message clock covers both provider work and any tool
/// calls emitted by that response. Remove the union of tool intervals so
/// parallel tools are not double-subtracted. The remainder is measured
/// end-to-end model/provider time (queueing, prefill, and generation), not an
/// estimate based on a nominal tokens-per-second rate.
fn message_inference_ms(message: &serde_json::Value) -> Option<u64> {
    let time = message.get("info")?.get("time")?;
    let created = time.get("created")?.as_u64()?;
    let completed = time.get("completed")?.as_u64()?;
    if completed < created {
        return None;
    }

    let mut tool_intervals = message
        .get("parts")
        .and_then(|parts| parts.as_array())
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(|kind| kind.as_str()) == Some("tool"))
        .filter_map(|part| {
            let time = part.get("state")?.get("time")?;
            let start = time.get("start")?.as_u64()?.max(created);
            let end = time.get("end")?.as_u64()?.min(completed);
            (end > start).then_some((start, end))
        })
        .collect::<Vec<_>>();
    tool_intervals.sort_unstable();

    let mut tool_ms = 0_u64;
    let mut merged: Option<(u64, u64)> = None;
    for (start, end) in tool_intervals {
        match merged {
            Some((merged_start, merged_end)) if start <= merged_end => {
                merged = Some((merged_start, merged_end.max(end)));
            }
            Some((merged_start, merged_end)) => {
                tool_ms = tool_ms.saturating_add(merged_end - merged_start);
                merged = Some((start, end));
            }
            None => merged = Some((start, end)),
        }
    }
    if let Some((start, end)) = merged {
        tool_ms = tool_ms.saturating_add(end - start);
    }

    Some((completed - created).saturating_sub(tool_ms))
}

fn merge_cost_reports<'a>(reports: impl Iterator<Item = &'a CostReport>) -> CostReport {
    let mut report = CostReport::default();
    let mut rows = std::collections::BTreeMap::<(String, String), CostRow>::new();
    for source in reports {
        report.tokens += source.tokens;
        report.accounted_sessions += source.accounted_sessions;
        report.legacy_sessions += source.legacy_sessions;
        report.last_updated = report.last_updated.max(source.last_updated);
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
            row.inference_ms += source_row.inference_ms;
            row.timed_messages += source_row.timed_messages;
            row.cost += source_row.cost;
        }
    }
    report.rows = rows.into_values().collect();
    report.inference_ms = report.rows.iter().map(|row| row.inference_ms).sum();
    report.timed_messages = report.rows.iter().map(|row| row.timed_messages).sum();
    report.rows.sort_by(|a, b| {
        b.cost
            .partial_cmp(&a.cost)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
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
/// Exact project mission/run that requested another mission's launch.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MissionRunRef {
    pub project: String,
    pub mission: String,
    pub run_id: String,
}

/// Private loopback control endpoint for the exact OpenCode process that
/// owns this mission's conversation. Its per-run password is deliberately
/// kept outside project-visible mission metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionControl {
    pub run_id: String,
    pub port: u16,
}

/// Durable request consumed by the app's launch reconciler. The custom
/// deserializer accepts the historical integer timestamp, so existing mission
/// records remain valid without a rewrite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLaunchRequest {
    pub requested_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_by: Option<MissionRunRef>,
}

/// Durable request consumed by the app's lifecycle reconciler. Deletion is
/// intentionally a request rather than an immediate store mutation because
/// only the app owns tmux and plugin-environment teardown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionDeleteRequest {
    pub requested_at: u64,
}

/// Terminal outcome observed for one Curator/Super-dispatched child run.
/// This is operational routing state, not corpus research output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MissionCompletion {
    Completed { at: u64 },
    LaunchFailed { at: u64, error: String },
    UnexpectedExit { at: u64 },
}

/// Durable supervision state for the current dispatched child run. The
/// parent is launcher-proven, and `child_run_id` is filled only after spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissionDispatch {
    pub parent: MissionRunRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    #[serde(default)]
    pub live_seen: bool,
    /// The exact child OpenCode process reported this conversation active at
    /// least once. PTY output alone is deliberately insufficient evidence.
    #[serde(default)]
    pub running_seen: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<MissionCompletion>,
    /// Number of distinct continuation prompts admitted for this result.
    /// Each attempt receives a new message id; replaying an id only proves
    /// admission and cannot restart an OpenCode loop that already failed.
    #[serde(default)]
    pub delivery_attempt: u32,
    /// The currently admitted continuation prompt. `delivered` remains false
    /// until the owning curator loop parks after a successful model step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_message_id: Option<String>,
    #[serde(default)]
    pub delivered: bool,
}

impl<'de> Deserialize<'de> for MissionLaunchRequest {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Legacy(u64),
            Current {
                requested_at: u64,
                #[serde(default)]
                requested_by: Option<MissionRunRef>,
            },
        }

        Ok(match Wire::deserialize(deserializer)? {
            Wire::Legacy(requested_at) => Self {
                requested_at,
                requested_by: None,
            },
            Wire::Current {
                requested_at,
                requested_by,
            } => Self {
                requested_at,
                requested_by,
            },
        })
    }
}

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
    /// The tmux session the mission's run is attached to (`corpus-<run-stem>-<ts>`),
    /// when live — re-attach after an app relaunch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Exact app-launched TUI endpoint that can durably queue input for this
    /// run. Legacy, piped, and operator-started sessions leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<MissionControl>,
    /// The opencode session id (transcript of record) — export-on-stop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opencode_session: Option<String>,
    /// Durable plugin environment session bound to this mission launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_session: Option<String>,
    /// A launch request the app has not yet honored. Historical records stored
    /// only an epoch integer; they deserialize as an origin-less request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_requested: Option<MissionLaunchRequest>,
    /// A deletion request the app has not yet completed. The mission record
    /// remains present until all run and environment cleanup succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delete_requested: Option<MissionDeleteRequest>,
    /// Durable routing and completion state for a Curator/Super-dispatched
    /// child. Operator and legacy launches leave this unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch: Option<MissionDispatch>,
}

impl Mission {
    pub fn load(store: &Store, project: &str, slug: &str) -> Result<Self> {
        let path = store
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
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

// -------------------------------------------------------------------------
// Projects
// -------------------------------------------------------------------------

impl Store {
    /// Create an empty project: `store/projects/<slug>/` with corpus category,
    /// agent, and mission directories. Agents are added explicitly by role.
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
            delete_requested: None,
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
        // Preflight the entire cascade before removing anything. A partial
        // project deletion would be worse than a refusal because it could
        // erase the mission identity needed to retry environment cleanup.
        for (mission, _) in self.list_missions(slug)? {
            self.ensure_mission_deletable(slug, &mission)?;
        }
        fs::remove_dir_all(&dir)?;
        // The run dir is a sibling of the store now, so deleting the
        // project no longer takes it with it. A left-behind run dir holds
        // a dangling project link and a generated opencode.json naming a
        // project that is gone.
        let _ = fs::remove_dir_all(self.project_run_dir(slug));
        Ok(())
    }

    /// Persist a project deletion request for the app lifecycle reconciler.
    pub fn request_project_delete(&self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let mut project = Project::load(self, slug)?;
        project.delete_requested.get_or_insert(MissionDeleteRequest {
            requested_at: now_epoch(),
        });
        project.save(self, slug)
    }

    /// Clone a project: config + agents + missions, corpus copy optional.
    /// The clone mirrors its source exactly.
    pub fn clone_project(
        &self,
        from: &str,
        to: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<Project> {
        let source = Project::load(self, from)?;
        if source.delete_requested.is_some()
            || self
                .list_agents(from)?
                .iter()
                .any(|(_, agent)| agent.meta.delete_requested.is_some())
        {
            return Err(Error::Store(format!(
                "project {from} or one of its agents is pending deletion"
            )));
        }
        self.create_project(to, name.unwrap_or(&source.name), &source.plugin)?;
        let project = Project {
            name: name.unwrap_or(&source.name).to_string(),
            cloned_from: Some(from.to_string()),
            delete_requested: None,
            ..source
        };
        project.save(self, to)?;
        copy_tree(&self.project_agents_dir(from), &self.project_agents_dir(to))?;
        copy_tree(
            &self.project_missions_dir(from),
            &self.project_missions_dir(to),
        )?;
        if with_corpus {
            copy_tree(&self.project_corpus_dir(from), &self.project_corpus_dir(to))?;
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
    pub fn set_project_pins(&self, slug: &str, pins: BTreeMap<String, String>) -> Result<Project> {
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
        let root = self
            .project_corpus_dir(project)
            .canonicalize()
            .map_err(|e| {
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
        let project_record = Project::load(self, project)
            .map_err(|_| Error::Store(format!("project not found: {project}")))?;
        if project_record.delete_requested.is_some() {
            return Err(Error::Store(format!(
                "project {project} is pending deletion"
            )));
        }
        // The agent ref must resolve on the project.
        let agent = self.load_agent(project, &mission.agent)
            .map_err(|e| Error::Store(format!("mission {slug}: agent {:?}: {e}", mission.agent)))?;
        if agent.meta.delete_requested.is_some() {
            return Err(Error::Store(format!(
                "agent {project}/{} is pending deletion",
                mission.agent
            )));
        }
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
        let path = self
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
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
        self.ensure_mission_deletable(project, slug)?;
        self.delete_mission_record(project, slug)
    }

    /// Refuse to discard the durable identity needed to retry cleanup. This
    /// is the last line of defence for direct CLI/admin calls and cascades;
    /// lifecycle-aware callers clear the tmux binding and close the lease
    /// before reaching this primitive.
    pub fn ensure_mission_deletable(&self, project: &str, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        let mission = self.load_mission(project, slug)?;
        if mission.session.is_some() {
            return Err(Error::Store(format!(
                "mission {project}/{slug} still has a run session; request lifecycle teardown first"
            )));
        }
        if let Some(key) = mission.environment_session.as_deref() {
            let plugin = Project::load(self, project)?.plugin;
            let record = self
                .load_environment_session_key(&plugin, key)
                .map_err(|error| {
                    Error::Store(format!(
                        "mission {project}/{slug} still has environment cleanup identity {key}: {error}"
                    ))
                })?;
            if record.state != crate::EnvironmentSessionState::Closed {
                return Err(Error::Store(format!(
                    "mission {project}/{slug} environment is {}; request lifecycle teardown first",
                    format!("{:?}", record.state).to_ascii_lowercase()
                )));
            }
        }
        Ok(())
    }

    fn delete_mission_record(&self, project: &str, slug: &str) -> Result<()> {
        let path = self
            .project_missions_dir(project)
            .join(format!("{slug}.md"));
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
        // This is an update of a record proven to exist above, not mission
        // authoring. Historical versions allowed deleting an agent without
        // its missions, so teardown bookkeeping must remain able to update
        // those orphan records long enough to cleanly delete them. New
        // missions still go through write_mission and require a live agent.
        mission.save(self, project, slug, &brief)
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
        let world = std::env::temp_dir().join(format!("corpus-store-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&world);
        Store::new(world.join("store"))
    }

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    #[test]
    fn validate_slug_rejects_bad_names() {
        for bad in [
            "",
            "..",
            "a/b",
            "Upper",
            "under_score",
            "-lead",
            "trail-",
            "a b",
        ] {
            assert!(validate_slug(bad).is_err(), "should reject {bad:?}");
        }
        for good in ["default", "cdk-regtest", "red-alpha-2"] {
            assert!(validate_slug(good).is_ok(), "should accept {good:?}");
        }
    }

    #[test]
    fn projects_start_empty_and_clones_mirror_only_declared_agents() {
        let store = tmp_store("project-agents");
        store.create_project("source", "Source", "cdk-regtest").unwrap();
        assert!(store.list_agents("source").unwrap().is_empty());

        store
            .create_agent_with_role("source", "analyst", crate::agents::AgentRole::Researcher)
            .unwrap();
        store
            .clone_project("source", "clone", None, false)
            .unwrap();

        let agents: Vec<String> = store
            .list_agents("clone")
            .unwrap()
            .into_iter()
            .map(|(slug, _)| slug)
            .collect();
        assert_eq!(agents, ["analyst"]);
        let _ = fs::remove_dir_all(store.root());
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
            logs: CategoryStat {
                name: RUNS.to_string(),
                files: 0,
                bytes: 0,
            },
            ..CorpusStats::default()
        }
    }

    #[test]
    fn corpus_stats_counts_files_and_bytes() {
        let store = tmp_store("stats");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        assert_eq!(corpus_stats(&store, "p").unwrap(), empty_stats());
        write(
            &store.project_corpus_dir("p").join("findings/1.md"),
            "hello world\n",
        );
        write(
            &store.project_corpus_dir("p").join("techniques/quote.md"),
            "abcd",
        );
        // attack dirs are directories; their FILE contents count.
        write(
            &store
                .project_corpus_dir("p")
                .join("attacks/attack-a/attack.md"),
            "body bytes\n",
        );
        write(
            &store
                .project_corpus_dir("p")
                .join("attacks/attack-a/run.sh"),
            "#!/bin/sh\n",
        );
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
        assert_eq!(
            stats.knowledge_files(),
            2,
            "logs excluded from the corpus count"
        );
        assert_eq!(stats.knowledge_bytes(), 17);
        let names: Vec<&str> = stats.categories.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["findings", "other"], "runs is never a category");
        assert_eq!(stats.logs.files, 2);
        assert_eq!(stats.logs.bytes, 13);

        // Listing: newest first, epoch/mission/kind parsed off the name.
        let logs = mission_logs(&store, "p").unwrap();
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].name, "1786891368-verify.raw");
        assert_eq!(logs[0].agent.as_deref(), Some("verify"));
        assert_eq!(logs[0].started, 1_786_891_368);
        assert_eq!(logs[0].kind, "raw");
        assert_eq!(logs[0].bytes, 11);
        assert_eq!(logs[1].agent.as_deref(), Some("discover"));
        assert!(
            mission_logs(&store, "ghost").unwrap().is_empty(),
            "no runs dir is not an error"
        );
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn session_keyed_logs_resolve_through_their_mission() {
        let store = tmp_store("session-log-agent");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", crate::agents::AgentRole::Tester)
            .unwrap();
        let mission = Mission {
            agent: "runner".into(),
            pins: BTreeMap::new(),
            budget: None,
            created: 1,
            name: None,
            session: None,
            control: None,
            opencode_session: Some("ses_abc".into()),
            environment_session: None,
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        store.write_mission("p", "probe", &mission, "brief").unwrap();
        write(
            &store.project_corpus_dir("p").join("runs/ses_abc.json"),
            "{}",
        );
        write(
            &store.project_corpus_dir("p").join("runs/legacy.json"),
            "{}",
        );

        let logs = mission_logs(&store, "p").unwrap();
        let linked = logs.iter().find(|log| log.name == "ses_abc.json").unwrap();
        assert_eq!(linked.agent.as_deref(), Some("runner"));
        let legacy = logs.iter().find(|log| log.name == "legacy.json").unwrap();
        assert_eq!(legacy.agent, None);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn an_historical_orphan_mission_can_be_updated_for_teardown_and_deleted() {
        let store = tmp_store("orphan-mission-delete");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "gone", crate::agents::AgentRole::Tester)
            .unwrap();
        let mut mission = Mission {
            agent: "gone".into(),
            pins: BTreeMap::new(),
            budget: None,
            created: 1,
            name: None,
            session: Some("corpus-old-run".into()),
            control: None,
            opencode_session: Some("ses_old".into()),
            environment_session: None,
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        store.write_mission("p", "orphan", &mission, "brief").unwrap();

        // Reproduce the historical bug: the agent disappeared without its
        // mission. New delete_agent calls cannot create this state.
        fs::remove_dir_all(store.project_agent_dir("p", "gone")).unwrap();
        mission.session = None;
        store
            .update_mission("p", "orphan", &mission)
            .expect("teardown bookkeeping tolerates the old orphan");
        store
            .delete_mission("p", "orphan")
            .expect("the orphan can be removed");
        assert!(store.load_mission("p", "orphan").is_err());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn active_environment_identity_blocks_every_delete_cascade() {
        let store = tmp_store("active-environment-delete");
        store
            .create_project("p", "P", "fixture-regtest")
            .unwrap();
        store
            .create_agent_with_role("p", "runner", crate::agents::AgentRole::Tester)
            .unwrap();
        let id = crate::EnvironmentSessionId {
            project: "p".into(),
            mission: "probe".into(),
            generation: 1,
        };
        let key = id.storage_key();
        store
            .save_environment_session(&crate::EnvironmentSessionRecord {
                id,
                plugin_id: "fixture-regtest".into(),
                plugin_version: "1.0.0".into(),
                plugin_digest: "sha256:fixture".into(),
                state: crate::EnvironmentSessionState::Ready,
                source_shas: BTreeMap::new(),
                environment_lock: None,
                image_digest: None,
                created: 1,
                updated: 1,
                error: None,
            })
            .unwrap();
        let mission = Mission {
            agent: "runner".into(),
            pins: BTreeMap::new(),
            budget: None,
            created: 1,
            name: None,
            session: None,
            control: None,
            opencode_session: None,
            environment_session: Some(key.clone()),
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        store.write_mission("p", "probe", &mission, "brief").unwrap();

        for error in [
            store.delete_mission("p", "probe").unwrap_err(),
            store.delete_agent("p", "runner").unwrap_err(),
            store.delete_project("p").unwrap_err(),
        ] {
            assert!(error.to_string().contains("lifecycle teardown first"), "{error}");
        }
        assert!(store.load_mission("p", "probe").is_ok());
        assert!(store.project_agent_dir("p", "runner").is_dir());
        assert!(store.project_dir("p").is_dir());

        let mut environment = store
            .load_environment_session_key("fixture-regtest", &key)
            .unwrap();
        environment.state = crate::EnvironmentSessionState::Closed;
        store.save_environment_session(&environment).unwrap();
        store.delete_agent("p", "runner").unwrap();
        assert!(store.load_mission("p", "probe").is_err());
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
        write(
            &runs.join("1-operator.json"),
            &export(0.5, 100, "deepseek/deepseek-v4-flash"),
        );
        write(
            &runs.join("2-operator.json"),
            &export(0.25, 50, "deepseek/deepseek-v4-flash"),
        );
        write(
            &runs.join("3-operator.json"),
            &export(1.5, 200, "moonshotai/kimi-k3"),
        );
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
    fn corpus_cost_measures_inference_time_without_parallel_tool_time() {
        let store = tmp_store("cost-inference-time");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let export = serde_json::json!({
            "messages": [{
                "info": {
                    "role": "assistant",
                    "providerID": "ollama",
                    "modelID": "qwen/qwen3",
                    "time": {"created": 1_000, "completed": 6_000},
                    "tokens": {"input": 10, "output": 5}
                },
                "parts": [
                    {"type": "tool", "state": {"time": {"start": 3_000, "end": 4_000}}},
                    {"type": "tool", "state": {"time": {"start": 3_500, "end": 4_500}}},
                    {"type": "tool", "state": {"time": {"start": 5_000, "end": 5_250}}}
                ]
            }, {
                "info": {
                    "role": "assistant",
                    "providerID": "ollama",
                    "modelID": "qwen/qwen3",
                    "tokens": {"input": 10, "output": 5}
                }
            }]
        });
        let runs = store.project_corpus_dir("p").join("runs");
        write(&runs.join("timed.json"), &export.to_string());

        let report = corpus_cost(&store, "p").unwrap();
        // 5,000ms assistant span - 1,500ms overlapping tool union - 250ms tool.
        assert_eq!(report.inference_ms, 3_250);
        assert_eq!(report.timed_messages, 1);
        assert_eq!(report.rows[0].inference_ms, 3_250);
        assert_eq!(report.rows[0].timed_messages, 1);
        assert_eq!(report.rows[0].messages, 2);
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn corpus_cost_reexport_overwrites_not_doubles() {
        // A live conversation is re-exported every turn to the SAME
        // session-keyed file (`runs/<session-id>.json`), so its cumulative
        // usage must REPLACE the prior read, never stack on top of it.
        let store = tmp_store("cost-reexport");
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let export = |input: u64| {
            serde_json::json!({
                "info": {},
                "messages": [{"info": {
                    "role": "assistant",
                    "providerID": "ollama",
                    "modelID": "qwen/qwen3",
                    "cost": 0.0,
                    "tokens": {"input": input, "output": 0,
                               "reasoning": 0, "cache": {"read": 0, "write": 0}}
                }}]
            })
            .to_string()
        };
        let runs = store.project_corpus_dir("p").join("runs");
        let file = runs.join("ses_abc.json");
        // Turn 1.
        write(&file, &export(100));
        let r1 = corpus_cost(&store, "p").unwrap();
        assert_eq!(r1.tokens, 100);
        assert_eq!(r1.rows.len(), 1);
        // Turn 2: same session, cumulative totals, same filename → overwrite.
        write(&file, &export(250));
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
        assert!(store
            .project_agent_dir("p", "operator")
            .join("opencode.json")
            .is_file());
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn legacy_launch_timestamp_deserializes_without_inventing_an_origin() {
        let legacy: Mission = serde_yaml::from_str(
            "agent: keeper\nlaunch_requested: 1700000000\n",
        )
        .unwrap();
        assert_eq!(
            legacy.launch_requested,
            Some(MissionLaunchRequest {
                requested_at: 1_700_000_000,
                requested_by: None,
            })
        );
        assert_eq!(legacy.dispatch, None);

        let current = MissionLaunchRequest {
            requested_at: 1_700_000_001,
            requested_by: Some(MissionRunRef {
                project: "p".into(),
                mission: "curator-a".into(),
                run_id: "p1-p-m9-curator-a-g2".into(),
            }),
        };
        let yaml = serde_yaml::to_string(&current).unwrap();
        assert!(yaml.contains("requested_at: 1700000001"), "{yaml}");
        assert!(yaml.contains("mission: curator-a"), "{yaml}");
        assert_eq!(
            serde_yaml::from_str::<MissionLaunchRequest>(&yaml).unwrap(),
            current
        );
    }
}
