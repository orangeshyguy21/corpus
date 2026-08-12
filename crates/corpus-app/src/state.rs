//! The app's thin state layer.
//!
//! House rule (app-flow-plan chunk 0): widgets never touch the filesystem
//! or the corpus-core store API directly — every corpus-core call goes
//! through `AppState`, and widgets only render state and request actions.
//! Business logic (validation, store plumbing) lives here or in corpus-core,
//! never in a view.

use std::collections::HashSet;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;

use corpus_core::{
    AgentConfig, Error, ModelList, PluginStatus, Project, RunLine, RunSession, Store,
};

/// App-wide state: the corpus-core store handle plus the data the
/// screens render. Owned by `App`, passed by reference to the views.
pub struct AppState {
    store: Store,
    /// All projects as `(slug, spec)`, sorted by slug (corpus-core order).
    pub projects: Vec<(String, Project)>,
    /// Discovered plugins with live probe results, refreshed on demand
    /// (`refresh_plugins`) — never per-frame: probing spawns processes
    /// on the host.
    plugins: Vec<PluginStatus>,
    /// Agents of `agents_project`, sorted by slug (corpus-core order).
    pub agents: Vec<(String, AgentConfig)>,
    /// Which project `agents` belongs to; a stale pair is never trusted.
    pub agents_project: Option<String>,
    /// The one active run (launch seam): a single session at a
    /// time by design — the run view is a tail, not a multiplexer.
    run: Option<RunSession>,
    /// Identity of the active (or last-finished) run.
    pub run_meta: Option<RunMeta>,
    /// Transcript lines drained so far (the run view renders these).
    pub run_lines: Vec<RunLine>,
    /// None = still running; Some = final state (set once, at exit).
    pub run_status: Option<RunStatus>,
    /// The exported transcript path for a dismissed run.
    pub export_path: Option<String>,
    /// Live corpus tmux sessions seen at the last `refresh_live_sessions`
    /// — the re-attach list a relaunched app offers (chunk 7).
    pub live_sessions: Vec<String>,
    /// The opencode model list (chunk 8), lazily loaded by
    /// `ensure_models`; corpus-core TTL-caches the shell-out underneath.
    models: Option<ModelList>,
    /// Why the model list is unavailable (pickers degrade to free text
    /// with this as the warning).
    models_error: Option<String>,
    /// In-flight background fetch. The shell-out (0.6s cached, multiple
    /// seconds with --refresh over the network) must NEVER run on the
    /// UI thread — immediate-mode means the whole app freezes.
    models_rx: Option<std::sync::mpsc::Receiver<Result<ModelList, String>>>,
    /// Registry-known model ids (`provider/tag`), the picker's
    /// "benchmarked" badge. Loaded once with the model list.
    benchmarked: Option<HashSet<String>>,
}

/// Who/what the active or last run was.
#[derive(Debug, Clone)]
pub struct RunMeta {
    pub project: String,
    pub agent: String,
    pub transcript: String,
    /// The embedded-PTY attach argv captured at launch (None = piped
    /// fallback): the pane must outlive the dropped session handle, so
    /// attach state lives on the META, not the backend.
    pub pty_attach: Option<Vec<String>>,
}

/// The final state of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunStatus {
    /// Exited on its own with this code (piped headless only — a TUI
    /// run never exits by itself).
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
            projects: Vec::new(),
            plugins: Vec::new(),
            agents: Vec::new(),
            agents_project: None,
            run: None,
            run_meta: None,
            run_lines: Vec::new(),
            run_status: None,
            export_path: None,
            live_sessions: Vec::new(),
            models: None,
            models_error: None,
            models_rx: None,
            benchmarked: None,
        };
        state.refresh();
        state
    }

    /// The store root this app operates on (displayed as reassurance).
    pub fn store_root(&self) -> String {
        self.store.root().display().to_string()
    }

    /// Re-list the projects from the store.
    pub fn refresh(&mut self) {
        self.projects = self.store.list_projects().unwrap_or_default();
    }

    /// Re-probe the discovered plugins (host-side aggregation; the app
    /// never spawns plugins itself).
    pub fn refresh_plugins(&mut self) {
        self.plugins = corpus_core::plugin_status();
    }

    /// A fresh generated id, for anything the app auto-ids (projects,
    /// agent slugs).
    pub fn fresh_id() -> String {
        new_uuid_id()
    }

    /// The last plugin probe results (empty until `refresh_plugins`).
    pub fn plugins(&self) -> &[PluginStatus] {
        &self.plugins
    }

    /// Re-list a project's agents.
    pub fn refresh_agents(&mut self, project: &str) {
        self.agents = self.store.list_agents(project).unwrap_or_default();
        self.agents_project = Some(project.to_string());
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

    /// Wipe a project's corpus (generation counter bumps, corpus gone,
    /// agents survive).
    pub fn wipe_project_corpus(&self, slug: &str) -> Result<Project, Error> {
        self.store.wipe_project_corpus(slug)
    }

    // --- agents ---

    /// Load an agent's opencode.json doc.
    pub fn load_agent(&self, project: &str, slug: &str) -> Result<AgentConfig, Error> {
        self.store.load_agent(project, slug)
    }

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

    /// Create a new blank agent.
    pub fn create_blank_agent(&self, project: &str) -> Result<(String, AgentConfig), Error> {
        let id = new_uuid_id();
        self.store.create_blank_agent(project, &id)?;
        let agent = self.store.load_agent(project, &id)?;
        Ok((id, agent))
    }

    // --- run launch ---

    /// Whether a run is currently live.
    pub fn run_active(&self) -> bool {
        self.run.is_some()
    }

    /// Materialize the agent and spawn the mission on the project
    /// scope. One active run at a time.
    pub fn launch(
        &mut self,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
    ) -> Result<(), Error> {
        if self.run.is_some() {
            return Err(Error::Store(
                "a run is already active — abort or wait for it first".into(),
            ));
        }
        self.store.render_agent(project, agent)?;
        let session = RunSession::spawn(project, agent, model, mission)?;
        let transcript = session.transcript.display().to_string();
        let pty_attach = session.pty_attach_command();
        self.run = Some(session);
        self.run_meta = Some(RunMeta {
            project: project.to_string(),
            agent: agent.to_string(),
            transcript,
            pty_attach,
        });
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

    /// Reset the run view for a fresh launch.
    pub fn clear_run(&mut self) {
        self.run = None;
        self.run_meta = None;
        self.run_lines.clear();
        self.run_status = None;
        self.export_path = None;
    }

    /// The model the launch dialog pre-fills: the registry's curated
    /// tool-use default (an explicit arg — the engine never falls back
    /// to opencode's ambient model). None when the registry is empty.
    pub fn suggested_model(&self) -> Option<String> {
        corpus_core::ModelRegistry::load(&models_yaml_path())
            .ok()?
            .launch_default()
    }

    // --- model list (chunk 8) ---

    /// Load the model list + badge set on first use; a no-op once
    /// either succeeded or failed (the caller's ↻ button retries).
    pub fn ensure_models(&mut self) {
        if self.benchmarked.is_none() {
            self.benchmarked = Some(load_benchmarked_ids());
        }
        if self.models.is_none() && self.models_error.is_none() {
            self.refresh_models(false);
        }
    }

    /// (Re)pull the model list ON A BACKGROUND THREAD; the result lands
    /// via `poll_models` (called every frame from App::update). `force`
    /// bypasses corpus-core's TTL and re-pulls opencode's models.dev
    /// cache — the pickers' ↻ button. A click while a fetch is in
    /// flight is ignored (the UI shows a spinner instead).
    pub fn refresh_models(&mut self, force: bool) {
        if self.models_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(corpus_core::model_list(force).map_err(|e| e.to_string()));
        });
        self.models_rx = Some(rx);
    }

    /// Apply a finished background model fetch. Called every frame.
    pub fn poll_models(&mut self) {
        let Some(rx) = &self.models_rx else { return };
        match rx.try_recv() {
            Ok(Ok(list)) => {
                self.models = Some(list);
                self.models_error = None;
                self.models_rx = None;
            }
            Ok(Err(error)) => {
                self.models = None;
                self.models_error = Some(error);
                self.models_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.models_rx = None;
            }
        }
    }

    /// True while a background model fetch is in flight (the ↻ buttons
    /// show a spinner instead of offering another click).
    pub fn models_loading(&self) -> bool {
        self.models_rx.is_some()
    }

    /// The grouped model list, when available (None = pickers degrade).
    pub fn models(&self) -> Option<&ModelList> {
        self.models.as_ref()
    }

    /// Why the model list is unavailable, for the degrade warning.
    pub fn models_error(&self) -> Option<&str> {
        self.models_error.as_deref()
    }

    /// Registry-known model ids (`provider/tag`) — the picker's
    /// "benchmarked" badge.
    pub fn benchmarked_ids(&self) -> Option<&HashSet<String>> {
        self.benchmarked.as_ref()
    }

    /// The launch-dialog pre-fill for an agent: primary entry model →
    /// registry tool-use default (all explicit choices visible in the
    /// dialog).
    pub fn agent_default_model(&self, project: &str, agent: &str) -> Option<String> {
        corpus_core::launch::agent_default_model(&self.store, project, agent)
            .or_else(|| self.suggested_model())
    }

    /// An agent's config hash (short hex — for display).
    pub fn agent_config_hash(&self, project: &str, slug: &str) -> String {
        self.store.agent_config_hash(project, slug).unwrap_or_else(|_| "??".to_string())
    }

    /// The store handle — exposed for direct CRUD by views when the
    /// pass-through doesn't cover a niche operation.
    pub fn store(&self) -> &Store {
        &self.store
    }
}

/// The registry path (`CORPUS_MODELS` override, else the repo file).
fn models_yaml_path() -> PathBuf {
    std::env::var("CORPUS_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("benchmarks/models.yaml"))
}

/// The registry's model ids as opencode refs (`provider/tag`) — the
/// picker's "benchmarked" badge set.
fn load_benchmarked_ids() -> HashSet<String> {
    corpus_core::ModelRegistry::load(&models_yaml_path())
        .map(|registry| {
            registry
                .models
                .iter()
                .map(|m| format!("{}/{}", m.provider, m.tag))
                .collect()
        })
        .unwrap_or_default()
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