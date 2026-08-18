//! The app's thin state layer.
//!
//! House rule (app-flow-plan chunk 0): widgets never touch the filesystem
//! or the corpus-core store API directly — every corpus-core call goes
//! through `AppState`, and widgets only render state and request actions.
//! Business logic (validation, store plumbing) lives here or in corpus-core,
//! never in a view.

use std::collections::{BTreeMap, BTreeSet, hash_map::RandomState};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;

use corpus_core::{
    AgentConfig, CorpusStats, CostReport, Error, Mission, PluginStatus, Project, RunLine,
    RunSession, SourceRevs, Store,
};

use crate::nav::Screen;

/// How often the raw captures are re-stat'd. Cheap next to the tmux
/// listing (no subprocess), so it runs on the faster beat.
const ACTIVITY_POLL: std::time::Duration = std::time::Duration::from_millis(500);
/// How often the corpus is re-walked to keep the sidebar summary current.
/// Slower than the activity beat — the count moving a second or two after
/// a write lands is fine, and this touches every corpus file's metadata.
const CORPUS_POLL: std::time::Duration = std::time::Duration::from_secs(2);

/// One project's subtree in the sidebar tree: its agents and missions.
#[derive(Debug, Clone, Default)]
pub struct ProjectTree {
    pub agents: Vec<(String, AgentConfig)>,
    pub missions: Vec<(String, Mission)>,
}

/// App-wide state: the corpus-core store handle plus the data the
/// screens render. Owned by `App`, passed by reference to the views.
pub struct AppState {
    store: Store,
    /// The screen the sidebar selection points at (Projects / Agents /
    /// Missions). `LaunchView` is not a screen — it takes the main column
    /// while a run is live (chunk 5 merges it into the mission view).
    pub current_screen: Screen,
    /// Whether the right chat panel is open (the top-bar toggle drives it).
    pub chat_open: bool,
    /// All projects as `(slug, spec)`, sorted by slug (corpus-core order).
    pub projects: Vec<(String, Project)>,
    /// The project the sidebar lists scope to (`None` = fall back to the
    /// first project; `select_project` sets it).
    pub selected_project: Option<String>,
    /// The top-bar per-source pins (`<repo> -> <rev>`), stamped into
    /// missions at creation. Derived from the selected project's plugin
    /// (each repo defaulting to its pinned rev) and editable in the top bar.
    pub source_pins: BTreeMap<String, String>,
    /// The available per-source revisions for the selected project's plugin
    /// (corpus-core `plugin_sources`) — the top bar's dropdown options.
    pub source_revs: Vec<SourceRevs>,
    /// Which project `source_revs` / `source_pins` were derived for; a
    /// stale pair is never trusted.
    source_revs_project: Option<String>,
    /// Which project the env probe aggregation belongs to (the top bar's
    /// live dot — a stale probe is never trusted).
    env_project: Option<String>,
    /// Missions of `selected_project`, newest-CREATED first (the store
    /// returns slug order, which reads as random in the sidebar; the app
    /// layer owns the presentation order, like `projects`).
    pub missions: Vec<(String, Mission)>,
    /// Which project `missions` belongs to; a stale pair is never trusted.
    missions_project: Option<String>,
    /// The last computed corpus summary (files/bytes per category) and
    /// cost report for `corpus_stats_project` — refreshed on selection
    /// change + manually.
    corpus_stats: Option<CorpusStats>,
    corpus_cost: Option<CostReport>,
    /// The project's mission transcripts (`corpus/runs/`), newest first —
    /// refreshed on the same beat as `corpus_stats`.
    mission_logs: Vec<corpus_core::MissionLog>,
    corpus_stats_project: Option<String>,
    /// When the corpus was last auto-re-walked (the sidebar summary keeps
    /// itself current on a throttle — no manual refresh). Independent of
    /// selection-change refreshes, which are immediate.
    corpus_polled_at: Option<std::time::Instant>,
    /// Discovered plugins with live probe results, refreshed on demand
    /// (`refresh_plugins`) — never per-frame: probing spawns processes
    /// on the host.
    plugins: Vec<PluginStatus>,
    /// Agents of `agents_project`, sorted by slug (corpus-core order).
    pub agents: Vec<(String, AgentConfig)>,
    /// The sidebar tree: every project's agents + missions, keyed by
    /// project slug — rebuilt with `refresh`/`refresh_agents`/
    /// `refresh_missions` (the CRUD paths), never per frame.
    pub trees: BTreeMap<String, ProjectTree>,
    /// Which project `agents` belongs to; a stale pair is never trusted.
    pub agents_project: Option<String>,
    /// The agent the Agents screen edits (sidebar click sets it; the view
    /// falls back to the first agent when stale).
    pub selected_agent: Option<String>,
    /// The mission the Missions screen shows (sidebar click + the create
    /// flows set it; the view falls back to the first mission).
    pub selected_mission: Option<String>,
    /// A mission the operator just created that should auto-launch on the
    /// Missions screen (set by New-Mission flows); consumed by the view.
    pub pending_launch: Option<String>,
    /// The one active run (launch seam): a single session at a
    /// time by design — the run view is a tail, not a multiplexer.
    run: Option<RunSession>,
    /// Identity of the active (or last-finished) run.
    pub run_meta: Option<RunMeta>,
    /// The mission the active run belongs to (None = not a mission run) —
    /// the mission view never shows another mission's output.
    pub run_mission: Option<String>,
    /// Transcript lines drained so far (the run view renders these).
    pub run_lines: Vec<RunLine>,
    /// None = still running; Some = final state (set once, at exit).
    pub run_status: Option<RunStatus>,
    /// A run that ended ON ITS OWN, held for exactly one report to the
    /// operator. An operator stop is not queued here — the act already
    /// answered itself with a toast; this is for the exits nobody asked
    /// for, which used to leave the pane silently idle.
    run_exit: Option<RunExit>,
    /// Live corpus tmux sessions seen at the last `refresh_live_sessions`
    /// — the re-attach list a relaunched app offers (chunk 7).
    pub live_sessions: Vec<String>,
    /// When `live_sessions` was last polled (polled on a throttle, never
    /// per frame — the poll spawns `tmux list-sessions`).
    live_sessions_polled_at: Option<std::time::Instant>,
    /// Per tmux session, the moment its TUI last painted anything —
    /// derived from the run's raw capture mtime and aged forward between
    /// polls, so it stays honest without re-statting every frame. This is
    /// what separates a WORKING agent from one parked at its prompt.
    session_activity: BTreeMap<String, std::time::Instant>,
    /// When `session_activity` was last refreshed (a `stat` per live
    /// session — cheap, so polled faster than the tmux listing).
    session_activity_polled_at: Option<std::time::Instant>,
    /// When the app last scanned mission records for a curator's
    /// `launch_requested` flag (throttled — the scan reads every mission
    /// off disk, so never per frame).
    launch_requests_polled_at: Option<std::time::Instant>,
    /// Curator-requested launches the app has honored, queued for one
    /// report to the operator each. Drained by the app loop into toasts —
    /// an autonomous launch the operator did not initiate should still
    /// announce itself.
    launch_notices: Vec<LaunchNotice>,
}

/// A curator-requested launch the app carried out (or tried to), queued
/// for a single operator-facing toast.
#[derive(Debug, Clone)]
pub struct LaunchNotice {
    pub mission: String,
    pub result: Result<(), String>,
}

/// The env probe as the top bar consumes it: readiness + notes, plus the
/// version the target is ACTUALLY running (live probe). `running_version`
/// is `None` when the target is unreachable — the top bar then simply omits
/// the version. (The manifest's `expected_tag` stays on `PluginStatus`; the
/// mismatch it implies is already spelled out in `notes`.)
#[derive(Debug, Clone)]
pub struct EnvStatus {
    pub name: String,
    pub ready: bool,
    pub notes: String,
    pub running_version: Option<String>,
}

/// The activity signal (Idle / Waiting / Working) is owned by corpus-core
/// now, so the app's dots and the curator's `mission_status` tool read the
/// SAME rule and window. Re-exported here so `crate::state::MissionActivity`
/// callers (the sidebar dot, the repaint budget) are unchanged.
pub use corpus_core::MissionActivity;

/// The status dot's decision from the app's aged in-memory reading: turns a
/// `last_paint` Instant into idle-seconds and defers to the shared core
/// rule (`corpus_core::activity_from_idle`). The app keeps its own polled
/// cache (statting per frame would be far too much I/O) — only the rule and
/// the window are shared.
fn activity_for(live: bool, last_paint: Option<std::time::Instant>) -> MissionActivity {
    corpus_core::activity_from_idle(live, last_paint.map(|p| p.elapsed().as_secs()))
}

/// Who the active (or last-finished) run was.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// The embedded-PTY attach argv captured at launch (None = piped
    /// fallback): the pane must outlive the dropped session handle, so
    /// attach state lives on the META, not the backend.
    pub pty_attach: Option<Vec<String>>,
}

/// A run that ended on its own, queued for one report to the operator.
/// `mission` is the display label of the mission it belonged to (None for
/// a non-mission run), resolved at exit while the bookkeeping still says
/// who it was.
#[derive(Debug, Clone)]
pub struct RunExit {
    pub mission: Option<String>,
    pub code: i32,
}

/// The final state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Exited on its own with this code (piped headless, or a TUI run
    /// whose tmux session ended — the operator quit opencode).
    Exited(i32),
    /// Stopped by the operator (best-effort transcript export first).
    Stopped,
}

impl AppState {
    /// Resolve the store from the environment once; list projects.
    pub fn from_env() -> Self {
        let store = Store::from_env();
        let mut state = Self {
            store,
            current_screen: Screen::Projects,
            chat_open: false,
            projects: Vec::new(),
            selected_project: None,
            source_pins: BTreeMap::new(),
            source_revs: Vec::new(),
            source_revs_project: None,
            env_project: None,
            missions: Vec::new(),
            missions_project: None,
            trees: BTreeMap::new(),
            corpus_stats: None,
            mission_logs: Vec::new(),
            corpus_cost: None,
            corpus_stats_project: None,
            corpus_polled_at: None,
            plugins: Vec::new(),
            agents: Vec::new(),
            agents_project: None,
            selected_agent: None,
            selected_mission: None,
            pending_launch: None,
            run: None,
            run_meta: None,
            run_mission: None,
            run_lines: Vec::new(),
            run_status: None,
            run_exit: None,
            live_sessions: Vec::new(),
            live_sessions_polled_at: None,
            session_activity: BTreeMap::new(),
            session_activity_polled_at: None,
            launch_requests_polled_at: None,
            launch_notices: Vec::new(),
        };
        state.refresh();
        state
    }

    /// Re-list the projects from the store (and rebuild the sidebar tree).
    /// Newest-created first — the tree's default-open project is the most
    /// recent (the selection fallback takes `projects.first()`).
    pub fn refresh(&mut self) {
        self.projects = self.store.list_projects().unwrap_or_default();
        self.projects.sort_by(|a, b| b.1.created.cmp(&a.1.created));
        self.refresh_trees();
    }

    /// Rebuild the sidebar tree: every project's agents + missions. One
    /// dir scan per project — called from the refresh paths, never per
    /// frame.
    pub fn refresh_trees(&mut self) {
        self.trees = self
            .projects
            .iter()
            .map(|(slug, _)| {
                let tree = ProjectTree {
                    agents: self.store.list_agents(slug).unwrap_or_default(),
                    missions: sort_missions(self.store.list_missions(slug).unwrap_or_default()),
                };
                (slug.clone(), tree)
            })
            .collect();
    }

    /// Re-probe the discovered plugins (host-side aggregation; the app
    /// never spawns plugins itself).
    pub fn refresh_plugins(&mut self) {
        self.plugins = corpus_core::plugin_status();
    }

    /// The last plugin probe results (empty until `refresh_plugins`).
    pub fn plugins(&self) -> &[PluginStatus] {
        &self.plugins
    }

    /// Re-list a project's agents (and keep its tree subtree fresh).
    pub fn refresh_agents(&mut self, project: &str) {
        self.agents = self.store.list_agents(project).unwrap_or_default();
        self.agents_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.agents = self.agents.clone();
        }
    }

    /// Re-list a project's missions, newest-created first (and keep its
    /// tree subtree fresh).
    pub fn refresh_missions(&mut self, project: &str) {
        self.missions = sort_missions(self.store.list_missions(project).unwrap_or_default());
        self.missions_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.missions = self.missions.clone();
        }
    }

    /// Re-walk a project's corpus (files/bytes per category, the mission
    /// transcripts, and the token/cost aggregation over run exports) for
    /// the sidebar summary and the project view.
    pub fn refresh_corpus_stats(&mut self, project: &str) {
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.mission_logs = corpus_core::mission_logs(&self.store, project).unwrap_or_default();
        self.corpus_cost = corpus_core::corpus_cost(&self.store, project).ok();
        self.corpus_stats_project = Some(project.to_string());
        self.corpus_polled_at = Some(std::time::Instant::now());
    }

    /// The sidebar's selected project — held by slug, falling back to the
    /// first project when unset or stale. `None` when there are no projects.
    pub fn effective_project(&self) -> Option<String> {
        self.selected_project
            .as_ref()
            .filter(|slug| self.projects.iter().any(|(s, _)| s == *slug))
            .cloned()
            .or_else(|| self.projects.first().map(|(slug, _)| slug.clone()))
    }

    /// Select a project in the sidebar and (re)load its scoped caches —
    /// agents, missions, and the corpus summary all move to `slug`.
    pub fn select_project(&mut self, slug: &str) {
        self.selected_project = Some(slug.to_string());
        self.refresh_agents(slug);
        self.refresh_missions(slug);
        self.refresh_corpus_stats(slug);
        self.refresh_source_revs(slug);
        self.refresh_env(slug);
    }

    /// Load the source-rev dropdowns for the project's plugin (the plugin
    /// defines the revs AVAILABLE), seeding the selection from the
    /// PROJECT's stored pins (the project owns the pick) with any unset
    /// source at its default rev. When the plugin/sources can't be found
    /// the current pins are left untouched (the placeholder defaults
    /// hold) rather than cleared.
    pub fn refresh_source_revs(&mut self, project: &str) {
        let revs = corpus_core::plugin_sources(&self.store, project).unwrap_or_default();
        if !revs.is_empty() {
            let stored: BTreeMap<String, String> = self
                .projects
                .iter()
                .find(|(s, _)| s == project)
                .map(|(_, p)| p.pins.clone())
                .unwrap_or_default();
            self.source_pins = revs
                .iter()
                .map(|source| {
                    let rev = stored
                        .get(&source.name)
                        .filter(|rev| source.revs.contains(rev))
                        .cloned()
                        .unwrap_or_else(|| source.default_rev().to_string());
                    (source.name.clone(), rev)
                })
                .collect();
        }
        self.source_revs = revs;
        self.source_revs_project = Some(project.to_string());
    }

    /// The top-bar dropdown changed: update the in-memory selection and
    /// persist the pick onto the project (missions stamp it at creation).
    pub fn set_source_pin(&mut self, project: &str, repo: &str, rev: &str) -> Result<(), Error> {
        self.source_pins.insert(repo.to_string(), rev.to_string());
        let updated = self.store.set_project_pins(project, self.source_pins.clone())?;
        if let Some((_, spec)) = self.projects.iter_mut().find(|(s, _)| s == project) {
            *spec = updated;
        }
        Ok(())
    }

    /// The current env-status aggregation for a project's plugin: the
    /// probe's readiness and notes PLUS the version the target is actually
    /// running (from the live probe), so the top bar can show what is up
    /// and flag a source pin that disagrees.
    pub fn env_status(&self, project: &str) -> Option<EnvStatus> {
        let (_slug, spec) = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)?;
        self.plugins
            .iter()
            .find(|p| p.name == spec.plugin)
            .map(|p| EnvStatus {
                name: p.name.clone(),
                ready: p.ready,
                notes: p.notes.clone(),
                running_version: p.running_version.clone(),
            })
    }

    /// Re-probe the env for a project (spawns the plugin's probe on the
    /// host — only ever on project switch or an explicit click, never
    /// per-frame).
    pub fn refresh_env(&mut self, project: &str) {
        self.refresh_plugins();
        self.env_project = Some(project.to_string());
    }

    /// Make sure the selected project's caches (agents, missions, corpus
    /// summary) are loaded, and that `selected_project` is concrete (falls
    /// back to the first project). Called once a frame from `App::update` —
    /// stale checks are project-name equality, so this only hits disk on
    /// change.
    pub fn ensure_selection(&mut self) {
        let Some(first) = self.projects.first().map(|(slug, _)| slug.clone()) else {
            self.selected_project = None;
            self.agents.clear();
            self.missions.clear();
            self.corpus_stats = None;
            self.mission_logs.clear();
            self.agents_project = None;
            self.missions_project = None;
            self.corpus_stats_project = None;
            self.source_revs.clear();
            self.source_revs_project = None;
            self.env_project = None;
            return;
        };
        let stale = !self
            .projects
            .iter()
            .any(|(slug, _)| Some(slug.as_str()) == self.selected_project.as_deref());
        if stale {
            self.selected_project = Some(first.clone());
        }
        let Some(project) = self.selected_project.clone() else {
            return;
        };
        if self.agents_project.as_deref() != Some(project.as_str()) {
            self.refresh_agents(&project);
        }
        if self.missions_project.as_deref() != Some(project.as_str()) {
            self.refresh_missions(&project);
        }
        if self.corpus_stats_project.as_deref() != Some(project.as_str()) {
            self.refresh_corpus_stats(&project);
        }
        if self.source_revs_project.as_deref() != Some(project.as_str()) {
            self.refresh_source_revs(&project);
        }
        if self.env_project.as_deref() != Some(project.as_str()) {
            self.refresh_env(&project);
        }
    }

    /// The sidebar's corpus summary for the selected project (None = not
    /// computed yet, or no project).
    pub fn corpus_stats(&self) -> Option<&CorpusStats> {
        self.corpus_stats.as_ref()
    }

    /// The project view's mission logs for the selected project, newest
    /// first (empty = none written yet, or no project).
    pub fn mission_logs(&self) -> &[corpus_core::MissionLog] {
        &self.mission_logs
    }

    /// The project view's cost report for the selected project.
    pub fn corpus_cost(&self) -> Option<&CostReport> {
        self.corpus_cost.as_ref()
    }

    /// Create a project. The human gives the display name; the machine
    /// gives the id — an auto-generated UUIDv4, which is a valid
    /// kebab-case slug, so it slots straight into the store layout
    /// (`store/projects/<id>/`), CLI scopes, and `CORPUS_PROJECT`.
    pub fn create_project(&self, name: &str, plugin: &str) -> Result<(String, Project), Error> {
        // Human names mint human slugs ("Dep Scans" → "dep-scans"); only a
        // name with no alphanumerics falls back to the opaque id. (UUID
        // slugs made every chat/tool reference unreadable — 2026-08-14.)
        let slug = {
            let s = corpus_core::slugify(name);
            if s.is_empty() { new_uuid_id() } else { s }
        };
        self.store.create_project(&slug, name, plugin).map(|p| (slug, p))
    }

    /// Clone a project; the copied name falls back to the source's when
    /// none is given. The slug derives from the name (kebab), else an id.
    pub fn clone_project(
        &self,
        from: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<(String, Project), Error> {
        let slug = name
            .map(corpus_core::slugify)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("{from}-copy"));
        // A taken slug gets a numeric suffix rather than an opaque id.
        let slug = (2..)
            .map(|n| {
                if n == 2 { slug.clone() } else { format!("{slug}-{n}") }
            })
            .find(|s| !self.store.project_dir(s).exists())
            .unwrap_or_else(new_uuid_id);
        self.store
            .clone_project(from, &slug, name, with_corpus)
            .map(|p| (slug, p))
    }

    pub fn delete_project(&self, slug: &str) -> Result<(), Error> {
        self.store.delete_project(slug)
    }

    /// The app's remembered UI choices (`store/app.yaml`). Read on demand —
    /// it is a tiny file and the app touches it at launch and on a picker
    /// change, never per frame.
    pub fn prefs(&self) -> corpus_core::AppPrefs {
        self.store.load_prefs()
    }

    /// Remember the chat model the operator picked, so the next launch comes
    /// back on it. A write failure is deliberately swallowed: a read-only
    /// store must degrade to "this session only", not toast on every pick.
    pub fn remember_chat_model(&self, model: &str) {
        let mut prefs = self.store.load_prefs();
        if prefs.chat_model == model {
            return;
        }
        prefs.chat_model = model.to_string();
        let _ = self.store.save_prefs(&prefs);
    }

    /// Rename a project's display label (the slug — its identity in every
    /// path — is untouched).
    pub fn rename_project(&self, slug: &str, name: &str) -> Result<Project, Error> {
        self.store.rename_project(slug, name)
    }

    /// Change a project's environment plugin binding.
    pub fn rebind_project(&self, slug: &str, plugin: &str) -> Result<Project, Error> {
        self.store.rebind_project(slug, plugin)
    }

    /// Wipe a project's corpus (the Corpus panel's red Delete): categories
    /// are emptied and `corpus_generation` bumps; the project + agents
    /// survive. Returns the updated project.
    pub fn wipe_project_corpus(&self, slug: &str) -> Result<Project, Error> {
        self.store.wipe_project_corpus(slug)
    }

    // --- agents ---

    /// Save (validate + write) an agent's opencode.json.
    pub fn save_agent(
        &self,
        project: &str,
        slug: &str,
        doc: &serde_json::Value,
    ) -> Result<(), Error> {
        self.store.save_agent(project, slug, doc)
    }

    // --- granular agent edits (the Forms tab) -------------------------
    // Each is a read-modify-validate-write in corpus-core, so the form
    // sends one value instead of rewriting the whole document.

    /// Set one field of an agent entry (`None` = the primary).
    pub fn set_agent_field(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        field: &str,
        value: serde_json::Value,
    ) -> Result<(), Error> {
        self.store.set_agent_field(project, slug, entry, field, value)
    }

    /// Rename an agent's display label (the slug — its identity in every
    /// path — is untouched).
    pub fn set_agent_name(&self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        self.store.set_agent_name(project, slug, name)
    }

    /// Set the agent's (or a subagent's) role — the server-enforced ceiling.
    pub fn set_agent_role(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        role: corpus_core::AgentRole,
    ) -> Result<(), Error> {
        match entry {
            Some(sub) => self.store.set_subagent_role(project, slug, sub, role),
            None => self.store.set_agent_role(project, slug, role),
        }
    }

    /// Merge a permission patch into an entry.
    pub fn patch_agent_permission(
        &self,
        project: &str,
        slug: &str,
        entry: Option<&str>,
        patch: &serde_json::Value,
    ) -> Result<(), Error> {
        self.store.patch_agent_permission(project, slug, entry, patch)
    }

    pub fn add_subagent(
        &self,
        project: &str,
        slug: &str,
        name: &str,
        description: &str,
        prompt: &str,
        model: Option<&str>,
        role: Option<corpus_core::AgentRole>,
    ) -> Result<(), Error> {
        self.store
            .add_subagent(project, slug, name, description, prompt, model, role)
    }

    pub fn remove_subagent(&self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        self.store.remove_subagent(project, slug, name)
    }

    /// opencode's launchable model ids — the catalog an AGENT config must
    /// resolve against. Deliberately NOT `ollama_models()`, which is the
    /// chat's own (locally-pulled) list and would offer ids a mission
    /// cannot launch with. TTL-cached in corpus-core; `refresh` re-pulls.
    pub fn opencode_models(&self, refresh: bool) -> Option<corpus_core::ModelList> {
        corpus_core::model_list(refresh).ok()
    }

    /// Clone an agent.
    pub fn clone_agent(&self, project: &str, from: &str) -> Result<(), Error> {
        let id = new_uuid_id();
        self.store.clone_agent(project, from, &id)
    }

    /// Delete an agent.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<(), Error> {
        self.store.delete_agent(project, slug)
    }

    /// Create a new (auto-id'd) agent from a ROLE — the sidebar's
    /// "+ agent" flow. Roles replaced the seed set: the role already
    /// decides the capability ceiling the renderer writes, so a seed
    /// document was only ever contributing a starting prompt, which now
    /// ships compiled into corpus-core.
    pub fn create_agent_with_role(&self, project: &str, role: corpus_core::AgentRole) -> Result<String, Error> {
        let id = new_uuid_id();
        self.store.create_agent_with_role(project, &id, role)?;
        // Stamp the human placeholder name so the Forms tab and the sidebar
        // show an editable label (and opencode a friendly handle), not the
        // opaque id. Best-effort: a naming failure must not undo a created
        // agent.
        let _ = self.store.set_agent_name(project, &id, corpus_core::DEFAULT_AGENT_NAME);
        Ok(id)
    }

    /// Create a mission record: auto-id slug, the agent ref, the current
    /// top-bar pins stamped in. Returns the mission slug.
    pub fn create_mission(&self, project: &str, agent: &str, brief: &str) -> Result<String, Error> {
        let id = new_uuid_id();
        let mission = Mission {
            agent: agent.to_string(),
            pins: self.source_pins.clone(),
            budget: None,
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            name: None,
            session: None,
            opencode_session: None,
            launch_requested: None,
        };
        self.store.write_mission(project, &id, &mission, brief)?;
        Ok(id)
    }

    /// Delete a mission record (the transcripts stay in the corpus runs/).
    pub fn delete_mission(&self, project: &str, slug: &str) -> Result<(), Error> {
        self.store.delete_mission(project, slug)
    }

    // --- run launch ---

    /// Whether a run is currently live.
    pub fn run_active(&self) -> bool {
        self.run.is_some()
    }

    /// Materialize the agent and spawn the mission on the project
    /// scope. Runs OVERLAP: the caller backgrounds the live one first
    /// (`background_active_run`), and the handle this adopts replaces it.
    /// `source_pins_json` is the resolved `repo -> sha` map exported to
    /// the run (None = the plugin's default pins).
    pub fn launch(
        &mut self,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
    ) -> Result<(), Error> {
        // Fail loudly on an unknown agent, then materialize the WHOLE
        // project: the agent list opencode shows is project-scoped.
        self.store.load_agent(project, agent)?;
        self.store.render_project_agents(project)?;
        let session = RunSession::spawn(project, agent, model, mission, source_pins_json)?;
        self.adopt_run(session);
        Ok(())
    }

    /// Take ownership of a freshly spawned run and reset the per-run
    /// bookkeeping (attach argv, drained lines, terminal status). Shared
    /// by `launch` and `resume_mission` so a resumed run is wired exactly
    /// like a fresh one.
    fn adopt_run(&mut self, session: RunSession) {
        let pty_attach = session.pty_attach_command();
        self.run = Some(session);
        self.run_meta = Some(RunMeta { pty_attach });
        self.run_mission = None;
        self.run_lines.clear();
        self.run_status = None;
    }

    /// Drain any new transcript lines; mark the run finished the moment
    /// it exits. Called every frame by the app loop (so an exit is noticed
    /// on any screen) and by the mission view.
    pub fn poll_run(&mut self) {
        let Some(mut session) = self.run.take() else {
            return;
        };
        while let Some(line) = session.poll_line() {
            self.run_lines.push(line);
        }
        if let Some(status) = session.try_exit() {
            // An operator stop already recorded its terminal state;
            // everything else surfaces as its exit code.
            if self.run_status != Some(RunStatus::Stopped) {
                let code = status.code().unwrap_or(1);
                self.run_status = Some(RunStatus::Exited(code));
                // Queue the report NOW: `run_mission` still names the
                // mission, and once the handle is gone the only evidence
                // this run existed is the pane going quiet.
                self.run_exit = Some(RunExit {
                    mission: self.run_mission.clone().map(|slug| self.mission_label(&slug)),
                    code,
                });
            }
            return;
        }
        self.run = Some(session);
    }

    /// Take the pending end-of-run report, if one is waiting. Drained by
    /// the app loop once per exit.
    pub fn take_run_exit(&mut self) -> Option<RunExit> {
        self.run_exit.take()
    }

    /// A mission's operator-facing label (from the cache): its name, else
    /// its human slug, else `new` — the same rule the nav uses. Never a raw
    /// uuid.
    pub fn mission_label(&self, slug: &str) -> String {
        let name = self
            .missions
            .iter()
            .find(|(s, _)| s == slug)
            .and_then(|(_, m)| m.name.clone());
        mission_label(name.as_deref(), slug)
    }

    /// Operator-initiated stop: best-effort transcript-of-record export,
    /// then kill the run. Returns the durable transcript path (the
    /// exported JSON when it lands, else the raw/.log fallback) — the
    /// caller is what reports it, so nothing is stored here.
    pub fn stop_run(&mut self) -> Option<PathBuf> {
        let mut session = self.run.take()?;
        let path = session.stop();
        self.run_status = Some(RunStatus::Stopped);
        Some(path)
    }

    /// The embedded-PTY attach argv of the LIVE run (chunk 7): Some for
    /// a tmux TUI run, None for the piped fallback (or no run) — the
    /// Launch screen then shows the transcript tail instead of the pane.
    pub fn live_pty_attach(&self) -> Option<Vec<String>> {
        if !self.run_active() {
            return None;
        }
        self.run_meta.as_ref()?.pty_attach.clone()
    }

    /// The tmux session name embedded in a pty attach argv, if any.
    pub fn pty_attach_session(argv: &[String]) -> Option<String> {
        argv.windows(2)
            .find(|w| w[0] == "-t")
            .map(|w| w[1].clone())
    }

    /// Re-list the live corpus tmux sessions (the re-attach list shown
    /// when the app was relaunched over a surviving run).
    pub fn refresh_live_sessions(&mut self) {
        self.live_sessions = corpus_core::live_tui_sessions();
        self.live_sessions_polled_at = Some(std::time::Instant::now());
    }

    /// Poll live sessions on a throttle (2 s): the poll spawns a `tmux
    /// list-sessions` subprocess, so never per frame — and only when a
    /// live session can even matter (a run is up, or some mission record
    /// still holds a session). This keeps the sidebar's activity dots
    /// fresh without polling an idle app forever.
    pub fn poll_live_sessions(&mut self) {
        let relevant = self.run_active()
            || self
                .trees
                .values()
                .any(|t| t.missions.iter().any(|(_, m)| m.session.is_some()));
        if !relevant {
            return;
        }
        let due = self
            .live_sessions_polled_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(2));
        if due {
            self.refresh_live_sessions();
            // On the tmux listing's beat, not the faster one: the sweep
            // shells out per pending mission, and it needs the listing it
            // just refreshed to know which sessions are live. A no-op once
            // every live mission has its conversation recorded.
            if let Some(project) = self.effective_project() {
                self.sweep_conversations(&project);
            }
        }
        // Activity is a `stat` per live session — no subprocess, so it
        // polls faster: the dot should catch a turn starting, not lag it
        // by the tmux listing's throttle.
        let activity_due = self
            .session_activity_polled_at
            .is_none_or(|t| t.elapsed() > ACTIVITY_POLL);
        if activity_due {
            self.refresh_session_activity();
            // Same beat: catch the live run's opencode session id as soon
            // as the TUI has created it (self-throttled, and a no-op once
            // the mission record has one).
            self.capture_opencode_session();
        }
    }

    /// Honor any launch the CURATOR requested (its `mission_launch` tool
    /// set `launch_requested` on a mission record from the MCP process —
    /// run spawning is the app's alone). Throttled (2 s): the scan reads
    /// every project's mission records off disk, since the flag was written
    /// by another process and the cached tree does not have it.
    ///
    /// The request is cleared BEFORE the spawn, so a launch that fails
    /// reports once instead of retrying every beat. A mission whose
    /// requested session is already live just clears — the curator asked
    /// for a run and there is one. Scans EVERY project, not just the
    /// selected one: the curator is scoped to its own project, which the
    /// operator need not be viewing when the launch fires.
    pub fn poll_launch_requests(&mut self) {
        let due = self
            .launch_requests_polled_at
            .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(2));
        if !due {
            return;
        }
        self.launch_requests_polled_at = Some(std::time::Instant::now());

        // Gather flagged missions off disk first (the authoritative record —
        // the flag came from the MCP process).
        let projects: Vec<String> = self.projects.iter().map(|(s, _)| s.clone()).collect();
        let mut pending: Vec<(String, String, Option<String>)> = Vec::new();
        for project in &projects {
            let Ok(missions) = self.store.list_missions(project) else {
                continue;
            };
            for (slug, m) in missions {
                if m.launch_requested.is_some() {
                    pending.push((project.clone(), slug, m.session.clone()));
                }
            }
        }
        if pending.is_empty() {
            return;
        }
        // A fresh listing so "already live" is a real answer, not a stale
        // one that would spawn a duplicate.
        self.refresh_live_sessions();
        for (project, slug, session) in pending {
            // Clear FIRST: a spawn failure must not loop the request.
            if let Err(error) = self.clear_launch_request(&project, &slug) {
                self.launch_notices.push(LaunchNotice {
                    mission: slug.clone(),
                    result: Err(error.to_string()),
                });
                continue;
            }
            let already_live = session
                .as_deref()
                .is_some_and(|s| self.live_sessions.iter().any(|l| l == s));
            if already_live {
                continue;
            }
            let label = self.mission_display_label(&project, &slug);
            let result = self
                .launch_mission_detached(&project, &slug)
                .map_err(|e| e.to_string());
            self.launch_notices.push(LaunchNotice { mission: label, result });
        }
    }

    /// Drain the curator-launch reports queued since the last call — the
    /// app loop turns each into a toast.
    pub fn take_launch_notices(&mut self) -> Vec<LaunchNotice> {
        std::mem::take(&mut self.launch_notices)
    }

    /// Clear a mission's `launch_requested` flag, preserving its brief.
    fn clear_launch_request(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.launch_requested = None;
        self.store.update_mission(project, slug, &mission)
    }

    /// A mission's operator-facing label from its DISK record (name, else
    /// its human slug, else `new`) — the cache covers only the selected
    /// project, and a curator launch can name any.
    fn mission_display_label(&self, project: &str, slug: &str) -> String {
        let name = self.store.load_mission(project, slug).ok().and_then(|m| m.name);
        mission_label(name.as_deref(), slug)
    }

    /// Keep the selected project's scoped caches — its agent list, its
    /// mission list, and the corpus summary — current on their own, so a
    /// change the CURATOR makes from the MCP process (deletes a mission,
    /// spawns an agent, writes a finding) just appears. Without this the
    /// lists refreshed only on the app's OWN CRUD or a reselect, so an
    /// external mutation stayed invisible until the operator clicked away
    /// and back. All three are cheap `read_dir` + `stat` passes (bounded
    /// by file COUNT, not size), so a throttle this tight is comfortable.
    /// Selection is held by slug and both views fall back when its target
    /// vanishes, so a background re-list never yanks the operator's cursor.
    /// Selection-change refreshes still happen immediately elsewhere; this
    /// only fills the gaps between them.
    pub fn poll_project_scope(&mut self) {
        let Some(project) = self.effective_project() else {
            return;
        };
        let due = self
            .corpus_polled_at
            .is_none_or(|t| t.elapsed() > CORPUS_POLL);
        if due {
            self.refresh_agents(&project);
            self.refresh_missions(&project);
            // Stamps `corpus_polled_at`, closing the throttle for all three.
            self.refresh_corpus_stats(&project);
        }
    }

    /// Re-stat the raw capture of every mission session we know of, and
    /// record WHEN it last grew as an `Instant`. Storing the instant (not
    /// the age) means the reading keeps aging correctly between polls, so
    /// a 500 ms poll still gives a dot that goes still the moment output
    /// stops.
    fn refresh_session_activity(&mut self) {
        self.session_activity_polled_at = Some(std::time::Instant::now());
        let sessions: Vec<(String, String)> = self
            .trees
            .iter()
            .flat_map(|(project, tree)| {
                tree.missions.iter().filter_map(move |(_, mission)| {
                    mission.session.clone().map(|s| (project.clone(), s))
                })
            })
            .collect();
        self.session_activity.clear();
        for (project, session) in sessions {
            let Some(log) = corpus_core::session_raw_log(&self.store, &project, &session) else {
                continue;
            };
            let Some(idle) = corpus_core::run_idle_secs(&log) else {
                continue;
            };
            let last_paint = std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(idle))
                .unwrap_or_else(std::time::Instant::now);
            self.session_activity.insert(session, last_paint);
        }
    }

    /// What the mission's status dot should say: `Idle` (nothing up),
    /// `Waiting` (session live, agent quiet), or `Working` (producing
    /// right now).
    ///
    /// A run is UP when the app-owned run is live and belongs to this
    /// mission (run_status set = exited/stopped = down), or when the
    /// mission's recorded tmux session is alive on the server — which
    /// covers a relaunched app and sessions the app never owned.
    ///
    /// The busy signal is the run's raw capture: everything the TUI
    /// paints flows through `tmux pipe-pane` into it, and a TUI waiting
    /// at its prompt paints nothing — so a capture that grew within
    /// `WORKING_WINDOW` means the agent is mid-turn. A piped headless run
    /// has no TUI to watch and is one-shot by nature: while it is up, it
    /// IS working.
    pub fn mission_activity(&self, project: &str, slug: &str) -> MissionActivity {
        let owned = self.run_active()
            && self.run_status.is_none()
            && self.run_mission.as_deref() == Some(slug);
        let session = self
            .trees
            .get(project)
            .and_then(|tree| tree.missions.iter().find(|(s, _)| s == slug))
            .and_then(|(_, mission)| mission.session.clone());
        let Some(session) = session else {
            // No tmux session: the only thing that can be up is an
            // app-owned piped run, which is busy for its whole life.
            return if owned { MissionActivity::Working } else { MissionActivity::Idle };
        };
        let live = self.live_sessions.iter().any(|l| l == &session)
            || self.live_run_session().as_deref() == Some(session.as_str())
            || owned;
        activity_for(live, self.session_activity.get(&session).copied())
    }

    /// The attach argv for a discovered live session (re-attach after an
    /// app relaunch; None when tmux is gone).
    pub fn session_attach_command(session: &str) -> Option<Vec<String>> {
        corpus_core::tui_attach_command(session)
    }

    // --- mission bookkeeping (run attach + sidebar ops) ---

    /// The tmux session of the app-owned live run, if any.
    pub(crate) fn live_run_session(&self) -> Option<String> {
        self.live_pty_attach()
            .and_then(|argv| AppState::pty_attach_session(&argv))
    }

    /// Launch a mission's run: a full opencode TUI in a detached tmux
    /// session, kicked off with the mission's BRIEF as the opencode
    /// `--prompt` (an empty brief lands at a bare prompt, the old
    /// behaviour — the sidebar `+` creates briefless missions on purpose).
    /// The spawned tmux session is persisted on the mission record so a
    /// relaunched app re-attaches by selection. A live run is BACKGROUNDED,
    /// not replaced: it keeps running under its own tmux session (and its
    /// own mission record), while this mission lands on a fresh opencode
    /// session that the operator watches and steers in the embedded pane.
    ///
    /// This ADOPTS the run as the app-owned one, so the mission view
    /// attaches its pane immediately — the path for an operator who clicked
    /// Launch and wants to watch. A curator's autonomous launch takes
    /// `launch_mission_detached` instead, which does not hijack the pane.
    pub fn launch_mission(&mut self, project: &str, agent: &str, slug: &str) -> Result<(), Error> {
        let (_record, pins_json) = self.prepare_launch(project, slug)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        self.background_active_run();
        let model = self.agent_default_model(project, agent);
        self.launch(project, agent, model.as_deref(), &prompt, pins_json.as_deref())?;
        self.run_mission = Some(slug.to_string());
        if let Some(session) = self.live_run_session() {
            self.set_tmux_session(project, slug, Some(session))?;
        }
        // A fresh launch is a NEW conversation: drop any id from a
        // previous run so discovery records the right one.
        self.set_opencode_session(project, slug, None)?;
        self.refresh_missions(project);
        Ok(())
    }

    /// The kickoff prompt for a mission launch: its brief, trimmed. Empty
    /// (a briefless mission, e.g. the sidebar `+`) means a bare TUI at an
    /// empty prompt. A brief read failure is not fatal — a launch with no
    /// prompt still runs — so it degrades to empty rather than refusing.
    fn mission_kickoff_prompt(&self, project: &str, slug: &str) -> String {
        self.store
            .mission_brief(project, slug)
            .map(|b| b.trim().to_string())
            .unwrap_or_default()
    }

    /// Spawn a mission's run in the BACKGROUND — a full opencode TUI in a
    /// detached tmux session, kicked off with the brief — WITHOUT adopting
    /// it as the app-owned run. This is the curator's autonomous launch:
    /// the session is real and watchable (the operator selects the mission
    /// to attach the pane and interact), but it does not seize the pane
    /// from whatever the operator is already watching.
    ///
    /// The tmux session name is recorded on the mission record before the
    /// handle is dropped, so the app's own discovery (`refresh_live_sessions`
    /// + `sweep_conversations`) adopts it exactly like any run that
    /// outlived the app — attach, activity dot, and eventual export all
    /// follow from the recorded session. A no-tmux fallback cannot be
    /// backgrounded (the piped child lives on the handle), so there it
    /// adopts the run rather than orphaning it.
    pub fn launch_mission_detached(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        let (record, pins_json) = self.prepare_launch(project, slug)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        let model = self.agent_default_model(project, &record.agent);
        // Same materialization as an adopted launch: the run's agent set is
        // this project's, rendered fresh.
        self.store.load_agent(project, &record.agent)?;
        self.store.render_project_agents(project)?;
        let session = corpus_core::RunSession::spawn(
            project,
            &record.agent,
            model.as_deref(),
            &prompt,
            pins_json.as_deref(),
        )?;
        let tmux = session
            .pty_attach_command()
            .and_then(|argv| AppState::pty_attach_session(&argv));
        match tmux {
            Some(name) => {
                // A detached TUI: record the session and let go. Discovery
                // takes it from here.
                self.set_tmux_session(project, slug, Some(name))?;
                self.set_opencode_session(project, slug, None)?;
                drop(session);
            }
            None => {
                // Piped fallback: the child lives on the handle, so it
                // must be adopted or it leaks. This does take the pane —
                // there is no detached session to hand off.
                self.background_active_run();
                self.adopt_run(session);
                self.run_mission = Some(slug.to_string());
                self.set_opencode_session(project, slug, None)?;
            }
        }
        self.refresh_live_sessions();
        self.refresh_missions(project);
        Ok(())
    }

    /// Re-open a mission's recorded opencode conversation in a fresh TUI
    /// (`opencode --session <id>`), so an old mission whose tmux session
    /// died is steerable again with its history intact. Same rule as
    /// `launch_mission`: a live run is backgrounded, not stopped.
    pub fn resume_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        let (record, pins_json) = self.prepare_launch(project, slug)?;
        let id = record.opencode_session.clone().ok_or_else(|| {
            Error::Store("no opencode session recorded for this mission — nothing to resume".into())
        })?;
        self.background_active_run();
        // Same materialization as a launch: the resumed conversation runs
        // against this project's agent set.
        self.store.load_agent(project, &record.agent)?;
        self.store.render_project_agents(project)?;
        let model = self.agent_default_model(project, &record.agent);
        let run = corpus_core::RunSession::resume(
            project,
            &record.agent,
            model.as_deref(),
            &id,
            pins_json.as_deref(),
        )?;
        self.adopt_run(run);
        self.run_mission = Some(slug.to_string());
        if let Some(session) = self.live_run_session() {
            self.set_tmux_session(project, slug, Some(session))?;
        }
        self.refresh_missions(project);
        Ok(())
    }

    /// The shared launch preamble: load the mission, resolve its rev
    /// pins to shas + fetch trees (loud failure here must never tear
    /// down a working run — that happens only after this returns).
    fn prepare_launch(
        &self,
        project: &str,
        slug: &str,
    ) -> Result<(Mission, Option<String>), Error> {
        let mission_record = self.store.load_mission(project, slug)?;
        let prepared = corpus_core::prepare_source_pins(&self.store, project, &mission_record.pins)?;
        let pins_json = if prepared.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&prepared)?)
        };
        Ok((mission_record, pins_json))
    }

    /// Start a new run WITHOUT stopping the live one.
    ///
    /// This used to tear the previous run down. Its guard tested handle
    /// OWNERSHIP rather than reality, so one action behaved two ways: a run
    /// this process launched was killed, while a run it had merely
    /// re-attached to after a restart survived untouched — which is why
    /// restarting the app appeared to "restore" a clobbered session. A run
    /// now ends only when the operator stops it.
    ///
    /// The outgoing run keeps its tmux binding, but `adopt_run` is about to
    /// drop its `RunSession`, and nothing polls for a conversation id once
    /// the handle is gone. So the id is claimed here, while the handle still
    /// exists. `sweep_conversations` is the backstop for a run displaced
    /// before opencode had created its session at all.
    ///
    /// Only a tmux run can be left behind: it is detached, so dropping the
    /// handle costs nothing but the record already names the session. The
    /// piped fallback lives ON the handle — dropping it would orphan the
    /// child and lose the transcript — so that one is stopped (exported)
    /// rather than backgrounded.
    fn background_active_run(&mut self) {
        if !self.run_active() {
            return;
        }
        self.capture_opencode_session();
        if self.live_pty_attach().is_none() {
            self.stop_run();
        }
    }

    /// Ids already bound to other missions.
    ///
    /// Concurrent runs share one run dir, so `opencode session list` shows
    /// every live mission's conversation. Excluding what is already claimed
    /// is what stops a slow-booting run from adopting a neighbour's.
    fn claimed_conversations(&self, except: &str) -> BTreeSet<String> {
        self.missions
            .iter()
            .filter(|(slug, _)| slug.as_str() != except)
            .filter_map(|(_, m)| m.opencode_session.clone())
            .collect()
    }

    /// Fill in the conversation id of any mission that is LIVE in tmux but
    /// has none recorded — the runs no handle covers: displaced by a later
    /// launch, or re-attached after a restart. Without this they stay
    /// orphans, attachable but neither exportable nor resumable.
    pub fn sweep_conversations(&mut self, project: &str) {
        let pending: Vec<(String, String)> = self
            .missions
            .iter()
            .filter(|(_, m)| m.opencode_session.is_none())
            .filter_map(|(slug, m)| Some((slug.clone(), m.session.clone()?)))
            .filter(|(_, session)| self.live_sessions.iter().any(|l| l == session))
            .collect();
        let mut changed = false;
        for (slug, session) in pending {
            let claimed = self.claimed_conversations(&slug);
            let Some(id) =
                corpus_core::session_conversation(&self.store, project, &session, &claimed)
            else {
                continue;
            };
            if self.set_opencode_session(project, &slug, Some(id)).is_ok() {
                changed = true;
            }
        }
        if changed {
            self.refresh_missions(project);
        }
    }

    /// Point a mission at a tmux session (or clear a dead one). The
    /// opencode session is left alone: the tmux session is where the run
    /// is ATTACHED, while the opencode session is what the mission IS —
    /// it outlives every attach and is what `resume_mission` re-opens.
    fn set_tmux_session(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.session = session;
        self.store.update_mission(project, slug, &mission)
    }

    /// Record (or clear) the opencode conversation a mission owns. A
    /// fresh launch clears it — opencode starts a new conversation, so
    /// the old id would resume the WRONG one — and discovery fills it
    /// back in once the TUI has created its session.
    fn set_opencode_session(
        &mut self,
        project: &str,
        slug: &str,
        id: Option<String>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.opencode_session = id;
        self.store.update_mission(project, slug, &mission)
    }

    /// Catch the opencode session id of the live run and persist it on
    /// its mission. Called on the poll beat: the id doesn't exist at
    /// launch (the TUI has to boot first), and without it a mission can
    /// be neither exported after an app restart nor resumed. Cheap once
    /// it lands — the lookup stops the moment the record has an id.
    fn capture_opencode_session(&mut self) {
        let (Some(project), Some(slug)) = (self.effective_project(), self.run_mission.clone())
        else {
            return;
        };
        if !self.run_active() || self.run_status.is_some() {
            return;
        }
        let known = self
            .store
            .load_mission(&project, &slug)
            .ok()
            .and_then(|m| m.opencode_session);
        if known.is_some() {
            return;
        }
        let claimed = self.claimed_conversations(&slug);
        let Some(id) = self
            .run
            .as_mut()
            .and_then(|run| run.opencode_session_id(&claimed))
        else {
            return;
        };
        if self.set_opencode_session(&project, &slug, Some(id)).is_ok() {
            self.refresh_missions(&project);
        }
    }

    /// Rename a mission (its display label) while keeping the slug.
    pub fn rename_mission(&mut self, project: &str, slug: &str, name: &str) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let name = name.trim();
        mission.name = if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        };
        self.store.update_mission(project, slug, &mission)
    }

    /// Stop a mission's run: best-effort transcript-of-record export,
    /// then kill — whether the app owns the run or it survived an app
    /// relaunch. Clears the dead tmux session and returns the durable
    /// transcript path when known. The opencode session id STAYS on the
    /// record: stopping ends the attach, not the conversation, and that
    /// id is what `resume_mission` re-opens.
    pub fn stop_mission(&mut self, project: &str, slug: &str) -> Result<String, Error> {
        let mission = self.store.load_mission(project, slug)?;
        let session = mission.session.as_deref().ok_or_else(|| {
            Error::Store("no live session on this mission — nothing to stop".into())
        })?;
        let path = if self.live_run_session().as_deref() == Some(session) {
            self.stop_run().map(|p| p.display().to_string())
        } else {
            // A run that outlived the app: export via the recorded
            // opencode session when we have one (best-effort — the raw
            // capture is the durable fallback), then kill the tmux session.
            let exported = mission
                .opencode_session
                .as_deref()
                .and_then(|id| corpus_core::export_session(project, &mission.agent, id).ok())
                .map(|p| p.display().to_string());
            corpus_core::kill_tmux_session(session);
            exported
        };
        // A stop the operator asked for reports itself: the path goes back
        // to the caller, which toasts it. Nothing to stash.
        self.set_tmux_session(project, slug, None)?;
        self.refresh_live_sessions();
        Ok(path.unwrap_or_default())
    }

    /// The model the launch dialog pre-fills: the registry's curated
    /// tool-use default (an explicit arg — the engine never falls back
    /// to opencode's ambient model). None when the registry is empty.
    pub fn suggested_model(&self) -> Option<String> {
        let path = std::env::var("CORPUS_MODELS")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("benchmarks/models.yaml"));
        corpus_core::ModelRegistry::load(&path)
            .ok()?
            .launch_default()
    }

    /// The launch pre-fill for an agent: primary entry model → registry
    /// tool-use default.
    pub fn agent_default_model(&self, project: &str, agent: &str) -> Option<String> {
        corpus_core::launch::agent_default_model(&self.store, project, agent)
            .or_else(|| self.suggested_model())
    }
}

/// The label to show for an agent: its display name, never an opaque
/// UUID slug.
///
/// A real name always wins. "unnamed agent" is only for an agent with no
/// meaningful handle: an empty name, or a name equal to the app's own
/// UUID slug — which the `+` flow writes into the sidecar before it stamps
/// the placeholder, so a raw id never surfaces if that stamp is lost.
///
/// The `name == slug` case is qualified by UUID-shape ON PURPOSE. The
/// curator names an agent by a human slug (`reporter`), and `create_agent`
/// records that slug as the name — so `name == slug` there is a REAL name,
/// not a missing one. Collapsing it unconditionally is what made every
/// curator-built agent read as "unnamed agent" while its own form showed
/// the name. The slug stays in hover tooltips and the JSON tab for identity.
pub fn agent_label(name: &str, slug: &str) -> String {
    if name.is_empty() || (name == slug && is_uuid_like(slug)) {
        "unnamed agent".to_string()
    } else {
        name.to_string()
    }
}

/// The label to show for a mission in the nav: its display name, else its
/// slug when that is a human handle (the curator names a mission
/// `cdk-proto-attack` — show it), else `new` for the app's own UUID-slug
/// missions created before they are named. Mirrors [`agent_label`].
pub fn mission_label(name: Option<&str>, slug: &str) -> String {
    match name.map(str::trim).filter(|n| !n.is_empty()) {
        Some(name) => name.to_string(),
        None if !is_uuid_like(slug) => slug.to_string(),
        None => "new".to_string(),
    }
}

/// Whether a slug is one of the app's generated UUIDs (see `new_uuid_id`):
/// 36 chars, `8-4-4-4-12` hex with dashes at the canonical offsets. A human
/// slug (`reporter`, `recon-mapper`) never matches, so it is never mistaken
/// for a placeholder id.
fn is_uuid_like(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    bytes.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    })
}

/// Mission list order, newest-CREATED first (slug tiebreak). The store
/// returns slug order — stable across saves, but the slugs are random
/// UUIDs so the sidebar looked shuffled; created order matches the
/// project list (state.rs `refresh`, newest first).
fn sort_missions(mut missions: Vec<(String, Mission)>) -> Vec<(String, Mission)> {
    missions.sort_by(|a, b| b.1.created.cmp(&a.1.created).then_with(|| a.0.cmp(&b.0)));
    missions
}

/// A fresh RFC-4122-v4-formatted id, generated without new dependencies:
/// `RandomState` seeds each process with 128 bits of system entropy, and
/// SipHash-128 over two fixed salts extracts two independent 64-bit
/// values. Collision odds are the UUIDv4's own.
fn new_uuid_id() -> String {
    let mut bytes = uuid_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn uuid_bytes() -> [u8; 16] {
    let seeds = RandomState::new();
    let mut low = seeds.build_hasher();
    low.write(&[0]);
    let mut high = seeds.build_hasher();
    high.write(&[1]);
    let mut out = [0u8; 16];
    out[..8].copy_from_slice(&low.finish().to_le_bytes());
    out[8..].copy_from_slice(&high.finish().to_le_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_are_formatted_uuids_and_valid_slugs() {
        for _ in 0..100 {
            let id = new_uuid_id();
            assert_eq!(id.len(), 36, "{id}");
            assert_eq!(id.bytes().filter(|b| *b == b'-').count(), 4, "{id}");
            // A generated id must drop straight into the store layout.
            assert!(corpus_core::validate_slug(&id).is_ok(), "{id}");
        }
    }

    #[test]
    fn mission_label_prefers_name_then_human_slug_then_new() {
        // An explicit name always wins.
        assert_eq!(mission_label(Some("recon sweep"), "cdk-recon"), "recon sweep");
        // No name, human slug: show the slug (the curator's mission id).
        assert_eq!(mission_label(None, "cdk-proto-attack"), "cdk-proto-attack");
        assert_eq!(mission_label(Some("  "), "cdk-proto-attack"), "cdk-proto-attack");
        // No name, UUID slug (the app's `+` before naming): placeholder.
        let uuid = new_uuid_id();
        assert_eq!(mission_label(None, &uuid), "new");
    }

    #[test]
    fn agent_label_shows_a_human_slug_but_hides_a_uuid() {
        // A curator names an agent by a human slug, and create_agent records
        // that slug as the name. name == slug there is a REAL name.
        assert_eq!(agent_label("reporter", "reporter"), "reporter");
        assert_eq!(agent_label("recon-mapper", "recon-mapper"), "recon-mapper");

        // The app's `+` flow assigns a UUID slug; if its placeholder stamp
        // is lost the name equals that UUID — hide it, never show a raw id.
        let uuid = new_uuid_id();
        assert_eq!(agent_label(&uuid, &uuid), "unnamed agent");
        // The stamped placeholder (name != the UUID slug) shows as itself.
        assert_eq!(agent_label("unnamed agent", &uuid), "unnamed agent");
        // A real name over a UUID slug wins.
        assert_eq!(agent_label("hunter", &uuid), "hunter");
        // No name at all falls back.
        assert_eq!(agent_label("", "reporter"), "unnamed agent");
    }

    #[test]
    fn uuid_shape_detection_rejects_human_slugs() {
        assert!(is_uuid_like(&new_uuid_id()));
        assert!(!is_uuid_like("reporter"));
        assert!(!is_uuid_like("recon-mapper"));
        assert!(!is_uuid_like("")); // empty is not a uuid
        // Right length, wrong content (a 'z' where hex is required).
        assert!(!is_uuid_like("zzzzzzzz-zzzz-zzzz-zzzz-zzzzzzzzzzzz"));
    }

    #[test]
    fn generated_ids_differ_across_calls() {
        let a = new_uuid_id();
        let b = new_uuid_id();
        assert_ne!(a, b);
    }

    fn mission(created: u64) -> Mission {
        Mission {
            agent: "operator".to_string(),
            pins: std::collections::BTreeMap::new(),
            budget: None,
            created,
            name: None,
            session: None,
            opencode_session: None,
            launch_requested: None,
        }
    }

    #[test]
    fn only_a_painting_session_counts_as_working() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        // Nothing up: the session state below is irrelevant.
        assert_eq!(activity_for(false, Some(now)), MissionActivity::Idle);
        // Live and painting right now — the pulse is earned.
        assert_eq!(activity_for(true, Some(now)), MissionActivity::Working);
        // Live but quiet past the window: an opencode TUI parked at its
        // prompt. This is the case that used to pulse forever.
        let stale = now - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1);
        assert_eq!(activity_for(true, Some(stale)), MissionActivity::Waiting);
        // Live with no capture to read: absence of evidence, not work.
        assert_eq!(activity_for(true, None), MissionActivity::Waiting);
    }

    #[test]
    fn missions_sort_newest_created_first() {
        let list = vec![
            ("b-old".to_string(), mission(100)),
            ("a-new".to_string(), mission(300)),
            ("c-mid".to_string(), mission(200)),
            ("d-tie".to_string(), mission(300)),
        ];
        let sorted = sort_missions(list);
        let order: Vec<&str> = sorted.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(order, ["a-new", "d-tie", "c-mid", "b-old"]);
    }
}
