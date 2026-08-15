//! The app's thin state layer.
//!
//! House rule (app-flow-plan chunk 0): widgets never touch the filesystem
//! or the corpus-core store API directly — every corpus-core call goes
//! through `AppState`, and widgets only render state and request actions.
//! Business logic (validation, store plumbing) lives here or in corpus-core,
//! never in a view.

use std::collections::{BTreeMap, hash_map::RandomState};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;

use corpus_core::{
    AgentConfig, CorpusStats, CostReport, Error, Mission, PluginStatus, Project, RunLine,
    RunSession, SourceRevs, Store,
};

use crate::nav::Screen;

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
    /// Missions of `selected_project`, sorted by slug (corpus-core order).
    pub missions: Vec<(String, Mission)>,
    /// Which project `missions` belongs to; a stale pair is never trusted.
    missions_project: Option<String>,
    /// The last computed corpus summary (files/bytes per category) and
    /// cost report for `corpus_stats_project` — refreshed on selection
    /// change + manually.
    corpus_stats: Option<CorpusStats>,
    corpus_cost: Option<CostReport>,
    corpus_stats_project: Option<String>,
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
    /// The exported transcript path for a dismissed run.
    pub export_path: Option<String>,
    /// Live corpus tmux sessions seen at the last `refresh_live_sessions`
    /// — the re-attach list a relaunched app offers (chunk 7).
    pub live_sessions: Vec<String>,
}

/// Who the active (or last-finished) run was.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// The embedded-PTY attach argv captured at launch (None = piped
    /// fallback): the pane must outlive the dropped session handle, so
    /// attach state lives on the META, not the backend.
    pub pty_attach: Option<Vec<String>>,
}

/// The final state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Exited on its own with this code (piped headless, or a TUI run
    /// whose tmux session ended — the operator quit opencode).
    Exited(i32),
    /// Torn down by the operator (best-effort transcript export first).
    Aborted,
    /// Gracefully closed by the operator: transcript exported first.
    Dismissed,
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
            corpus_cost: None,
            corpus_stats_project: None,
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
            export_path: None,
            live_sessions: Vec::new(),
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
                    missions: self.store.list_missions(slug).unwrap_or_default(),
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

    /// Re-list a project's missions (and keep its tree subtree fresh).
    pub fn refresh_missions(&mut self, project: &str) {
        self.missions = self.store.list_missions(project).unwrap_or_default();
        self.missions_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.missions = self.missions.clone();
        }
    }

    /// Re-walk a project's corpus (files/bytes per category, and the
    /// token/cost aggregation over run exports) for the sidebar summary
    /// and the project view.
    pub fn refresh_corpus_stats(&mut self, project: &str) {
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.corpus_cost = corpus_core::corpus_cost(&self.store, project).ok();
        self.corpus_stats_project = Some(project.to_string());
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

    /// The current env-status aggregation for a project's plugin, as a
    /// `(name, ready)` pair plus the probe notes.
    pub fn env_status(&self, project: &str) -> Option<(String, bool, String)> {
        let (_slug, spec) = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)?;
        self.plugins
            .iter()
            .find(|p| p.name == spec.plugin)
            .map(|p| (p.name.clone(), p.ready, p.notes.clone()))
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

    /// The project view's cost report for the selected project.
    pub fn corpus_cost(&self) -> Option<&CostReport> {
        self.corpus_cost.as_ref()
    }

    /// Create a project. The human gives the display name; the machine
    /// gives the id — an auto-generated UUIDv4, which is a valid
    /// kebab-case slug, so it slots straight into the store layout
    /// (`store/projects/<id>/`), CLI scopes, and `CORPUS_PROJECT`.
    pub fn create_project(&self, name: &str, plugin: &str) -> Result<(String, Project), Error> {
        let id = new_uuid_id();
        self.store.create_project(&id, name, plugin).map(|p| (id, p))
    }

    /// Clone a project with a fresh auto-generated id; the copied name
    /// falls back to the source's when none is given.
    pub fn clone_project(
        &self,
        from: &str,
        name: Option<&str>,
        with_corpus: bool,
    ) -> Result<(String, Project), Error> {
        let id = new_uuid_id();
        self.store
            .clone_project(from, &id, name, with_corpus)
            .map(|p| (id, p))
    }

    pub fn delete_project(&self, slug: &str) -> Result<(), Error> {
        self.store.delete_project(slug)
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

    /// Clone an agent.
    pub fn clone_agent(&self, project: &str, from: &str) -> Result<(), Error> {
        let id = new_uuid_id();
        self.store.clone_agent(project, from, &id)
    }

    /// Delete an agent.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<(), Error> {
        self.store.delete_agent(project, slug)
    }

    /// Clone a core seed into the project as a new (auto-id'd) agent —
    /// the sidebar's "+ agent → clone-from-seed" flow. `seed` is a
    /// core-seed name (`operator` / `researcher`), or `blank` for the
    /// empty config.
    pub fn create_agent_from_seed(&self, project: &str, seed: &str) -> Result<String, Error> {
        let id = new_uuid_id();
        if seed == "blank" {
            self.store.create_blank_agent(project, &id)?;
        } else {
            self.store.create_agent_from_seed(project, &id, seed)?;
        }
        Ok(id)
    }

    /// Create a mission record: auto-id slug, the agent ref, the current
    /// top-bar pins stamped in, status `queued`. Returns the mission slug.
    pub fn create_mission(&self, project: &str, agent: &str, brief: &str) -> Result<String, Error> {
        let id = new_uuid_id();
        let mission = Mission {
            agent: agent.to_string(),
            pins: self.source_pins.clone(),
            budget: None,
            status: "queued".to_string(),
            created: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            name: None,
            session: None,
            opencode_session: None,
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
    /// scope. One active run at a time. `source_pins_json` is the
    /// resolved `repo -> sha` map exported to the run (None = the
    /// plugin's default pins).
    pub fn launch(
        &mut self,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
    ) -> Result<(), Error> {
        if self.run.is_some() {
            return Err(Error::Store(
                "a run is already active — abort or wait for it first".into(),
            ));
        }
        // Fail loudly on an unknown agent, then materialize the WHOLE
        // project: the agent list opencode shows is project-scoped.
        self.store.load_agent(project, agent)?;
        self.store.render_project_agents(project)?;
        let session = RunSession::spawn(project, agent, model, mission, source_pins_json)?;
        let pty_attach = session.pty_attach_command();
        self.run = Some(session);
        self.run_meta = Some(RunMeta { pty_attach });
        self.run_mission = None;
        self.run_lines.clear();
        self.run_status = None;
        Ok(())
    }

    /// Drain any new transcript lines; mark the run finished the moment
    /// it exits. Called every frame by the Launch screen.
    pub fn poll_run(&mut self) {
        let Some(mut session) = self.run.take() else {
            return;
        };
        while let Some(line) = session.poll_line() {
            self.run_lines.push(line);
        }
        if let Some(status) = session.try_exit() {
            // An operator abort already recorded its terminal state;
            // everything else surfaces as its exit code.
            if self.run_status != Some(RunStatus::Aborted) {
                self.run_status = Some(RunStatus::Exited(status.code().unwrap_or(1)));
            }
            return;
        }
        self.run = Some(session);
    }

    /// Operator-initiated tear-down: best-effort transcript export,
    /// then kill the whole session tree. The run is immediately over.
    pub fn abort_run(&mut self) {
        if let Some(mut session) = self.run.take() {
            session.abort();
        }
        self.run_status = Some(RunStatus::Aborted);
    }

    /// Graceful close: export the transcript of record (a TUI run's
    /// `<epoch>-<agent>.json`), then close the run. On export failure
    /// the run stays live so the operator can abort instead.
    pub fn dismiss_run(&mut self) -> Result<(), Error> {
        let Some(mut session) = self.run.take() else {
            return Err(Error::Store("no run to dismiss".into()));
        };
        match session.dismiss() {
            Ok(export) => {
                self.export_path = Some(export.display().to_string());
                self.run_status = Some(RunStatus::Dismissed);
                Ok(())
            }
            Err(error) => {
                self.run = Some(session);
                Err(error)
            }
        }
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

    /// Launch a mission's run: a BARE opencode TUI (empty prompt — the
    /// operator types the mission into opencode's own input), then persist
    /// the spawned tmux session on the mission record so a relaunched app
    /// re-attaches by selection. One active run at a time: a live run is
    /// REPLACED (transcript exported when possible, torn down either way)
    /// so a new mission always lands on a fresh opencode session.
    pub fn launch_mission(&mut self, project: &str, agent: &str, slug: &str) -> Result<(), Error> {
        // Resolve the mission's rev pins to shas + fetch trees FIRST —
        // a failed resolution (offline, tag gone) must not tear down the
        // working run for nothing.
        let mission_record = self.store.load_mission(project, slug)?;
        let prepared = corpus_core::prepare_source_pins(&self.store, project, &mission_record.pins)?;
        let pins_json = if prepared.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&prepared)?)
        };
        if self.run_active() {
            let replaced = self.live_run_session();
            if self.dismiss_run().is_err() {
                self.abort_run();
            }
            // Clear the replaced session off whichever mission held it —
            // the tmux session is dead now; re-attach must not aim at it.
            if let Some(replaced) = replaced {
                let holder = self
                    .missions
                    .iter()
                    .find(|(s, m)| s != slug && m.session.as_deref() == Some(replaced.as_str()))
                    .map(|(s, _)| s.clone());
                if let Some(holder) = holder {
                    let _ = self.set_mission_session(project, &holder, None, None);
                }
            }
        }
        let model = self.agent_default_model(project, agent);
        self.launch(project, agent, model.as_deref(), "", pins_json.as_deref())?;
        self.run_mission = Some(slug.to_string());
        if let Some(session) = self.live_run_session() {
            self.set_mission_session(project, slug, Some(session), None)?;
        }
        self.refresh_missions(project);
        Ok(())
    }

    /// Write the run bookkeeping (tmux session / opencode session) onto a
    /// mission record, preserving its brief.
    fn set_mission_session(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
        opencode_session: Option<String>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.session = session;
        mission.opencode_session = opencode_session;
        self.store.update_mission(project, slug, &mission)
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

    /// Abort a mission's run: `tmux kill-session` on its recorded session
    /// (works whether the app owns the run or it survived an app relaunch).
    pub fn abort_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        let mission = self.store.load_mission(project, slug)?;
        let session = mission.session.as_deref().ok_or_else(|| {
            Error::Store("no live session on this mission — nothing to abort".into())
        })?;
        if self.live_run_session().as_deref() == Some(session) {
            self.abort_run();
        } else {
            corpus_core::kill_tmux_session(session);
        }
        self.refresh_live_sessions();
        Ok(())
    }

    /// Dismiss a mission: export the transcript of record to the project
    /// corpus `runs/`, kill the run, and clear its bookkeeping. Uses the
    /// stored opencode session for re-attached runs; the app-owned run
    /// exports through its own handle.
    pub fn dismiss_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        let mission = self.store.load_mission(project, slug)?;
        let session = mission.session.as_deref();
        let is_owned = session.is_some()
            && self.live_run_session().as_deref() == session;
        let path = if is_owned {
            self.dismiss_run()?;
            self.export_path.clone().unwrap_or_default()
        } else {
            let opencode_id = mission.opencode_session.as_deref().ok_or_else(|| {
                Error::Store("no opencode session recorded — cannot export".into())
            })?;
            let path = corpus_core::export_session(project, &mission.agent, opencode_id)?;
            if let Some(sref) = session {
                corpus_core::kill_tmux_session(sref);
            }
            path.display().to_string()
        };
        self.export_path = Some(path);
        self.set_mission_session(project, slug, None, None)?;
        self.refresh_live_sessions();
        Ok(())
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
    fn generated_ids_differ_across_calls() {
        let a = new_uuid_id();
        let b = new_uuid_id();
        assert_ne!(a, b);
    }
}