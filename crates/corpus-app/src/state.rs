//! The app's thin state layer.
//!
//! House rule (app-flow-plan chunk 0): widgets never touch the filesystem
//! or the corpus-core store API directly — every corpus-core call goes
//! through `AppState`, and widgets only render state and request actions.
//! Business logic (validation, store plumbing) lives here or in corpus-core,
//! never in a view.

use std::collections::{hash_map::RandomState, BTreeMap, BTreeSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use corpus_core::{
    AgentConfig, CorpusStats, CostReport, Error, FindingCard, FindingIndexCache, Mission,
    MissionDeleteRequest, PluginStatus, Project, RunLine, RunSession, SourceRevs, StopOutcome, Store,
};

use crate::file_watch::{FileInvalidationSource, NotifyFileInvalidationSource};
use crate::jobs::{JobKind, JobScope, JobSet, JobTerminal, StartOutcome};
use crate::nav::Screen;
use crate::session_service::{
    launch_stamp_ms, ConfiguredSessionService, PromptDeliveryState, SessionRef, SessionService,
    SessionTurnState,
};

/// How often the raw captures are re-stat'd. Cheap next to the tmux
/// listing (no subprocess), so it runs on the faster beat.
const ACTIVITY_EVENT_MIN: Duration = Duration::from_millis(100);
/// Notifications are hints. These slower timers reconcile startup, dropped
/// events, watcher failure, and changes made on filesystems without native
/// notification support.
const ACTIVITY_BACKSTOP: Duration = Duration::from_secs(2);
const STORE_BACKSTOP: Duration = Duration::from_secs(10);
/// The fallback transcript is a diagnostic tail, not an in-memory copy of
/// the durable run log. Embedded PTY runs render directly and retain none.
const MAX_RUN_LINES: usize = 4_000;

/// Time is a runtime dependency, not ambient state. Keeping both clocks
/// behind one seam makes polling/activity tests deterministic while retaining
/// unix timestamps for persisted mission records.
trait Clock: Send + Sync {
    fn monotonic_now(&self) -> Instant;
    fn unix_seconds(&self) -> u64;
}

struct SystemClock;

#[derive(Clone, Default)]
struct RunCancellation(crate::jobs::CancellationToken);

impl RunCancellation {
    fn cancel(&self) {
        self.0.cancel();
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Clock for SystemClock {
    fn monotonic_now(&self) -> Instant {
        Instant::now()
    }

    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or(0)
    }
}

/// The process-owning half of a run. App state sees lifecycle facts, not
/// child-process or tmux implementation details.
trait ActiveRun: Send {
    fn poll_line(&mut self) -> Option<RunLine>;
    fn try_exit_code(&mut self) -> Option<i32>;
    fn pty_attach_command(&self) -> Option<Vec<String>>;
    fn stop(&mut self) -> StopOutcome;
    fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String>;
    fn launch_identity(&self) -> Option<String>;
    fn control_port(&self) -> Option<u16>;
}

impl ActiveRun for RunSession {
    fn poll_line(&mut self) -> Option<RunLine> {
        RunSession::poll_line(self)
    }

    fn try_exit_code(&mut self) -> Option<i32> {
        RunSession::try_exit(self).map(|status| status.code().unwrap_or(1))
    }

    fn pty_attach_command(&self) -> Option<Vec<String>> {
        RunSession::pty_attach_command(self)
    }

    fn stop(&mut self) -> StopOutcome {
        RunSession::stop_detailed(self)
    }

    fn opencode_session_id(&mut self, claimed: &BTreeSet<String>) -> Option<String> {
        RunSession::opencode_session_id(self, claimed)
    }

    fn launch_identity(&self) -> Option<String> {
        RunSession::launch_identity(self)
    }

    fn control_port(&self) -> Option<u16> {
        RunSession::control_port(self)
    }
}

trait RunBackend: Send + Sync {
    fn spawn(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error>;

    fn resume(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error>;

    fn prepare_source_pins(
        &self,
        store: &Store,
        project: &str,
        pins: &BTreeMap<String, String>,
        cancellation: &RunCancellation,
    ) -> Result<BTreeMap<String, String>, Error>;

    fn export_session(&self, project: &str, opencode_session_id: &str) -> Result<PathBuf, Error>;
    fn kill_tmux_session(&self, session: &str) -> Result<(), Error>;
}

struct CoreRunBackend;

impl RunBackend for CoreRunBackend {
    fn spawn(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error> {
        if cancellation.is_cancelled() {
            return Err(Error::Store("launch start cancelled".into()));
        }
        debug_assert_eq!(project, run_id.project);
        RunSession::spawn_mission_with_environment(
            run_id,
            agent,
            model,
            mission,
            source_pins_json,
            environment_session,
        )
        .map(|run| Box::new(run) as Box<dyn ActiveRun>)
    }

    fn resume(
        &self,
        run_id: &RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        opencode_session_id: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
        cancellation: &RunCancellation,
    ) -> Result<Box<dyn ActiveRun>, Error> {
        if cancellation.is_cancelled() {
            return Err(Error::Store("launch start cancelled".into()));
        }
        debug_assert_eq!(project, run_id.project);
        RunSession::resume_mission_with_environment(
            run_id,
            agent,
            model,
            opencode_session_id,
            source_pins_json,
            environment_session,
        )
        .map(|run| Box::new(run) as Box<dyn ActiveRun>)
    }

    fn prepare_source_pins(
        &self,
        store: &Store,
        project: &str,
        pins: &BTreeMap<String, String>,
        _cancellation: &RunCancellation,
    ) -> Result<BTreeMap<String, String>, Error> {
        corpus_core::prepare_source_pins(store, project, pins)
    }

    fn export_session(&self, project: &str, opencode_session_id: &str) -> Result<PathBuf, Error> {
        corpus_core::export_session(project, opencode_session_id)
    }

    fn kill_tmux_session(&self, session: &str) -> Result<(), Error> {
        corpus_core::kill_tmux_session_checked(session)
    }
}

/// Read-only discovery for sessions not owned by this app process.
trait SessionCatalog: Send + Sync {
    fn live_tui_sessions(&self) -> Vec<String>;
    fn raw_log(&self, store: &Store, project: &str, session: &str) -> Option<PathBuf>;
}

/// Host-side environment mutation used during launch. Keeping this seam next
/// to the process/session adapters prevents app tests from consulting whatever
/// immutable plugin version happens to be selected in the operator's home.
trait EnvironmentRuntime: Send + Sync {
    fn open(
        &self,
        store: &Store,
        id: RunId,
        source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error>;
}

struct CoreEnvironmentRuntime;

impl EnvironmentRuntime for CoreEnvironmentRuntime {
    fn open(
        &self,
        store: &Store,
        id: RunId,
        source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error> {
        corpus_core::open_environment_session(store, id, source_shas)
    }
}

#[cfg(test)]
struct NoopEnvironmentRuntime;

#[cfg(test)]
impl EnvironmentRuntime for NoopEnvironmentRuntime {
    fn open(
        &self,
        _store: &Store,
        _id: RunId,
        _source_shas: BTreeMap<String, String>,
    ) -> Result<Option<corpus_core::EnvironmentSessionRecord>, Error> {
        Ok(None)
    }
}

struct CoreSessionCatalog;

impl SessionCatalog for CoreSessionCatalog {
    fn live_tui_sessions(&self) -> Vec<String> {
        corpus_core::live_tui_sessions()
    }

    fn raw_log(&self, store: &Store, project: &str, session: &str) -> Option<PathBuf> {
        corpus_core::session_raw_log(store, project, session)
    }
}

/// One project's subtree in the sidebar tree: its agents and missions.
#[derive(Debug, Clone, Default)]
pub struct ProjectTree {
    pub agents: Vec<(String, AgentConfig)>,
    pub missions: Vec<(String, Mission)>,
}

/// A durable environment lease projected for the selected project. Every
/// field comes from Corpus's session record; the Project view never inspects
/// Docker, plugin state directories, or mission files while painting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginLeaseView {
    pub session_key: String,
    /// Operator-facing mission name (name, human slug, or `new`).
    pub mission: String,
    /// Exact durable identity, retained for diagnostics and tooltips.
    pub mission_slug: String,
    pub orphaned: bool,
    pub state: corpus_core::EnvironmentSessionState,
    pub plugin_version: String,
    pub plugin_digest: String,
    pub source_shas: BTreeMap<String, String>,
    pub environment_lock: Option<String>,
    pub image_digest: Option<String>,
    pub drift: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginOperationState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// Retained operator-facing lifecycle state. Progress callbacks update this
/// shared prepared value; the UI only clones it and draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginOperationView {
    pub plugin: String,
    pub operation: String,
    pub state: PluginOperationState,
    pub phase: Option<String>,
    pub detail: String,
    pub recovery: Option<String>,
}

/// App-wide state: the corpus-core store handle plus the data the
/// screens render. Owned by `App`, passed by reference to the views.
pub struct AppState {
    store: Store,
    clock: Arc<dyn Clock>,
    run_backend: Arc<dyn RunBackend>,
    session_catalog: Arc<dyn SessionCatalog>,
    environment_runtime: Arc<dyn EnvironmentRuntime>,
    session_service: Arc<dyn SessionService>,
    file_invalidations: Option<Box<dyn FileInvalidationSource>>,
    file_watch_warning: Option<String>,
    jobs: Option<JobSet<AppJobOutput>>,
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
    source_revs_loading: bool,
    source_revs_error: Option<String>,
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
    corpus_cost_cache: corpus_core::CorpusCostCache,
    finding_index_cache: FindingIndexCache,
    findings: FindingDiscovery,
    findings_project: Option<String>,
    /// Per-project invalidation clocks for corpus-reading background work.
    corpus_revisions: BTreeMap<String, u64>,
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
    plugins_loading: bool,
    plugins_error: Option<String>,
    /// Latest requested binding and the binding owned by the in-flight job.
    /// They differ when a project/picker changes during a probe.
    plugin_probe_target: Option<String>,
    plugin_probe_project: Option<String>,
    plugin_probe_active: Option<Option<String>>,
    plugin_probe_active_project: Option<Option<String>>,
    /// Durable non-closed environment leases loaded by the SAME selected
    /// plugin probe job. There is no lease watcher or render-time I/O.
    plugin_leases: Vec<PluginLeaseView>,
    plugin_operation: Arc<Mutex<Option<PluginOperationView>>>,
    /// The model picker reads this state only; discovery is performed once
    /// per cache lifetime (including failures), or on explicit refresh.
    opencode_models: ModelDiscovery,
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
    /// The one active run (launch seam): a single session at a
    /// time by design — the run view is a tail, not a multiplexer.
    run: Option<Box<dyn ActiveRun>>,
    /// Identity of the active (or last-finished) run.
    pub run_meta: Option<RunMeta>,
    /// Stable ownership of the process handle currently adopted by the app.
    /// Project selection is presentation state and must never redirect it.
    owned_run_id: Option<RunId>,
    /// Explicit lifecycle for every operation still in flight (or failed and
    /// awaiting operator recovery). Absence is the `Idle` state.
    run_phases: BTreeMap<RunId, RunPhase>,
    /// Cooperative cancellation handles exist only while preparation owns
    /// no child process. Cancellation is checked before spawn/adoption.
    run_cancellations: BTreeMap<RunId, RunCancellation>,
    /// Per `(project, mission)` launch generation. Relaunching the same
    /// mission gets a new identity, so late work cannot attach to its successor.
    run_generations: BTreeMap<(String, String), u64>,
    /// Missions whose Delete action is waiting for run/environment teardown.
    /// The record is removed only after cleanup succeeds; failures retain
    /// both the record and this intent so Delete can be retried safely.
    pending_mission_deletes: BTreeSet<(String, String)>,
    /// Per-mission ownership shared by periodic checkpointing and destructive
    /// teardown. Distinct background job kinds are not a synchronization
    /// boundary because both can mutate the same external session.
    session_operation_leases: SessionOperationLeases,
    /// Transcript lines drained so far (the run view renders these).
    pub run_lines: Vec<RunLine>,
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
    session_activity_dirty: bool,
    /// Per tmux session, the moment we last re-exported its usage transcript.
    /// The turn-completion sweep exports only when the session last painted
    /// (its `session_activity` instant) is NEWER than this — so a finished
    /// turn records exactly once, and a session parked quiet at its prompt
    /// is not re-exported every beat.
    last_exported_at: BTreeMap<String, std::time::Instant>,
    /// Failed checkpoint exports are retried on a bounded cadence rather than
    /// every liveness beat. Deletion/Stop bypasses this map and owns its final
    /// best-effort export after the writer is stopped.
    export_retry_after: BTreeMap<String, std::time::Instant>,
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

/// A completed background job's operator-facing notice. Keeping the job kind
/// alongside the message lets the app condense bursts without parsing copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BackgroundNotice {
    pub severity: BackgroundNoticeSeverity,
    pub job_kind: JobKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundNoticeSeverity {
    Info,
    Error,
    /// Silent signal used to end repeat suppression after a job recovers.
    Resolved,
}

impl BackgroundNotice {
    fn info(job_kind: JobKind, message: impl Into<String>) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Info,
            job_kind,
            message: message.into(),
        }
    }

    fn error(job_kind: JobKind, message: impl Into<String>) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Error,
            job_kind,
            message: message.into(),
        }
    }

    fn resolved(job_kind: JobKind) -> Self {
        Self {
            severity: BackgroundNoticeSeverity::Resolved,
            job_kind,
            message: String::new(),
        }
    }
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

/// Render-safe discovery state. Production discovery always runs as a P1 job;
/// widgets only read this value and request an asynchronous refresh.
#[derive(Debug, Clone, Default)]
pub enum ModelDiscovery {
    #[default]
    Loading,
    Ready(corpus_core::ModelList),
    Failed(String),
}

/// Render-safe finding discovery for the selected project. A failed refresh
/// may retain that same project's last good cards, but navigation never does.
#[derive(Debug, Clone, Default)]
pub enum FindingDiscovery {
    #[default]
    Loading,
    Ready(Vec<FindingCard>),
    Failed {
        message: String,
        last_good: Vec<FindingCard>,
    },
}

enum AppJobOutput {
    Plugins {
        target: Option<String>,
        project: Option<String>,
        statuses: Vec<PluginStatus>,
        leases: Vec<PluginLeaseView>,
    },
    PluginInstalled(corpus_core::InstallReceipt),
    PluginLifecycle(PluginLifecycleResult),
    SourceRevisions(Vec<SourceRevs>),
    OpencodeModels(corpus_core::ModelList),
    LaunchReady(LaunchReady),
    CorpusSnapshot(CorpusSnapshot),
    LiveSessions(Vec<String>),
    SessionMaintenance(SessionMaintenance),
    DispatchDeliveries,
    TeardownReady(TeardownReady),
    OrphanCleanup { project: String, plugin: String },
    ProjectScope(ProjectScopeSnapshot),
    LaunchRequests {
        launches: Vec<LaunchRequest>,
        deletions: Vec<DeletionRequest>,
        agent_deletions: Vec<AgentDeletionRequest>,
        project_deletions: Vec<String>,
    },
    ProjectIndex(Vec<(String, Project)>, BTreeMap<String, ProjectTree>),
    Agents(Vec<(String, AgentConfig)>),
    Missions(Vec<(String, Mission)>),
}

struct PluginLifecycleResult {
    plugin: String,
    operation: &'static str,
    phases: Vec<String>,
    result: serde_json::Value,
}

#[derive(Clone, Copy)]
enum LaunchMode {
    AdoptFresh,
    DetachedFresh,
    Resume,
}

struct LaunchReady {
    session: Box<dyn ActiveRun>,
    mode: LaunchMode,
    notice: Option<String>,
    environment_session: Option<String>,
}

struct CorpusSnapshot {
    stats: CorpusStats,
    logs: Vec<corpus_core::MissionLog>,
    cost: Option<(CostReport, corpus_core::CorpusCostCache)>,
    findings: FindingSnapshot,
}

struct FindingSnapshot {
    cards: Vec<FindingCard>,
    cache: FindingIndexCache,
}

struct SessionMaintenance {
    /// Mission slug, exact tmux launch identity, OpenCode conversation id.
    conversations: Vec<(String, String, String)>,
    exported_tmux: Vec<String>,
    export_failure: Option<(String, String)>,
    warning: Option<String>,
}

struct TeardownReady {
    transcript: Option<String>,
    error: Option<String>,
    cleanup_complete: bool,
    retained: Option<Box<dyn ActiveRun>>,
}

struct ProjectScopeSnapshot {
    agents: Vec<(String, AgentConfig)>,
    missions: Vec<(String, Mission)>,
    stats: CorpusStats,
    logs: Vec<corpus_core::MissionLog>,
    findings: FindingSnapshot,
}

struct LaunchRequest {
    project: String,
    slug: String,
    label: String,
    already_live: bool,
}

struct DeletionRequest {
    project: String,
    slug: String,
}

struct AgentDeletionRequest {
    project: String,
    agent: String,
}

#[derive(Debug)]
pub enum StopMissionResult {
    Scheduled,
    Completed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMissionResult {
    Scheduled,
    Completed,
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
fn activity_for(now: Instant, live: bool, last_paint: Option<Instant>) -> MissionActivity {
    corpus_core::activity_from_idle(
        live,
        last_paint.map(|paint| now.saturating_duration_since(paint).as_secs()),
    )
}

/// The turn-completion export gate: given a `Waiting` session's last paint
/// and the moment we last exported it, should we re-export now? Yes only when
/// it painted output more recently than our last export (a real turn since),
/// or was never exported. No paint reading ⇒ nothing to record. Keeps
/// capture to once per completed turn — see [`AppState::sweep_usage_exports`].
fn should_reexport(
    last_paint: Option<std::time::Instant>,
    last_export: Option<std::time::Instant>,
) -> bool {
    match last_paint {
        Some(paint) => last_export.is_none_or(|e| paint > e),
        None => false,
    }
}

/// A live transcript checkpoint is useful only after a turn has settled.
/// Delete owns teardown and its final best-effort export, so ordinary
/// maintenance must never race it. A failed checkpoint is also held until
/// its retry deadline rather than being relaunched on every two-second beat.
fn checkpoint_export_due(
    delete_requested: bool,
    activity: MissionActivity,
    last_paint: Option<std::time::Instant>,
    last_export: Option<std::time::Instant>,
    retry_after: Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    !delete_requested
        && activity == MissionActivity::Waiting
        && retry_after.is_none_or(|deadline| now >= deadline)
        && should_reexport(last_paint, last_export)
}

/// Reload the complete durable launch ancestry. Parent deletion requests do
/// not immediately stamp every child mission, so checking only the mission
/// leaves a reconciliation window in which a run could still be spawned or
/// adopted into a deleting agent/project.
fn load_launchable_mission(store: &Store, project: &str, mission: &str) -> Result<Mission, Error> {
    let mission_record = store.load_mission(project, mission)?;
    if mission_record.delete_requested.is_some() {
        return Err(Error::Store(format!(
            "mission {project}/{mission} is pending deletion"
        )));
    }
    if Project::load(store, project)?.delete_requested.is_some() {
        return Err(Error::Store(format!(
            "project {project} is pending deletion"
        )));
    }
    if store
        .load_agent(project, &mission_record.agent)?
        .meta
        .delete_requested
        .is_some()
    {
        return Err(Error::Store(format!(
            "agent {project}/{} is pending deletion",
            mission_record.agent
        )));
    }
    Ok(mission_record)
}

/// One mission may have discovery, checkpointing, Stop, and Delete requests
/// arrive on different background-job keys. The lease is the authoritative
/// writer boundary: checkpoint export and teardown for the same mission can
/// never overlap, while unrelated missions retain full concurrency.
#[derive(Clone, Default)]
struct SessionOperationLeases(Arc<Mutex<BTreeMap<(String, String), Weak<Mutex<()>>>>>);

impl SessionOperationLeases {
    fn claim(&self, project: &str, mission: &str) -> Arc<Mutex<()>> {
        let mut leases = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        leases.retain(|_, lease| lease.strong_count() != 0);
        let key = (project.to_string(), mission.to_string());
        if let Some(lease) = leases.get(&key).and_then(Weak::upgrade) {
            return lease;
        }
        let lease = Arc::new(Mutex::new(()));
        leases.insert(key, Arc::downgrade(&lease));
        lease
    }
}

/// Who the active (or last-finished) run was.
#[derive(Debug, Clone)]
pub struct RunMeta {
    /// The embedded-PTY attach argv captured at launch (None = piped
    /// fallback): the pane must outlive the dropped session handle, so
    /// attach state lives on the META, not the backend.
    pub pty_attach: Option<Vec<String>>,
}

/// Stable identity carried by every operation belonging to a run.
pub type RunId = corpus_core::EnvironmentSessionId;

/// A run that ended on its own, queued for one report to the operator.
/// `mission` is the display label of the mission it belonged to (None for
/// a non-mission run), resolved at exit while the bookkeeping still says
/// who it was.
#[derive(Debug, Clone)]
pub struct RunExit {
    pub mission: Option<String>,
    pub code: i32,
}

struct StopAttempt {
    transcript: PathBuf,
    error: Option<Error>,
    cleanup_complete: bool,
}

/// A run operation's authoritative app lifecycle. Durable mission/session
/// records remain the truth after a successful detached handoff (`Idle`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunPhase {
    Idle,
    Preparing,
    Starting,
    Running,
    Stopping,
    Exporting,
    Failed {
        at: RunPhaseKind,
        message: String,
        recoverable: bool,
        cleanup_pending: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunPhaseKind {
    Preparing,
    Starting,
    Running,
    Stopping,
    Exporting,
}

impl RunPhase {
    fn blocks_deletion(&self) -> bool {
        matches!(
            self,
            Self::Preparing
                | Self::Starting
                | Self::Running
                | Self::Stopping
                | Self::Exporting
                | Self::Failed {
                    cleanup_pending: true,
                    ..
                }
        )
    }

    fn allows_delete_action(&self) -> bool {
        !matches!(
            self,
            Self::Preparing | Self::Starting | Self::Stopping | Self::Exporting
        )
    }
}

impl AppState {
    pub fn install_job_runtime(&mut self, context: eframe::egui::Context) {
        let wake = Arc::new(context.clone());
        self.jobs = Some(JobSet::new(wake.clone()));
        match NotifyFileInvalidationSource::new(self.store.projects_dir(), wake) {
            Ok(watcher) => self.file_invalidations = Some(Box::new(watcher)),
            Err(error) => {
                self.file_watch_warning = Some(format!(
                    "filesystem notifications unavailable ({error}); using timed reconciliation"
                ));
            }
        }
    }

    /// Drain coalesced filesystem hints. They only make reconciliation due;
    /// the callback never edits app state and a path is never treated as a
    /// precise run event.
    pub fn poll_file_invalidations(&mut self) -> Option<String> {
        let invalidations = self
            .file_invalidations
            .as_ref()
            .map(|source| source.take())
            .unwrap_or_default();
        let warning = self
            .file_watch_warning
            .take()
            .or(invalidations.warning.clone());
        if invalidations.is_empty() {
            return warning;
        }

        if invalidations.project_index {
            self.refresh();
        }
        if invalidations.all_projects {
            let projects: Vec<String> =
                self.projects.iter().map(|(slug, _)| slug.clone()).collect();
            for project in projects {
                self.note_corpus_mutation(&project);
            }
        } else {
            for project in &invalidations.corpus {
                self.note_corpus_mutation(project);
            }
        }
        let selected = self.effective_project();
        let applies = |projects: &BTreeSet<String>| {
            invalidations.all_projects
                || selected
                    .as_ref()
                    .is_some_and(|project| projects.contains(project))
        };
        if applies(&invalidations.metadata) || applies(&invalidations.corpus) {
            self.corpus_polled_at = None;
        }
        if applies(&invalidations.metadata) {
            self.launch_requests_polled_at = None;
        }
        if applies(&invalidations.activity) {
            self.session_activity_dirty = true;
        }
        warning
    }

    fn job_scope(&self, project: &str, run_id: Option<RunId>) -> JobScope {
        let project_generation = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .map(|(_, project)| project.corpus_generation)
            .unwrap_or(0);
        JobScope {
            project: project.to_string(),
            project_generation,
            corpus_revision: None,
            run_id,
        }
    }

    fn corpus_revision(&self, project: &str) -> u64 {
        self.corpus_revisions.get(project).copied().unwrap_or(0)
    }

    fn corpus_job_scope(&self, project: &str) -> JobScope {
        let mut scope = self.job_scope(project, None);
        scope.corpus_revision = Some(self.corpus_revision(project));
        scope
    }

    /// Mark corpus projections dirty after a known local mutation. Filesystem
    /// notifications call the same seam for external writes.
    pub fn note_corpus_mutation(&mut self, project: &str) {
        let revision = self
            .corpus_revisions
            .entry(project.to_string())
            .or_default();
        *revision = revision.saturating_add(1);
        if self.effective_project().as_deref() == Some(project) {
            self.corpus_polled_at = None;
        }
    }

    /// Apply all completed jobs that still belong to current state. The
    /// returned notices are rendered as toasts by `App`; discovery failures
    /// also remain in their field state so reopening a picker is explanatory.
    pub fn poll_background_jobs(&mut self) -> Vec<BackgroundNotice> {
        let Some(mut jobs) = self.jobs.take() else {
            return Vec::new();
        };
        let results = jobs.drain_applicable(|scope| self.job_scope_current(scope));
        self.jobs = Some(jobs);
        let mut notices = Vec::new();
        for result in results {
            if self.retry_stale_corpus_job(result.kind, &result.scope) {
                continue;
            }
            if matches!(&result.terminal, JobTerminal::Success(_)) {
                notices.push(BackgroundNotice::resolved(result.kind));
            }
            match result.terminal {
                JobTerminal::Success(AppJobOutput::Plugins {
                    target,
                    project,
                    statuses,
                    leases,
                }) => {
                    self.plugin_probe_active = None;
                    self.plugin_probe_active_project = None;
                    if target == self.plugin_probe_target && project == self.plugin_probe_project {
                        self.plugins = merge_plugin_statuses(&self.plugins, statuses);
                        self.plugin_leases = leases;
                        self.plugins_loading = false;
                        self.plugins_error = None;
                    } else {
                        // A project/picker changed while the old global-keyed
                        // job was running. Its catalog is stale for health;
                        // now that the key is free, immediately probe the
                        // latest requested binding.
                        let desired = self.plugin_probe_target.clone();
                        self.refresh_plugins(desired.as_deref());
                    }
                }
                JobTerminal::Success(AppJobOutput::PluginInstalled(receipt)) => {
                    if let Some(operation) = self.plugin_operation.lock().unwrap().as_mut() {
                        operation.plugin = receipt.id.clone();
                    }
                    self.finish_plugin_operation(
                        PluginOperationState::Succeeded,
                        format!(
                            "installed {}@{} · {}",
                            receipt.id, receipt.version, receipt.digest
                        ),
                        None,
                    );
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        format!("installed {}@{}", receipt.id, receipt.version),
                    ));
                    let desired = self.plugin_probe_target.clone();
                    self.refresh_plugins(desired.as_deref());
                }
                JobTerminal::Success(AppJobOutput::PluginLifecycle(lifecycle)) => {
                    let phases = if lifecycle.phases.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", lifecycle.phases.join(" → "))
                    };
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        format!(
                            "{} {} complete{}: {}",
                            lifecycle.plugin, lifecycle.operation, phases, lifecycle.result
                        ),
                    ));
                    self.finish_plugin_operation(
                        PluginOperationState::Succeeded,
                        lifecycle.result.to_string(),
                        None,
                    );
                    let desired = self.plugin_probe_target.clone();
                    self.refresh_plugins(desired.as_deref());
                }
                JobTerminal::Success(AppJobOutput::SourceRevisions(revs)) => {
                    self.apply_source_revisions(&result.scope.project, revs);
                    self.source_revs_loading = false;
                    self.source_revs_error = None;
                }
                JobTerminal::Success(AppJobOutput::OpencodeModels(models)) => {
                    self.opencode_models = ModelDiscovery::Ready(models);
                }
                JobTerminal::Success(AppJobOutput::LaunchReady(ready)) => {
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if let Err(error) = self.apply_launch_ready(run_id, ready) {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error.to_string(),
                            );
                            notices.push(BackgroundNotice::error(
                                result.kind,
                                error.to_string(),
                            ));
                        }
                    }
                }
                JobTerminal::Success(AppJobOutput::CorpusSnapshot(snapshot)) => {
                    let project = result.scope.project;
                    self.corpus_stats = Some(snapshot.stats);
                    self.mission_logs = snapshot.logs;
                    if let Some((cost, cache)) = snapshot.cost {
                        self.corpus_cost = Some(cost);
                        self.corpus_cost_cache = cache;
                    }
                    self.apply_findings(project.as_str(), snapshot.findings);
                    self.corpus_stats_project = Some(project);
                    self.corpus_polled_at = Some(self.clock.monotonic_now());
                }
                JobTerminal::Success(AppJobOutput::LiveSessions(sessions)) => {
                    self.live_sessions = sessions;
                    self.live_sessions_polled_at = Some(self.clock.monotonic_now());
                    self.reconcile_mission_dispatches();
                    self.schedule_dispatch_deliveries();
                    if let Some(project) = self.effective_project() {
                        self.schedule_session_maintenance(&project);
                    }
                }
                JobTerminal::Success(AppJobOutput::SessionMaintenance(maintenance)) => {
                    let project = result.scope.project;
                    if let Some(warning) = maintenance.warning {
                        notices.push(BackgroundNotice::info(result.kind, warning));
                    }
                    let mut missions_changed = false;
                    for (slug, tmux, conversation) in maintenance.conversations {
                        // The project generation guard is not enough: a
                        // mission can stop and relaunch within one project
                        // generation. Bind only if this is still the exact
                        // launch the worker inspected.
                        let durable_launch_is_current = load_launchable_mission(
                            &self.store,
                            &project,
                            &slug,
                        )
                        .is_ok_and(|mission| {
                            mission.session.as_deref() == Some(tmux.as_str())
                                && mission.opencode_session.is_none()
                        });
                        let launch_is_current = durable_launch_is_current
                            && self
                            .trees
                            .get(&project)
                            .into_iter()
                            .flat_map(|tree| tree.missions.iter())
                            .find(|(candidate, _)| candidate == &slug)
                            .is_some_and(|(_, mission)| {
                                mission.session.as_deref() == Some(tmux.as_str())
                                    && mission.opencode_session.is_none()
                                    && mission.delete_requested.is_none()
                            });
                        if !launch_is_current {
                            continue;
                        }
                        if self
                            .set_opencode_session(&project, &slug, Some(conversation))
                            .is_ok()
                        {
                            missions_changed = true;
                        }
                    }
                    let exported = !maintenance.exported_tmux.is_empty();
                    for tmux in maintenance.exported_tmux {
                        self.export_retry_after.remove(&tmux);
                        self.last_exported_at
                            .insert(tmux, self.clock.monotonic_now());
                    }
                    if let Some((tmux, error)) = maintenance.export_failure {
                        self.export_retry_after.insert(
                            tmux,
                            self.clock.monotonic_now() + Duration::from_secs(30),
                        );
                        notices.push(BackgroundNotice::error(
                            result.kind,
                            format!("transcript checkpoint failed: {error}"),
                        ));
                    }
                    if missions_changed {
                        self.refresh_missions(&project);
                    }
                    if exported {
                        self.note_corpus_mutation(&project);
                        self.refresh_corpus_stats(&project);
                    }
                    self.schedule_dispatch_deliveries();
                }
                JobTerminal::Success(AppJobOutput::DispatchDeliveries) => {}
                JobTerminal::Success(AppJobOutput::TeardownReady(ready)) => {
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        let (is_error, message) = self.apply_teardown_ready(run_id, ready);
                        notices.push(if is_error {
                            BackgroundNotice::error(result.kind, message)
                        } else {
                            BackgroundNotice::info(result.kind, message)
                        });
                    }
                }
                JobTerminal::Success(AppJobOutput::OrphanCleanup { project, plugin }) => {
                    if self.effective_project().as_deref() == Some(project.as_str()) {
                        self.refresh_plugins(Some(&plugin));
                    }
                }
                JobTerminal::Success(AppJobOutput::ProjectScope(snapshot)) => {
                    let project = result.scope.project;
                    self.agents = snapshot.agents;
                    self.agents_project = Some(project.clone());
                    self.missions = snapshot.missions;
                    self.missions_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.agents = self.agents.clone();
                        tree.missions = self.missions.clone();
                    }
                    self.corpus_stats = Some(snapshot.stats);
                    self.mission_logs = snapshot.logs;
                    self.apply_findings(project.as_str(), snapshot.findings);
                    self.corpus_stats_project = Some(project);
                    self.corpus_polled_at = Some(self.clock.monotonic_now());
                }
                JobTerminal::Success(AppJobOutput::LaunchRequests {
                    launches,
                    deletions,
                    agent_deletions,
                    project_deletions,
                }) => {
                    self.apply_mission_requests(
                        deletions,
                        agent_deletions,
                        project_deletions,
                        launches,
                    );
                }
                JobTerminal::Success(AppJobOutput::ProjectIndex(projects, trees)) => {
                    self.projects = projects;
                    self.trees = trees;
                }
                JobTerminal::Success(AppJobOutput::Agents(agents)) => {
                    let project = result.scope.project;
                    self.agents = agents;
                    self.agents_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.agents = self.agents.clone();
                    }
                }
                JobTerminal::Success(AppJobOutput::Missions(missions)) => {
                    let project = result.scope.project;
                    self.missions = missions;
                    self.missions_project = Some(project.clone());
                    if let Some(tree) = self.trees.get_mut(&project) {
                        tree.missions = self.missions.clone();
                    }
                }
                JobTerminal::Failure(error) => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, &error);
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error,
                            );
                        }
                        let wrapped = Error::Store(error.clone());
                        let teardown = result.kind == JobKind::SessionTeardown;
                        self.fail_run(
                            run_id,
                            if teardown {
                                RunPhaseKind::Stopping
                            } else {
                                RunPhaseKind::Preparing
                            },
                            &wrapped,
                            true,
                            teardown,
                        );
                    }
                    notices.push(BackgroundNotice::error(result.kind, error));
                }
                JobTerminal::Cancelled => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, "cancelled");
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                "launch cancelled",
                            );
                        }
                        self.finish_run(run_id);
                    }
                    notices.push(BackgroundNotice::info(
                        result.kind,
                        "background work cancelled",
                    ));
                }
                JobTerminal::TimedOut => {
                    if self.retry_stale_plugin_probe(result.kind) {
                        continue;
                    }
                    self.mark_job_failed(result.kind, &result.scope, "timed out");
                    if let Some(run_id) = result.scope.run_id.as_ref() {
                        let error = Error::Store(format!("{} timed out", result.kind.label()));
                        if result.kind == JobKind::LaunchPreparation {
                            self.record_dispatch_launch_failure(
                                &run_id.project,
                                &run_id.mission,
                                &error.to_string(),
                            );
                        }
                        let teardown = result.kind == JobKind::SessionTeardown;
                        self.fail_run(
                            run_id,
                            if teardown {
                                RunPhaseKind::Stopping
                            } else {
                                RunPhaseKind::Preparing
                            },
                            &error,
                            true,
                            teardown,
                        );
                    }
                    notices.push(BackgroundNotice::error(
                        result.kind,
                        format!("{} timed out", result.kind.label()),
                    ));
                }
            }
        }
        notices
    }

    fn retry_stale_plugin_probe(&mut self, kind: JobKind) -> bool {
        if kind != JobKind::PluginProbe {
            return false;
        }
        let Some(finished) = self.plugin_probe_active.take() else {
            return false;
        };
        let finished_project = self.plugin_probe_active_project.take().flatten();
        if finished == self.plugin_probe_target && finished_project == self.plugin_probe_project {
            return false;
        }
        let desired = self.plugin_probe_target.clone();
        self.refresh_plugins(desired.as_deref());
        true
    }

    fn retry_stale_corpus_job(&mut self, kind: JobKind, scope: &JobScope) -> bool {
        let Some(captured_revision) = scope.corpus_revision else {
            return false;
        };
        if !matches!(
            kind,
            JobKind::ProjectScope | JobKind::CorpusSummary | JobKind::CorpusCost
        ) || captured_revision == self.corpus_revision(&scope.project)
        {
            return false;
        }
        match kind {
            JobKind::CorpusSummary => self.schedule_corpus_refresh(&scope.project, false),
            JobKind::CorpusCost => self.schedule_corpus_refresh(&scope.project, true),
            JobKind::ProjectScope => {
                self.corpus_polled_at = None;
                self.poll_project_scope();
            }
            _ => unreachable!(),
        }
        true
    }

    fn mark_job_failed(&mut self, kind: JobKind, scope: &JobScope, error: &str) {
        match kind {
            JobKind::PluginProbe => {
                self.plugin_probe_active = None;
                self.plugin_probe_active_project = None;
                self.plugins_loading = false;
                self.plugins_error = Some(error.to_string());
            }
            JobKind::PluginInstall
            | JobKind::PluginSetup
            | JobKind::PluginDoctor
            | JobKind::PluginStop => {
                let state = if error == "cancelled" {
                    PluginOperationState::Cancelled
                } else {
                    PluginOperationState::Failed
                };
                self.finish_plugin_operation(
                    state,
                    error.to_string(),
                    plugin_recovery_hint(error).map(str::to_string),
                );
            }
            JobKind::SourceRevisions => {
                // Keep the previous revision list/pins; only the refresh failed.
                self.source_revs_loading = false;
                self.source_revs_error = Some(error.to_string());
            }
            JobKind::ModelDiscovery => {
                self.opencode_models = ModelDiscovery::Failed(error.to_string());
            }
            JobKind::ProjectScope | JobKind::CorpusSummary | JobKind::CorpusCost => {
                self.fail_findings(&scope.project, error);
            }
            _ => {}
        }
    }

    fn finish_plugin_operation(
        &self,
        state: PluginOperationState,
        detail: String,
        recovery: Option<String>,
    ) {
        let mut operation = self.plugin_operation.lock().unwrap();
        if let Some(current) = operation.as_mut() {
            current.state = state;
            current.detail = detail;
            current.recovery = recovery;
        }
    }

    fn apply_findings(&mut self, project: &str, snapshot: FindingSnapshot) {
        if self.findings_project.as_deref() != Some(project) {
            return;
        }
        self.finding_index_cache = snapshot.cache;
        self.findings = FindingDiscovery::Ready(snapshot.cards);
    }

    fn fail_findings(&mut self, project: &str, message: &str) {
        if self.findings_project.as_deref() != Some(project) {
            return;
        }
        let last_good = match std::mem::take(&mut self.findings) {
            FindingDiscovery::Ready(cards) => cards,
            FindingDiscovery::Failed { last_good, .. } => last_good,
            FindingDiscovery::Loading => Vec::new(),
        };
        self.findings = FindingDiscovery::Failed {
            message: message.to_string(),
            last_good,
        };
    }

    fn apply_source_revisions(&mut self, project: &str, revs: Vec<SourceRevs>) {
        if !revs.is_empty() {
            let stored: BTreeMap<String, String> = self
                .projects
                .iter()
                .find(|(slug, _)| slug == project)
                .map(|(_, project)| project.pins.clone())
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

    /// Whether a background result still belongs to current UI/run state.
    /// Non-run work follows the selected project; run work follows its stable
    /// generation even when the operator navigates elsewhere.
    pub fn job_scope_current(&self, scope: &crate::jobs::JobScope) -> bool {
        if scope.project.is_empty() {
            return scope.run_id.is_none();
        }
        let project_current = self.projects.iter().any(|(slug, project)| {
            slug == &scope.project && project.corpus_generation == scope.project_generation
        });
        if !project_current {
            return false;
        }
        match &scope.run_id {
            Some(run_id) => {
                run_id.project == scope.project
                    && self
                        .run_generations
                        .get(&(run_id.project.clone(), run_id.mission.clone()))
                        .is_some_and(|generation| *generation == run_id.generation)
            }
            None => self.effective_project().as_deref() == Some(scope.project.as_str()),
        }
    }

    pub fn from_env_deferred(context: eframe::egui::Context) -> Self {
        let mut state = Self::with_runtime_inner(
            Store::from_env(),
            Arc::new(SystemClock),
            Arc::new(CoreRunBackend),
            Arc::new(CoreSessionCatalog),
            Arc::new(CoreEnvironmentRuntime),
            Arc::new(ConfiguredSessionService::from_env()),
            false,
        );
        state.install_job_runtime(context);
        state.refresh();
        state
    }

    #[cfg(test)]
    fn with_runtime(
        store: Store,
        clock: Arc<dyn Clock>,
        run_backend: Arc<dyn RunBackend>,
        session_catalog: Arc<dyn SessionCatalog>,
    ) -> Self {
        Self::with_runtime_inner(
            store,
            clock,
            run_backend,
            session_catalog,
            Arc::new(NoopEnvironmentRuntime),
            Arc::new(crate::session_service::FakeSessionService),
            true,
        )
    }

    fn with_runtime_inner(
        store: Store,
        clock: Arc<dyn Clock>,
        run_backend: Arc<dyn RunBackend>,
        session_catalog: Arc<dyn SessionCatalog>,
        environment_runtime: Arc<dyn EnvironmentRuntime>,
        session_service: Arc<dyn SessionService>,
        eager_refresh: bool,
    ) -> Self {
        let mut state = Self {
            store,
            clock,
            run_backend,
            session_catalog,
            environment_runtime,
            session_service,
            file_invalidations: None,
            file_watch_warning: None,
            jobs: None,
            current_screen: Screen::Projects,
            chat_open: false,
            projects: Vec::new(),
            selected_project: None,
            source_pins: BTreeMap::new(),
            source_revs: Vec::new(),
            source_revs_project: None,
            source_revs_loading: false,
            source_revs_error: None,
            env_project: None,
            missions: Vec::new(),
            missions_project: None,
            trees: BTreeMap::new(),
            corpus_stats: None,
            mission_logs: Vec::new(),
            corpus_cost: None,
            corpus_cost_cache: corpus_core::CorpusCostCache::default(),
            finding_index_cache: FindingIndexCache::default(),
            findings: FindingDiscovery::Loading,
            findings_project: None,
            corpus_revisions: BTreeMap::new(),
            corpus_stats_project: None,
            corpus_polled_at: None,
            plugins: Vec::new(),
            plugins_loading: false,
            plugins_error: None,
            plugin_probe_target: None,
            plugin_probe_project: None,
            plugin_probe_active: None,
            plugin_probe_active_project: None,
            plugin_leases: Vec::new(),
            plugin_operation: Arc::new(Mutex::new(None)),
            opencode_models: ModelDiscovery::Loading,
            agents: Vec::new(),
            agents_project: None,
            selected_agent: None,
            selected_mission: None,
            run: None,
            run_meta: None,
            owned_run_id: None,
            run_phases: BTreeMap::new(),
            run_cancellations: BTreeMap::new(),
            run_generations: BTreeMap::new(),
            pending_mission_deletes: BTreeSet::new(),
            session_operation_leases: SessionOperationLeases::default(),
            run_lines: Vec::new(),
            run_exit: None,
            live_sessions: Vec::new(),
            live_sessions_polled_at: None,
            session_activity: BTreeMap::new(),
            session_activity_polled_at: None,
            session_activity_dirty: false,
            last_exported_at: BTreeMap::new(),
            export_retry_after: BTreeMap::new(),
            launch_requests_polled_at: None,
            launch_notices: Vec::new(),
        };
        if eager_refresh {
            state.refresh();
        }
        state
    }

    /// Re-list the projects from the store (and rebuild the sidebar tree).
    /// Newest-created first — the tree's default-open project is the most
    /// recent (the selection fallback takes `projects.first()`).
    pub fn refresh(&mut self) {
        if self.jobs.is_some() {
            let store = self.store.clone();
            self.jobs.as_mut().expect("installed above").start(
                JobKind::ProjectScope,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(30),
                move |_| {
                    let mut projects = store.list_projects().map_err(|error| error.to_string())?;
                    projects.sort_by(|a, b| b.1.created.cmp(&a.1.created));
                    let trees = projects
                        .iter()
                        .map(|(slug, _)| {
                            let agents =
                                store.list_agents(slug).map_err(|error| error.to_string())?;
                            let missions = sort_missions(
                                store
                                    .list_missions(slug)
                                    .map_err(|error| error.to_string())?,
                            );
                            Ok((slug.clone(), ProjectTree { agents, missions }))
                        })
                        .collect::<Result<BTreeMap<_, _>, String>>()?;
                    Ok(AppJobOutput::ProjectIndex(projects, trees))
                },
            );
            return;
        }
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

    /// Discover every plugin manifest but live-probe only `selected`.
    /// Host process work stays in corpus-core and on the P1 job boundary.
    pub fn refresh_plugins(&mut self, selected: Option<&str>) {
        let target = selected.map(str::to_string);
        let project = self.effective_project();
        self.plugin_probe_target = target.clone();
        self.plugin_probe_project = project.clone();
        self.plugins_loading = true;
        self.plugins_error = None;
        let Some(jobs) = self.jobs.as_mut() else {
            self.plugins = corpus_core::selected_plugin_status(target.as_deref());
            self.plugin_leases = prepared_plugin_leases(
                &self.store,
                project.as_deref(),
                target.as_deref(),
                &self.plugins,
            );
            self.plugins_loading = false;
            return;
        };
        let work_target = target.clone();
        let work_project = project.clone();
        let store = self.store.clone();
        let outcome = jobs.start(
            JobKind::PluginProbe,
            JobScope {
                project: String::new(),
                project_generation: 0,
                corpus_revision: None,
                run_id: None,
            },
            Duration::from_secs(30),
            move |_| {
                let statuses = corpus_core::selected_plugin_status(work_target.as_deref());
                Ok(AppJobOutput::Plugins {
                    leases: prepared_plugin_leases(
                        &store,
                        work_project.as_deref(),
                        work_target.as_deref(),
                        &statuses,
                    ),
                    statuses,
                    target: work_target,
                    project: work_project,
                })
            },
        );
        if matches!(outcome, StartOutcome::Started(_)) {
            self.plugin_probe_active = Some(target);
            self.plugin_probe_active_project = Some(project);
        }
    }

    /// The last plugin probe results (empty until `refresh_plugins`).
    pub fn plugins(&self) -> &[PluginStatus] {
        &self.plugins
    }

    pub fn plugin_leases(&self) -> &[PluginLeaseView] {
        &self.plugin_leases
    }

    /// Close a durable lease whose mission record is already gone. The
    /// reconciliation beat invokes this automatically; the UI calls it only
    /// as an immediate retry after automatic cleanup failed.
    pub fn cleanup_orphan_environment(
        &mut self,
        plugin_id: &str,
        session_key: &str,
    ) -> Result<bool, String> {
        let record = self
            .store
            .load_environment_session_key(plugin_id, session_key)
            .map_err(|error| error.to_string())?;
        if self
            .store
            .load_mission(&record.id.project, &record.id.mission)
            .is_ok()
        {
            return Err(
                "environment still belongs to a mission; delete that mission instead".into(),
            );
        }
        if self.jobs.is_none() {
            corpus_core::close_environment_session_key(&self.store, plugin_id, session_key)
                .map_err(|error| error.to_string())?;
            self.refresh_plugins(Some(plugin_id));
            return Ok(true);
        }

        let store = self.store.clone();
        let plugin = plugin_id.to_string();
        let key = session_key.to_string();
        let project = record.id.project;
        let scope = self.job_scope(&project, None);
        let jobs = self.jobs.as_mut().expect("checked above");
        Ok(matches!(
            jobs.start(
                JobKind::OrphanCleanup,
                scope,
                Duration::from_secs(30),
                move |_| {
                    corpus_core::close_environment_session_key(&store, &plugin, &key)
                        .map_err(|error| error.to_string())?;
                    Ok(AppJobOutput::OrphanCleanup { project, plugin })
                },
            ),
            StartOutcome::Started(_)
        ))
    }

    pub fn plugin_operation(&self) -> Option<PluginOperationView> {
        self.plugin_operation.lock().unwrap().clone()
    }

    /// Install and atomically select a local immutable v1 bundle without
    /// asking the operator to leave the app. Archive download remains a
    /// release/marketplace concern; Chunk 7 accepts an unpacked bundle path.
    pub(crate) fn start_plugin_install(&mut self, bundle: &str) -> Result<bool, String> {
        let bundle = bundle.trim();
        if bundle.is_empty() {
            return Err("choose an unpacked plugin bundle directory".into());
        }
        let Some(jobs) = self.jobs.as_mut() else {
            return Err("plugin installation requires the app background-job runtime".into());
        };
        if plugin_work_active(jobs) {
            return Ok(false);
        }
        let path = PathBuf::from(bundle);
        *self.plugin_operation.lock().unwrap() = Some(PluginOperationView {
            plugin: path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("plugin bundle")
                .to_string(),
            operation: "install".into(),
            state: PluginOperationState::Running,
            phase: Some("validating bundle".into()),
            detail: path.display().to_string(),
            recovery: None,
        });
        Ok(matches!(
            jobs.start(
                JobKind::PluginInstall,
                global_job_scope(),
                Duration::from_secs(120),
                move |_| corpus_core::install_plugin_bundle(&path)
                    .map(AppJobOutput::PluginInstalled)
                    .map_err(|error| error.to_string()),
            ),
            StartOutcome::Started(_)
        ))
    }

    /// Start an installation-scoped plugin lifecycle operation. The worker
    /// resolves the selected bundle and spawns the executable off the render
    /// thread; cancellation is observed by the protocol client every 100ms.
    pub(crate) fn start_plugin_lifecycle(
        &mut self,
        plugin_id: &str,
        operation: &'static str,
    ) -> Result<bool, String> {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => {
                return Err(format!(
                    "unsupported plugin lifecycle operation {operation:?}"
                ))
            }
        };
        let timeout = if operation == "setup" {
            Duration::from_secs(30 * 60)
        } else {
            Duration::from_secs(120)
        };
        let plugin_id = plugin_id.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            return Err("plugin lifecycle requires the app background-job runtime".into());
        };
        if plugin_work_active(jobs) {
            return Ok(false);
        }
        let operation_state = self.plugin_operation.clone();
        *operation_state.lock().unwrap() = Some(PluginOperationView {
            plugin: plugin_id.clone(),
            operation: operation.into(),
            state: PluginOperationState::Running,
            phase: Some(if operation == "setup" {
                "preparing sources".into()
            } else {
                format!("running {operation}")
            }),
            detail: String::new(),
            recovery: None,
        });
        Ok(matches!(
            jobs.start(kind, global_job_scope(), timeout, move |cancellation| {
                let selected = corpus_core::find_plugin(&plugin_id)
                    .map_err(|error| error.to_string())?
                    .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;
                let mut phases = Vec::new();
                let result = corpus_core::call_plugin_lifecycle_cancellable(
                    &selected,
                    operation,
                    timeout.saturating_sub(Duration::from_secs(1)),
                    || cancellation.is_cancelled(),
                    |progress| {
                        phases.push(progress.phase.clone());
                        if let Some(current) = operation_state.lock().unwrap().as_mut() {
                            current.phase = Some(progress.phase.clone());
                            current.detail = match (progress.completed, progress.total) {
                                (Some(completed), Some(total)) => {
                                    format!("{} · {completed}/{total}", progress.message)
                                }
                                _ => progress.message.clone(),
                            };
                        }
                    },
                )
                .map_err(|error| error.to_string())?;
                Ok(AppJobOutput::PluginLifecycle(PluginLifecycleResult {
                    plugin: plugin_id,
                    operation,
                    phases,
                    result,
                }))
            }),
            StartOutcome::Started(_)
        ))
    }

    pub(crate) fn cancel_plugin_lifecycle(&self, operation: &str) -> bool {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => return false,
        };
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.cancel_kind(kind) > 0)
    }

    pub(crate) fn plugin_lifecycle_active(&self, operation: &str) -> bool {
        let kind = match operation {
            "setup" => JobKind::PluginSetup,
            "doctor" | "status" => JobKind::PluginDoctor,
            "stop" => JobKind::PluginStop,
            _ => return false,
        };
        self.jobs
            .as_ref()
            .is_some_and(|jobs| jobs.is_kind_active(kind))
    }

    pub(crate) fn plugin_work_active(&self) -> bool {
        self.jobs.as_ref().is_some_and(plugin_work_active)
    }

    pub fn env_probe_loading(&self, project: &str) -> bool {
        self.plugins_loading
            && self.env_project.as_deref() == Some(project)
            && self
                .projects
                .iter()
                .find(|(slug, _)| slug == project)
                .is_some_and(|(_, spec)| {
                    self.plugin_probe_target.as_deref() == Some(spec.plugin.as_str())
                })
    }

    pub fn env_probe_error(&self, project: &str) -> Option<&str> {
        let targets_binding = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .is_some_and(|(_, spec)| {
                self.plugin_probe_target.as_deref() == Some(spec.plugin.as_str())
            });
        if self.env_project.as_deref() == Some(project) && targets_binding {
            self.plugins_error.as_deref()
        } else {
            None
        }
    }

    pub fn source_revisions_loading(&self, project: &str) -> bool {
        self.source_revs_loading && self.source_revs_project.as_deref() == Some(project)
    }

    /// Re-list a project's agents (and keep its tree subtree fresh).
    pub fn refresh_agents(&mut self, project: &str) {
        if self.jobs.is_some() {
            let store = self.store.clone();
            let project_owned = project.to_string();
            let scope = self.job_scope(project, None);
            self.jobs.as_mut().expect("installed above").start(
                JobKind::ProjectAgents,
                scope,
                Duration::from_secs(15),
                move |_| {
                    store
                        .list_agents(&project_owned)
                        .map(AppJobOutput::Agents)
                        .map_err(|error| error.to_string())
                },
            );
            return;
        }
        self.agents = self.store.list_agents(project).unwrap_or_default();
        self.agents_project = Some(project.to_string());
        if let Some(tree) = self.trees.get_mut(project) {
            tree.agents = self.agents.clone();
        }
    }

    /// Re-list a project's missions, newest-created first (and keep its
    /// tree subtree fresh).
    pub fn refresh_missions(&mut self, project: &str) {
        if self.jobs.is_some() {
            let store = self.store.clone();
            let project_owned = project.to_string();
            let scope = self.job_scope(project, None);
            self.jobs.as_mut().expect("installed above").start(
                JobKind::ProjectMissions,
                scope,
                Duration::from_secs(15),
                move |_| {
                    store
                        .list_missions(&project_owned)
                        .map(sort_missions)
                        .map(AppJobOutput::Missions)
                        .map_err(|error| error.to_string())
                },
            );
            return;
        }
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
        self.prepare_findings_project(project);
        if self.jobs.is_some() {
            self.schedule_corpus_refresh(project, true);
            return;
        }
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.mission_logs = corpus_core::mission_logs(&self.store, project).unwrap_or_default();
        self.corpus_cost =
            corpus_core::corpus_cost_cached(&self.store, project, &mut self.corpus_cost_cache).ok();
        self.refresh_findings_sync(project);
        self.corpus_stats_project = Some(project.to_string());
        self.corpus_polled_at = Some(self.clock.monotonic_now());
    }

    /// The recurring cadence updates cheap counts and the run listing only.
    /// Cost JSON is parsed by `refresh_corpus_stats` on selection/manual
    /// refresh and reused by `(path, mtime, len)` in between.
    fn refresh_corpus_summary(&mut self, project: &str) {
        self.prepare_findings_project(project);
        if self.jobs.is_some() {
            self.schedule_corpus_refresh(project, false);
            return;
        }
        self.corpus_stats = corpus_core::corpus_stats(&self.store, project).ok();
        self.mission_logs = corpus_core::mission_logs(&self.store, project).unwrap_or_default();
        self.refresh_findings_sync(project);
        self.corpus_stats_project = Some(project.to_string());
        self.corpus_polled_at = Some(self.clock.monotonic_now());
    }

    fn schedule_corpus_refresh(&mut self, project: &str, include_cost: bool) {
        self.prepare_findings_project(project);
        let scope = self.corpus_job_scope(project);
        // Close the cadence immediately; duplicate suppression covers manual
        // refreshes that arrive while the first walk is still running.
        self.corpus_polled_at = Some(self.clock.monotonic_now());
        let store = self.store.clone();
        let project = project.to_string();
        let mut cache = self.corpus_cost_cache.clone();
        let mut finding_cache = self.finding_index_cache.clone();
        self.jobs.as_mut().expect("installed above").start(
            if include_cost {
                JobKind::CorpusCost
            } else {
                JobKind::CorpusSummary
            },
            scope,
            Duration::from_secs(30),
            move |token| {
                let stats = corpus_core::corpus_stats(&store, &project)
                    .map_err(|error| error.to_string())?;
                let logs = corpus_core::mission_logs(&store, &project)
                    .map_err(|error| error.to_string())?;
                let cost = if include_cost {
                    Some((
                        corpus_core::corpus_cost_cached(&store, &project, &mut cache)
                            .map_err(|error| error.to_string())?,
                        cache,
                    ))
                } else {
                    None
                };
                let findings =
                    corpus_core::scan_findings_cached(&store, &project, &mut finding_cache, || {
                        token.is_cancelled()
                    })
                    .map_err(|error| error.to_string())?;
                Ok(AppJobOutput::CorpusSnapshot(CorpusSnapshot {
                    stats,
                    logs,
                    cost,
                    findings: FindingSnapshot {
                        cards: findings.cards,
                        cache: finding_cache,
                    },
                }))
            },
        );
    }

    fn prepare_findings_project(&mut self, project: &str) {
        if self.findings_project.as_deref() == Some(project) {
            return;
        }
        self.findings_project = Some(project.to_string());
        self.findings = FindingDiscovery::Loading;
        self.finding_index_cache = FindingIndexCache::default();
    }

    fn refresh_findings_sync(&mut self, project: &str) {
        match corpus_core::scan_findings_cached(
            &self.store,
            project,
            &mut self.finding_index_cache,
            || false,
        ) {
            Ok(scan) => self.findings = FindingDiscovery::Ready(scan.cards),
            Err(error) => self.fail_findings(project, &error.to_string()),
        }
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
        if self.jobs.is_some() {
            let tree = self.trees.get(slug).cloned().unwrap_or_default();
            self.agents = tree.agents;
            self.missions = tree.missions;
            self.agents_project = Some(slug.to_string());
            self.missions_project = Some(slug.to_string());
        } else {
            self.refresh_agents(slug);
            self.refresh_missions(slug);
        }
        self.refresh_corpus_stats(slug);
        self.refresh_source_revs(slug);
        self.refresh_env(slug);
    }

    /// Mission selection is navigation only. It may switch the cached project
    /// scope, but it never prepares, resumes, or spawns a run. Launch and
    /// Resume call their explicit state actions after selecting.
    pub fn select_mission(&mut self, project: &str, slug: &str) {
        if self.effective_project().as_deref() != Some(project) {
            self.select_project(project);
        }
        self.selected_mission = Some(slug.to_string());
        self.current_screen = Screen::Missions;
    }

    /// Load the source-rev dropdowns for the project's plugin (the plugin
    /// defines the revs AVAILABLE), seeding the selection from the
    /// PROJECT's stored pins (the project owns the pick) with any unset
    /// source at its default rev. When the plugin/sources can't be found
    /// the current pins are left untouched (the placeholder defaults
    /// hold) rather than cleared.
    pub fn refresh_source_revs(&mut self, project: &str) {
        let scope = self.job_scope(project, None);
        self.source_revs_project = Some(project.to_string());
        self.source_revs_loading = true;
        self.source_revs_error = None;
        let store = self.store.clone();
        let project_owned = project.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            let revs = corpus_core::plugin_sources(&store, &project_owned).unwrap_or_default();
            self.apply_source_revisions(project, revs);
            self.source_revs_loading = false;
            return;
        };
        jobs.start(
            JobKind::SourceRevisions,
            scope,
            Duration::from_secs(30),
            move |cancellation| {
                if cancellation.is_cancelled() {
                    return Err("source revision refresh cancelled".into());
                }
                corpus_core::plugin_sources(&store, &project_owned)
                    .map(AppJobOutput::SourceRevisions)
                    .map_err(|error| error.to_string())
            },
        );
    }

    /// The top-bar dropdown changed: update the in-memory selection and
    /// persist the pick onto the project (missions stamp it at creation).
    pub fn set_source_pin(&mut self, project: &str, repo: &str, rev: &str) -> Result<(), Error> {
        self.source_pins.insert(repo.to_string(), rev.to_string());
        let updated = self
            .store
            .set_project_pins(project, self.source_pins.clone())?;
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
        let (_slug, spec) = self.projects.iter().find(|(slug, _)| slug == project)?;
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
        self.env_project = Some(project.to_string());
        let plugin = self
            .projects
            .iter()
            .find(|(slug, _)| slug == project)
            .map(|(_, project)| project.plugin.clone());
        self.refresh_plugins(plugin.as_deref());
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
            self.corpus_cost = None;
            self.corpus_cost_cache = corpus_core::CorpusCostCache::default();
            self.finding_index_cache = FindingIndexCache::default();
            self.findings = FindingDiscovery::Loading;
            self.findings_project = None;
            self.corpus_revisions.clear();
            self.mission_logs.clear();
            self.agents_project = None;
            self.missions_project = None;
            self.corpus_stats_project = None;
            self.source_revs.clear();
            self.source_revs_project = None;
            self.source_revs_loading = false;
            self.source_revs_error = None;
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
            if self.jobs.is_some() {
                self.agents = self
                    .trees
                    .get(&project)
                    .map(|tree| tree.agents.clone())
                    .unwrap_or_default();
                self.agents_project = Some(project.clone());
            } else {
                self.refresh_agents(&project);
            }
        }
        if self.missions_project.as_deref() != Some(project.as_str()) {
            if self.jobs.is_some() {
                self.missions = self
                    .trees
                    .get(&project)
                    .map(|tree| tree.missions.clone())
                    .unwrap_or_default();
                self.missions_project = Some(project.clone());
            } else {
                self.refresh_missions(&project);
            }
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

    /// Findings projection for the selected project. This state never belongs
    /// to a project other than `effective_project()`.
    pub fn finding_discovery(&self) -> &FindingDiscovery {
        &self.findings
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
            if s.is_empty() {
                new_uuid_id()
            } else {
                s
            }
        };
        self.store
            .create_project(&slug, name, plugin)
            .map(|p| (slug, p))
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
                if n == 2 {
                    slug.clone()
                } else {
                    format!("{slug}-{n}")
                }
            })
            .find(|s| !self.store.project_dir(s).exists())
            .unwrap_or_else(new_uuid_id);
        self.store
            .clone_project(from, &slug, name, with_corpus)
            .map(|p| (slug, p))
    }

    pub fn delete_project(&self, slug: &str) -> Result<(), Error> {
        let missions = self.store.list_missions(slug)?;
        if self.project_has_inflight_run(slug)
            || missions
            .iter()
            .any(|(mission, _)| self.store.ensure_mission_deletable(slug, mission).is_err())
        {
            self.store.request_project_delete(slug)
        } else {
            self.store.delete_project(slug)
        }
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
    pub fn wipe_project_corpus(&mut self, slug: &str) -> Result<Project, Error> {
        let project = self.store.wipe_project_corpus(slug)?;
        if let Some((_, cached)) = self.projects.iter_mut().find(|(name, _)| name == slug) {
            *cached = project.clone();
        }
        self.note_corpus_mutation(slug);
        if self.findings_project.as_deref() == Some(slug) {
            self.finding_index_cache = FindingIndexCache::default();
            self.findings = FindingDiscovery::Ready(Vec::new());
        }
        Ok(project)
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
        self.store
            .set_agent_field(project, slug, entry, field, value)
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
        self.store
            .patch_agent_permission(project, slug, entry, patch)
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
    pub fn opencode_models(&mut self, refresh: bool) -> ModelDiscovery {
        if refresh || matches!(self.opencode_models, ModelDiscovery::Loading) {
            self.opencode_models = ModelDiscovery::Loading;
            let Some(jobs) = self.jobs.as_mut() else {
                self.opencode_models = match corpus_core::model_list(refresh) {
                    Ok(models) => ModelDiscovery::Ready(models),
                    Err(error) => ModelDiscovery::Failed(error.to_string()),
                };
                return self.opencode_models.clone();
            };
            jobs.start(
                JobKind::ModelDiscovery,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(45),
                move |_| {
                    corpus_core::model_list(refresh)
                        .map(AppJobOutput::OpencodeModels)
                        .map_err(|error| error.to_string())
                },
            );
        }
        self.opencode_models.clone()
    }

    /// Clone an agent.
    pub fn clone_agent(&self, project: &str, from: &str) -> Result<(), Error> {
        let id = new_uuid_id();
        self.store.clone_agent(project, from, &id)
    }

    /// Delete an agent.
    pub fn delete_agent(&self, project: &str, slug: &str) -> Result<(), Error> {
        let missions = self.store.missions_for_agent(project, slug)?;
        if self.run_phases.iter().any(|(id, phase)| {
            id.project == project
                && missions.iter().any(|mission| mission == &id.mission)
                && phase.blocks_deletion()
        }) || missions
            .iter()
            .any(|mission| self.store.ensure_mission_deletable(project, mission).is_err())
        {
            self.store.request_agent_delete(project, slug)
        } else {
            self.store.delete_agent(project, slug)
        }
    }

    /// Create a new (auto-id'd) agent from a ROLE — the sidebar's
    /// "+ agent" flow. Roles replaced the seed set: the role already
    /// decides the capability ceiling the renderer writes, so a seed
    /// document was only ever contributing a starting prompt, which now
    /// ships compiled into corpus-core.
    pub fn create_agent_with_role(
        &self,
        project: &str,
        role: corpus_core::AgentRole,
    ) -> Result<String, Error> {
        if Project::load(&self.store, project)?.delete_requested.is_some() {
            return Err(Error::Store("project deletion is pending".into()));
        }
        let id = new_uuid_id();
        self.store.create_agent_with_role(project, &id, role)?;
        // Stamp the human placeholder name so the Forms tab and the sidebar
        // show an editable label (and opencode a friendly handle), not the
        // opaque id. Best-effort: a naming failure must not undo a created
        // agent.
        let _ = self
            .store
            .set_agent_name(project, &id, corpus_core::DEFAULT_AGENT_NAME);
        Ok(id)
    }

    /// Create a mission record: auto-id slug, the agent ref, the current
    /// top-bar pins stamped in. Returns the mission slug.
    pub fn create_mission(&self, project: &str, agent: &str, brief: &str) -> Result<String, Error> {
        if Project::load(&self.store, project)?.delete_requested.is_some() {
            return Err(Error::Store("project deletion is pending".into()));
        }
        if self.store.load_agent(project, agent)?.meta.delete_requested.is_some() {
            return Err(Error::Store("agent deletion is pending".into()));
        }
        let id = new_uuid_id();
        let mission = Mission {
            agent: agent.to_string(),
            pins: self.source_pins.clone(),
            budget: None,
            created: self.clock.unix_seconds(),
            name: None,
            session: None,
            control: None,
            opencode_session: None,
            environment_session: None,
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        self.store.write_mission(project, &id, &mission, brief)?;
        Ok(id)
    }

    /// Delete is the mission's single teardown verb. A live run is exported
    /// and cleaned up first; its mission record is removed only after that
    /// succeeds. Durable transcripts remain in `corpus/runs/`.
    pub fn delete_mission(
        &mut self,
        project: &str,
        slug: &str,
    ) -> Result<DeleteMissionResult, Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        // Persist intent before starting teardown. If the app exits after
        // killing tmux or closing the plugin but before removing the record,
        // the next reconciliation beat resumes this request instead of
        // leaving a half-cleaned mission behind.
        if mission.delete_requested.is_none() || mission.launch_requested.is_some() {
            mission.launch_requested = None;
            mission.delete_requested.get_or_insert(MissionDeleteRequest {
                requested_at: self.clock.unix_seconds(),
            });
            self.store.update_mission(project, slug, &mission)?;
        }
        let needs_teardown =
            mission.session.is_some() || self.mission_environment_needs_cleanup(project, slug);
        if needs_teardown {
            match self.stop_mission(project, slug)? {
                StopMissionResult::Scheduled => {
                    self.pending_mission_deletes
                        .insert((project.to_string(), slug.to_string()));
                    Ok(DeleteMissionResult::Scheduled)
                }
                StopMissionResult::Completed(path) => {
                    drop(path);
                    self.store.delete_mission(project, slug)?;
                    Ok(DeleteMissionResult::Completed)
                }
            }
        } else {
            // A previous cleanup attempt may have failed in-process while an
            // external/plugin recovery subsequently closed the durable
            // environment. Once both durable handles are verified absent,
            // that failed phase is stale and must not require a UI-only
            // "Retry cleanup" command forever.
            let reconciled = self
                .run_phases
                .iter()
                .filter(|(id, _)| id.project == project && id.mission == slug)
                .max_by_key(|(id, _)| id.generation)
                .and_then(|(id, phase)| {
                    matches!(
                        phase,
                        RunPhase::Failed {
                            cleanup_pending: true,
                            ..
                        }
                    )
                    .then(|| id.clone())
                });
            if let Some(run_id) = reconciled {
                self.finish_run(&run_id);
            }
            if self.mission_run_inflight(project, slug) {
                return Err(Error::Store(
                    "mission launch or teardown is still in progress".into(),
                ));
            }
            self.store.delete_mission(project, slug)?;
            Ok(DeleteMissionResult::Completed)
        }
    }

    fn project_has_inflight_run(&self, project: &str) -> bool {
        self.run_phases
            .iter()
            .any(|(id, phase)| id.project == project && phase.blocks_deletion())
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
    fn launch(
        &mut self,
        run_id: RunId,
        project: &str,
        agent: &str,
        model: Option<&str>,
        mission: &str,
        source_pins_json: Option<&str>,
        environment_session: Option<&str>,
    ) -> Result<(), Error> {
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .entry(run_id.clone())
            .or_default()
            .clone();
        // Fail loudly on an unknown agent, then materialize the WHOLE
        // project: the agent list opencode shows is project-scoped.
        let session = match (|| {
            self.store.load_agent(project, agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.spawn(
                &run_id,
                project,
                agent,
                model,
                mission,
                source_pins_json,
                environment_session,
                &cancellation,
            )
        })() {
            Ok(session) => session,
            Err(error) => {
                self.run_cancellations.remove(&run_id);
                if cancellation.is_cancelled() {
                    self.finish_run(&run_id);
                } else {
                    self.fail_run(&run_id, RunPhaseKind::Starting, &error, true, false);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.refuse_pending_mission_delete(project, &run_id.mission) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(
                &run_id,
                session,
                environment_session,
                error,
            ));
        }
        self.run_cancellations.remove(&run_id);
        self.adopt_run(session, run_id);
        Ok(())
    }

    /// Take ownership of a freshly spawned run and reset the per-run
    /// bookkeeping (attach argv, drained lines, terminal status). Shared
    /// by `launch` and `resume_mission` so a resumed run is wired exactly
    /// like a fresh one.
    fn adopt_run(&mut self, session: Box<dyn ActiveRun>, run_id: RunId) {
        let pty_attach = session.pty_attach_command();
        self.run = Some(session);
        self.run_meta = Some(RunMeta { pty_attach });
        self.owned_run_id = Some(run_id.clone());
        self.run_phases.insert(run_id, RunPhase::Running);
        self.run_lines.clear();
    }

    fn reject_unadopted_run(
        &mut self,
        run_id: &RunId,
        mut session: Box<dyn ActiveRun>,
        environment_session: Option<&str>,
        reason: Error,
    ) -> Error {
        self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
        let stopped = session.stop();
        let transcript = stopped.transcript.display().to_string();
        let mut cleanup_errors = stopped.cleanup_errors;
        if let Some(key) = environment_session {
            match Project::load(&self.store, &run_id.project) {
                Ok(project) => {
                    if let Err(error) = corpus_core::close_environment_session_key(
                        &self.store,
                        &project.plugin,
                        key,
                    ) {
                        cleanup_errors.push(format!("environment cleanup failed: {error}"));
                    }
                }
                Err(error) => cleanup_errors.push(format!(
                    "cannot resolve environment for cleanup: {error}"
                )),
            }
        }
        let mut detail = reason.to_string();
        if let Some(error) = stopped.export_error {
            detail.push_str(&format!("; final transcript export failed: {error}"));
        }
        if cleanup_errors.is_empty() {
            let error = Error::Store(format!(
                "{detail}; spawned run was stopped; transcript: {transcript}"
            ));
            self.fail_run(run_id, RunPhaseKind::Running, &error, false, false);
            error
        } else {
            let error = Error::Store(format!(
                "{detail}; spawned run cleanup failed: {}",
                cleanup_errors.join("; ")
            ));
            self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, true);
            error
        }
    }

    fn fail_run(
        &mut self,
        run_id: &RunId,
        at: RunPhaseKind,
        error: &Error,
        recoverable: bool,
        cleanup_pending: bool,
    ) {
        self.run_phases.insert(
            run_id.clone(),
            RunPhase::Failed {
                at,
                message: error.to_string(),
                recoverable,
                cleanup_pending,
            },
        );
    }

    fn finish_run(&mut self, run_id: &RunId) {
        self.run_phases.remove(run_id);
    }

    pub fn run_phase(&self, run_id: &RunId) -> RunPhase {
        self.run_phases
            .get(run_id)
            .cloned()
            .unwrap_or(RunPhase::Idle)
    }

    pub fn latest_run_phase(&self, project: &str, mission: &str) -> RunPhase {
        self.run_phases
            .iter()
            .filter(|(id, _)| id.project == project && id.mission == mission)
            .max_by_key(|(id, _)| id.generation)
            .map(|(_, phase)| phase.clone())
            .unwrap_or(RunPhase::Idle)
    }

    /// Whether this mission already owns an operation that must finish or be
    /// cancelled before another Launch/Resume can start. This is enforced in
    /// state as well as reflected by disabled UI actions.
    pub fn mission_run_inflight(&self, project: &str, mission: &str) -> bool {
        self.latest_run_phase(project, mission).blocks_deletion()
    }

    /// User-facing Delete availability. Preparation and an active teardown
    /// are indivisible background operations, so a second Delete is held
    /// until they settle. Running and failed-cleanup states deliberately stay
    /// actionable: Delete is the command that starts or retries teardown.
    pub fn mission_delete_available(&self, project: &str, mission: &str) -> bool {
        self.latest_run_phase(project, mission)
            .allows_delete_action()
    }

    /// Whether Delete has committed to tearing this mission down in the
    /// background. Views keep the mission selected and render a stable busy
    /// pane until teardown and record removal complete.
    pub fn mission_delete_pending(&self, project: &str, mission: &str) -> bool {
        self.pending_mission_deletes
            .contains(&(project.to_string(), mission.to_string()))
            || (self.missions_project.as_deref() == Some(project)
                && self
                    .missions
                    .iter()
                    .find(|(slug, _)| slug == mission)
                    .is_some_and(|(_, record)| record.delete_requested.is_some()))
            || self
                .trees
                .get(project)
                .and_then(|tree| tree.missions.iter().find(|(slug, _)| slug == mission))
                .is_some_and(|(_, record)| record.delete_requested.is_some())
    }

    /// A mission may outlive the app between environment creation and agent
    /// spawn. Its persisted key remains the operator's cleanup handle even
    /// when no tmux session was ever created.
    pub fn mission_environment_needs_cleanup(&self, project: &str, mission: &str) -> bool {
        let Ok(mission) = self.store.load_mission(project, mission) else {
            return false;
        };
        let Some(key) = mission.environment_session.as_deref() else {
            return false;
        };
        let Ok(project) = corpus_core::Project::load(&self.store, project) else {
            return true;
        };
        self.store
            .load_environment_session_key(&project.plugin, key)
            .map(|record| record.state != corpus_core::EnvironmentSessionState::Closed)
            .unwrap_or(true)
    }

    fn refuse_pending_mission_delete(&self, project: &str, mission: &str) -> Result<(), Error> {
        load_launchable_mission(&self.store, project, mission).map(drop)
    }

    fn refuse_duplicate_mission_run(&self, project: &str, mission: &str) -> Result<(), Error> {
        // This is an early concurrency guard, before a run generation exists.
        // Preserve the launch path's established error accounting for missing
        // or unreadable missions: `prepare_launch` will record those failures.
        // A readable durable delete marker, however, can be refused without
        // allocating a generation or starting any preparation.
        if self.store.load_mission(project, mission).is_ok() {
            self.refuse_pending_mission_delete(project, mission)?;
        }
        if self.mission_run_inflight(project, mission) {
            Err(Error::Store(
                "this mission already has a run operation in progress".into(),
            ))
        } else if self.mission_environment_needs_cleanup(project, mission) {
            Err(Error::Store(
                "this mission has an environment requiring cleanup; stop it before launching"
                    .into(),
            ))
        } else {
            Ok(())
        }
    }

    fn next_run_id(&mut self, project: &str, mission: &str) -> RunId {
        let generation = self
            .run_generations
            .entry((project.to_string(), mission.to_string()))
            .or_default();
        *generation += 1;
        RunId {
            project: project.to_string(),
            mission: mission.to_string(),
            generation: *generation,
        }
    }

    pub fn run_belongs_to(&self, project: &str, mission: &str) -> bool {
        self.owned_run_id
            .as_ref()
            .is_some_and(|id| id.project == project && id.mission == mission)
    }

    /// Drain any new transcript lines; mark the run finished the moment it
    /// exits. Called from the app-level update path (so an exit is noticed on
    /// any screen); live producer deadlines own the next poll.
    pub fn poll_run(&mut self) {
        let Some(mut session) = self.run.take() else {
            return;
        };
        while let Some(line) = session.poll_line() {
            // egui_term consumes the PTY directly. Keeping the parallel raw
            // capture here only duplicated an unbounded stream in memory.
            if self
                .run_meta
                .as_ref()
                .is_some_and(|meta| meta.pty_attach.is_none())
            {
                self.run_lines.push(RunLine {
                    stderr: line.stderr,
                    text: strip_ansi(&line.text),
                });
            }
        }
        if self.run_lines.len() > MAX_RUN_LINES {
            self.run_lines.drain(..self.run_lines.len() - MAX_RUN_LINES);
        }
        if let Some(code) = session.try_exit_code() {
            // Queue the report NOW: ownership still names the mission, and
            // once the handle is gone the pane going quiet is otherwise the
            // only evidence this run existed.
            self.run_exit = Some(RunExit {
                mission: self
                    .owned_run_id
                    .as_ref()
                    .map(|id| self.mission_display_label(&id.project, &id.mission)),
                code,
            });
            if let Some(run_id) = self.owned_run_id.take() {
                let completion = if code == 0 {
                    corpus_core::MissionCompletion::Completed {
                        at: self.clock.unix_seconds(),
                    }
                } else {
                    corpus_core::MissionCompletion::UnexpectedExit {
                        at: self.clock.unix_seconds(),
                    }
                };
                let _ = self.record_dispatch_completion(
                    &run_id.project,
                    &run_id.mission,
                    completion,
                );
                self.finish_run(&run_id);
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

    /// An agent's operator-facing label from the selected project's cache:
    /// its name, else its human slug, else `unnamed agent` — never a raw
    /// uuid. Mirrors [`Self::mission_label`].
    pub fn agent_label(&self, slug: &str) -> String {
        let name = self
            .agents
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, a)| a.meta.name.clone())
            .unwrap_or_default();
        agent_label(&name, slug)
    }

    /// Operator-initiated stop: attempt transcript-of-record export, then
    /// always attempt cleanup. Returns the durable transcript path (the
    /// exported JSON when it lands, else the raw/.log fallback) — the
    /// caller is what reports it, so nothing is stored here.
    fn stop_run(&mut self) -> Option<StopAttempt> {
        let mut session = self.run.take()?;
        let run_id = self.owned_run_id.take();
        if let Some(id) = &run_id {
            self.run_phases.insert(id.clone(), RunPhase::Stopping);
        }
        let outcome = session.stop();
        let cleanup_complete = outcome.cleanup_errors.is_empty();
        let mut errors = Vec::new();
        if let Some(error) = outcome.export_error {
            errors.push(error);
        }
        errors.extend(outcome.cleanup_errors);
        let error = if errors.is_empty() {
            None
        } else {
            Some(Error::Store(format!(
                "stop completed with errors (transcript: {}): {}",
                outcome.transcript.display(),
                errors.join("; ")
            )))
        };
        if let Some(id) = &run_id {
            if cleanup_complete {
                self.finish_run(id);
                if let Some(error) = &error {
                    self.fail_run(id, RunPhaseKind::Exporting, error, true, false);
                }
            } else {
                self.run = Some(session);
                self.owned_run_id = Some(id.clone());
                self.fail_run(
                    id,
                    RunPhaseKind::Stopping,
                    error.as_ref().unwrap(),
                    true,
                    true,
                );
            }
        }
        Some(StopAttempt {
            transcript: outcome.transcript,
            error,
            cleanup_complete,
        })
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
        argv.windows(2).find(|w| w[0] == "-t").map(|w| w[1].clone())
    }

    /// Re-list the live corpus tmux sessions (the re-attach list shown
    /// when the app was relaunched over a surviving run).
    pub fn refresh_live_sessions(&mut self) {
        if self.jobs.is_some() {
            self.live_sessions_polled_at = Some(self.clock.monotonic_now());
            let catalog = self.session_catalog.clone();
            self.jobs.as_mut().expect("installed above").start(
                JobKind::SessionDiscovery,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(10),
                move |_| Ok(AppJobOutput::LiveSessions(catalog.live_tui_sessions())),
            );
            return;
        }
        self.live_sessions = self.session_catalog.live_tui_sessions();
        self.live_sessions_polled_at = Some(self.clock.monotonic_now());
    }

    fn schedule_session_maintenance(&mut self, project: &str) {
        let missions = self
            .trees
            .get(project)
            .map(|tree| tree.missions.clone())
            .unwrap_or_default();
        let live = self.live_sessions.clone();
        let pending_conversations = missions
            .iter()
            .filter(|(_, mission)| mission.delete_requested.is_none())
            .filter(|(_, mission)| mission.opencode_session.is_none())
            .filter_map(|(slug, mission)| Some((slug.clone(), mission.session.clone()?)))
            .filter(|(_, tmux)| live.iter().any(|session| session == tmux))
            .collect::<Vec<_>>();
        let now = self.clock.monotonic_now();
        let pending_exports = missions
            .iter()
            .filter_map(|(slug, mission)| {
                Some((
                    slug.clone(),
                    mission.delete_requested.is_some(),
                    mission.opencode_session.clone()?,
                    mission.session.clone()?,
                ))
            })
            .filter(|(_, _, _, tmux)| live.iter().any(|session| session == tmux))
            .filter(|(slug, deleting, _, tmux)| {
                checkpoint_export_due(
                    *deleting,
                    self.mission_activity(project, slug),
                    self.session_activity.get(tmux).copied(),
                    self.last_exported_at.get(tmux).copied(),
                    self.export_retry_after.get(tmux).copied(),
                    now,
                )
            })
            // Checkpoints are serialized deliberately. A second quiet
            // conversation waits for the next maintenance beat instead of
            // turning one job into an unbounded batch of CLI subprocesses.
            .take(1)
            .map(|(slug, _, conversation, tmux)| {
                let lease = self.session_operation_leases.claim(project, &slug);
                (slug, conversation, tmux, lease)
            })
            .collect::<Vec<_>>();
        if pending_conversations.is_empty() && pending_exports.is_empty() {
            return;
        }
        let scope = self.job_scope(project, None);
        let store = self.store.clone();
        let service = self.session_service.clone();
        let backend = self.run_backend.clone();
        let project_owned = project.to_string();
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        jobs.start(
            JobKind::SessionExport,
            scope,
            Duration::from_secs(30),
            move |_| {
                let mut conversations = Vec::new();
                let mut claimed = missions
                    .iter()
                    .filter_map(|(_, mission)| mission.opencode_session.clone())
                    .collect::<BTreeSet<_>>();
                for (slug, tmux) in pending_conversations {
                    let Some(launched_at_ms) = launch_stamp_ms(&tmux) else {
                        continue;
                    };
                    if let Ok(conversation) = service.find_for_launch(
                        &store.project_run_dir(&project_owned),
                        launched_at_ms,
                        &claimed,
                    ) {
                        claimed.insert(conversation.clone());
                        conversations.push((slug, tmux, conversation));
                    }
                }
                let mut exported_tmux = Vec::new();
                let mut export_failure = None;
                for (slug, conversation, tmux, lease) in pending_exports {
                    let _ownership = lease
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    // Delete may have committed while this checkpoint waited
                    // for an earlier owner. Recheck the durable record under
                    // the lease so stale work cannot run after teardown.
                    let still_current = load_launchable_mission(&store, &project_owned, &slug)
                        .is_ok_and(|mission| {
                            mission.session.as_deref() == Some(tmux.as_str())
                                && mission.opencode_session.as_deref()
                                    == Some(conversation.as_str())
                        });
                    if !still_current {
                        continue;
                    }
                    match backend.export_session(&project_owned, &conversation) {
                        Ok(_) => exported_tmux.push(tmux),
                        Err(error) => export_failure = Some((tmux, error.to_string())),
                    }
                }
                Ok(AppJobOutput::SessionMaintenance(SessionMaintenance {
                    conversations,
                    exported_tmux,
                    export_failure,
                    warning: service.take_warning(),
                }))
            },
        );
    }

    /// Deliver terminal child results through each exact parent TUI. The
    /// worker owns all HTTP and store I/O; the render loop only schedules a
    /// coalesced global pass after liveness/conversation reconciliation.
    fn schedule_dispatch_deliveries(&mut self) {
        let Some(jobs) = self.jobs.as_mut() else {
            return;
        };
        let store = self.store.clone();
        let service = self.session_service.clone();
        let live = self.live_sessions.clone();
        jobs.start(
            JobKind::DispatchDelivery,
            JobScope {
                project: String::new(),
                project_generation: 0,
                corpus_revision: None,
                run_id: None,
            },
            Duration::from_secs(10),
            move |_| {
                reconcile_dispatch_activity(&store, service.as_ref(), &live)?;
                deliver_completed_dispatches(&store, service.as_ref(), &live)?;
                Ok(AppJobOutput::DispatchDeliveries)
            },
        );
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
        let now = self.clock.monotonic_now();
        let due = self
            .live_sessions_polled_at
            .is_none_or(|t| now.saturating_duration_since(t) >= Duration::from_secs(2));
        if due {
            self.refresh_live_sessions();
            if self.jobs.is_none() {
                self.reconcile_mission_dispatches();
                // Synchronous test/headless fallback retains the legacy
                // maintenance path. Production schedules listing first;
                // maintenance moves to its own scoped jobs below.
                if let Some(project) = self.effective_project() {
                    self.sweep_conversations(&project);
                    self.sweep_usage_exports(&project);
                }
            }
        }
        // Activity is a `stat` per live session — no subprocess, so it
        // polls faster: the dot should catch a turn starting, not lag it
        // by the tmux listing's throttle.
        let now = self.clock.monotonic_now();
        let activity_due = self.session_activity_polled_at.is_none_or(|t| {
            let elapsed = now.saturating_duration_since(t);
            elapsed >= ACTIVITY_BACKSTOP
                || self.session_activity_dirty && elapsed >= ACTIVITY_EVENT_MIN
        });
        if activity_due {
            self.refresh_session_activity();
            // Same beat: catch the live run's opencode session id as soon
            // as the TUI has created it (self-throttled, and a no-op once
            // the mission record has one).
            if self.jobs.is_none() {
                self.capture_opencode_session();
            }
        }
    }

    /// Honor any launch the CURATOR requested (its `mission_launch` tool
    /// set `launch_requested` on a mission record from the MCP process —
    /// run spawning is the app's alone). Filesystem notifications make the
    /// scan due promptly; the slower timer reconciles missed events. The scan
    /// reads every project's mission records because the flag was written by
    /// another process and the cached tree does not have it.
    ///
    /// The request is cleared BEFORE the spawn, so a launch that fails
    /// reports once instead of retrying every beat. A mission whose
    /// requested session is already live just clears — the curator asked
    /// for a run and there is one. Scans EVERY project, not just the
    /// selected one: the curator is scoped to its own project, which the
    /// operator need not be viewing when the launch fires.
    pub fn poll_launch_requests(&mut self) {
        let now = self.clock.monotonic_now();
        let due = self
            .launch_requests_polled_at
            .is_none_or(|t| now.saturating_duration_since(t) >= STORE_BACKSTOP);
        if !due {
            return;
        }
        self.launch_requests_polled_at = Some(now);
        self.reconcile_orphan_environments();

        // Gather flagged missions off disk first (the authoritative record —
        // the flag came from the MCP process).
        let projects = self
            .store
            .list_projects()
            .unwrap_or_else(|_| self.projects.clone());
        if self.jobs.is_some() {
            let store = self.store.clone();
            let catalog = self.session_catalog.clone();
            self.jobs.as_mut().expect("installed above").start(
                JobKind::LaunchRequests,
                JobScope {
                    project: String::new(),
                    project_generation: 0,
                    corpus_revision: None,
                    run_id: None,
                },
                Duration::from_secs(15),
                move |_| {
                    let live = catalog.live_tui_sessions();
                    let mut launches = Vec::new();
                    let mut deletions = Vec::new();
                    let mut agent_deletions = Vec::new();
                    let mut project_deletions = Vec::new();
                    for (project, project_record) in projects {
                        let project_deleting = project_record.delete_requested.is_some();
                        if project_deleting {
                            project_deletions.push(project.clone());
                        }
                        let deleting_agents: BTreeSet<String> = store
                            .list_agents(&project)
                            .unwrap_or_default()
                            .into_iter()
                            .filter_map(|(agent, config)| {
                                config.meta.delete_requested.is_some().then_some(agent)
                            })
                            .collect();
                        agent_deletions.extend(deleting_agents.iter().map(|agent| {
                            AgentDeletionRequest {
                                project: project.clone(),
                                agent: agent.clone(),
                            }
                        }));
                        let Ok(missions) = store.list_missions(&project) else {
                            continue;
                        };
                        for (slug, mission) in missions {
                            if mission.delete_requested.is_some() {
                                deletions.push(DeletionRequest {
                                    project: project.clone(),
                                    slug,
                                });
                                continue;
                            }
                            if project_deleting || deleting_agents.contains(&mission.agent) {
                                continue;
                            }
                            if mission.launch_requested.is_none() {
                                continue;
                            }
                            let already_live = mission
                                .session
                                .as_deref()
                                .is_some_and(|session| live.iter().any(|item| item == session));
                            launches.push(LaunchRequest {
                                project: project.clone(),
                                label: mission_label(mission.name.as_deref(), &slug),
                                slug,
                                already_live,
                            });
                        }
                    }
                    Ok(AppJobOutput::LaunchRequests {
                        launches,
                        deletions,
                        agent_deletions,
                        project_deletions,
                    })
                },
            );
            return;
        }
        let mut pending: Vec<(String, String, Option<String>)> = Vec::new();
        let mut deletions = Vec::new();
        let mut agent_deletions = Vec::new();
        let mut project_deletions = Vec::new();
        for (project, project_record) in &projects {
            let project_deleting = project_record.delete_requested.is_some();
            if project_deleting {
                project_deletions.push(project.clone());
            }
            let deleting_agents: BTreeSet<String> = self
                .store
                .list_agents(project)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(agent, config)| {
                    config.meta.delete_requested.is_some().then_some(agent)
                })
                .collect();
            agent_deletions.extend(deleting_agents.iter().map(|agent| AgentDeletionRequest {
                project: project.clone(),
                agent: agent.clone(),
            }));
            let Ok(missions) = self.store.list_missions(project) else {
                continue;
            };
            for (slug, m) in missions {
                if m.delete_requested.is_some() {
                    deletions.push(DeletionRequest {
                        project: project.clone(),
                        slug,
                    });
                    continue;
                }
                if project_deleting || deleting_agents.contains(&m.agent) {
                    continue;
                }
                if m.launch_requested.is_some() {
                    pending.push((project.clone(), slug, m.session.clone()));
                }
            }
        }
        if pending.is_empty()
            && deletions.is_empty()
            && agent_deletions.is_empty()
            && project_deletions.is_empty()
        {
            return;
        }
        self.apply_deletion_requests(deletions);
        self.apply_parent_deletion_requests(agent_deletions, project_deletions);
        // A fresh listing so "already live" is a real answer, not a stale
        // one that would spawn a duplicate.
        self.refresh_live_sessions();
        for (project, slug, session) in pending {
            let already_live = session
                .as_deref()
                .is_some_and(|s| self.live_sessions.iter().any(|l| l == s));
            // Clear FIRST: a spawn failure must not loop the request.
            if let Err(error) = self.clear_launch_request(&project, &slug, !already_live) {
                self.launch_notices.push(LaunchNotice {
                    mission: slug.clone(),
                    result: Err(error.to_string()),
                });
                continue;
            }
            if already_live {
                continue;
            }
            let label = self.mission_display_label(&project, &slug);
            let result = self.launch_mission_detached(&project, &slug).map_err(|error| {
                let message = error.to_string();
                self.record_dispatch_launch_failure(&project, &slug, &message);
                message
            });
            self.launch_notices.push(LaunchNotice {
                mission: label,
                result,
            });
        }
    }

    fn apply_mission_requests(
        &mut self,
        deletions: Vec<DeletionRequest>,
        agent_deletions: Vec<AgentDeletionRequest>,
        project_deletions: Vec<String>,
        launches: Vec<LaunchRequest>,
    ) {
        self.apply_deletion_requests(deletions);
        self.apply_parent_deletion_requests(agent_deletions, project_deletions);
        for request in launches {
            if let Err(error) = self.clear_launch_request(
                &request.project,
                &request.slug,
                !request.already_live,
            ) {
                self.launch_notices.push(LaunchNotice {
                    mission: request.label,
                    result: Err(error.to_string()),
                });
                continue;
            }
            if request.already_live {
                continue;
            }
            if let Err(error) = self.launch_mission_detached(&request.project, &request.slug) {
                self.record_dispatch_launch_failure(
                    &request.project,
                    &request.slug,
                    &error.to_string(),
                );
                self.launch_notices.push(LaunchNotice {
                    mission: request.label,
                    result: Err(error.to_string()),
                });
            }
        }
    }

    fn apply_deletion_requests(&mut self, requests: Vec<DeletionRequest>) {
        let mut refresh = BTreeSet::new();
        for request in requests {
            match self.delete_mission(&request.project, &request.slug) {
                Ok(DeleteMissionResult::Completed) => {
                    refresh.insert(request.project);
                }
                Ok(DeleteMissionResult::Scheduled) | Err(_) => {}
            }
        }
        for project in refresh {
            self.refresh_missions(&project);
        }
    }

    fn reconcile_orphan_environments(&mut self) {
        for (plugin, key) in orphan_environment_sessions(&self.store) {
            let _ = self.cleanup_orphan_environment(&plugin, &key);
        }
    }

    fn apply_parent_deletion_requests(
        &mut self,
        agent_requests: Vec<AgentDeletionRequest>,
        project_requests: Vec<String>,
    ) {
        let project_set: BTreeSet<String> = project_requests.iter().cloned().collect();
        for request in agent_requests {
            if project_set.contains(&request.project) {
                continue;
            }
            let missions = self
                .store
                .missions_for_agent(&request.project, &request.agent)
                .unwrap_or_default();
            for mission in missions {
                let _ = self.delete_mission(&request.project, &mission);
            }
            if self
                .store
                .missions_for_agent(&request.project, &request.agent)
                .is_ok_and(|missions| missions.is_empty())
            {
                let _ = self.store.delete_agent(&request.project, &request.agent);
                self.refresh_agents(&request.project);
            }
        }
        for project in project_requests {
            let missions = self.store.list_missions(&project).unwrap_or_default();
            for (mission, _) in missions {
                let _ = self.delete_mission(&project, &mission);
            }
            if self
                .store
                .list_missions(&project)
                .is_ok_and(|missions| missions.is_empty())
            {
                let _ = self.store.delete_project(&project);
                self.refresh();
            }
        }
    }

    /// Drain the curator-launch reports queued since the last call — the
    /// app loop turns each into a toast.
    pub fn take_launch_notices(&mut self) -> Vec<LaunchNotice> {
        std::mem::take(&mut self.launch_notices)
    }

    /// Consume a mission's launch request before spawning. The proven parent
    /// origin is copied onto the child record in the same write, so a spawn
    /// failure or app restart cannot lose the return path.
    fn clear_launch_request(
        &mut self,
        project: &str,
        slug: &str,
        bind_origin: bool,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let request = mission.launch_requested.take();
        if bind_origin {
            mission.dispatch = request
                .and_then(|request| request.requested_by)
                .map(|parent| corpus_core::MissionDispatch {
                    parent,
                    child_run_id: None,
                    live_seen: false,
                    running_seen: false,
                    completion: None,
                    delivery_attempt: 0,
                    delivery_message_id: None,
                    delivered: false,
                });
        }
        self.store.update_mission(project, slug, &mission)
    }

    fn record_dispatch_launch_failure(&mut self, project: &str, slug: &str, error: &str) {
        let bounded: String = error.chars().take(2_000).collect();
        let _ = self.record_dispatch_completion(
            project,
            slug,
            corpus_core::MissionCompletion::LaunchFailed {
                at: self.clock.unix_seconds(),
                error: bounded,
            },
        );
    }

    /// Persist one terminal child result. Once present it is immutable, so
    /// repeated liveness scans and app restarts cannot create duplicates.
    fn record_dispatch_completion(
        &mut self,
        project: &str,
        slug: &str,
        completion: corpus_core::MissionCompletion,
    ) -> Result<bool, Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let Some(dispatch) = mission.dispatch.as_mut() else {
            return Ok(false);
        };
        if dispatch.completion.is_some() {
            return Ok(false);
        }
        dispatch.completion = Some(completion);
        self.store.update_mission(project, slug, &mission)?;
        Ok(true)
    }

    /// Fold the existing tmux/raw-capture state into durable dispatch facts.
    /// This starts no inference and performs no delivery. An initial parked
    /// session is only remembered as live; completion requires that the exact
    /// child run was first observed producing output.
    fn reconcile_mission_dispatches(&mut self) {
        let Ok(projects) = self.store.list_projects() else {
            return;
        };
        let now = self.clock.unix_seconds();
        for (project, _) in projects {
            let Ok(missions) = self.store.list_missions(&project) else {
                continue;
            };
            for (slug, mut mission) in missions {
                let Some(snapshot) = mission.dispatch.as_ref() else {
                    continue;
                };
                if snapshot.completion.is_some() {
                    continue;
                }
                let Some(child_run_id) = snapshot.child_run_id.as_deref() else {
                    continue;
                };
                // Piped children have no tmux binding; `poll_run` owns their
                // exact process exit and records completion there.
                if mission.session.is_none() {
                    continue;
                }
                // Only the exact bound child may advance this dispatch. A
                // later manual relaunch cannot complete an older request.
                let exact_binding = mission.session.as_deref() == Some(child_run_id);
                let live = exact_binding
                    && self
                        .live_sessions
                        .iter()
                        .any(|session| session == child_run_id);
                let dispatch = mission.dispatch.as_mut().expect("checked above");
                let mut changed = false;
                if live && !dispatch.live_seen {
                    dispatch.live_seen = true;
                    changed = true;
                }
                // Terminal-paint recency is only a display signal. A quiet
                // provider or tool may produce no PTY bytes for many seconds,
                // so successful completion is recorded off-thread from the
                // exact OpenCode process's active-session endpoint instead.
                if !live && dispatch.live_seen {
                    dispatch.completion =
                        Some(corpus_core::MissionCompletion::UnexpectedExit { at: now });
                    changed = true;
                }
                if changed {
                    let _ = self.store.update_mission(&project, &slug, &mission);
                }
            }
        }
    }

    /// A mission's operator-facing label from its DISK record (name, else
    /// its human slug, else `new`) — the cache covers only the selected
    /// project, and a curator launch can name any.
    fn mission_display_label(&self, project: &str, slug: &str) -> String {
        let name = self
            .store
            .load_mission(project, slug)
            .ok()
            .and_then(|m| m.name);
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
        let now = self.clock.monotonic_now();
        let due = self
            .corpus_polled_at
            .is_none_or(|t| now.saturating_duration_since(t) >= STORE_BACKSTOP);
        if due {
            if self.jobs.is_some() {
                self.corpus_polled_at = Some(now);
                self.prepare_findings_project(&project);
                let scope = self.corpus_job_scope(&project);
                let store = self.store.clone();
                let project_owned = project.clone();
                let mut finding_cache = self.finding_index_cache.clone();
                self.jobs.as_mut().expect("installed above").start(
                    JobKind::ProjectScope,
                    scope,
                    Duration::from_secs(30),
                    move |token| {
                        let agents = store
                            .list_agents(&project_owned)
                            .map_err(|error| error.to_string())?;
                        let missions = sort_missions(
                            store
                                .list_missions(&project_owned)
                                .map_err(|error| error.to_string())?,
                        );
                        let stats = corpus_core::corpus_stats(&store, &project_owned)
                            .map_err(|error| error.to_string())?;
                        let logs = corpus_core::mission_logs(&store, &project_owned)
                            .map_err(|error| error.to_string())?;
                        let findings = corpus_core::scan_findings_cached(
                            &store,
                            &project_owned,
                            &mut finding_cache,
                            || token.is_cancelled(),
                        )
                        .map_err(|error| error.to_string())?;
                        Ok(AppJobOutput::ProjectScope(ProjectScopeSnapshot {
                            agents,
                            missions,
                            stats,
                            logs,
                            findings: FindingSnapshot {
                                cards: findings.cards,
                                cache: finding_cache,
                            },
                        }))
                    },
                );
                return;
            }
            self.refresh_agents(&project);
            self.refresh_missions(&project);
            // Stamps `corpus_polled_at`, closing the throttle for all three.
            self.refresh_corpus_summary(&project);
        }
    }

    /// Re-stat the raw capture of every mission session we know of, and
    /// record WHEN it last grew as an `Instant`. Storing the instant (not
    /// the age) means the reading keeps aging correctly between polls, so
    /// a 500 ms poll still gives a dot that goes still the moment output
    /// stops.
    fn refresh_session_activity(&mut self) {
        let now = self.clock.monotonic_now();
        self.session_activity_polled_at = Some(now);
        self.session_activity_dirty = false;
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
            let Some(log) = self
                .session_catalog
                .raw_log(&self.store, &project, &session)
            else {
                continue;
            };
            let Some(idle) = corpus_core::run_idle_secs(&log) else {
                continue;
            };
            let last_paint = now.checked_sub(Duration::from_secs(idle)).unwrap_or(now);
            self.session_activity.insert(session, last_paint);
        }
    }

    /// What the mission's status dot should say: `Idle` (nothing up),
    /// `Waiting` (session live, agent quiet), or `Working` (producing
    /// right now).
    ///
    /// A run is UP when the app-owned run is live and belongs to this
    /// mission (an owned handle in `Running`), or when the
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
        let owned = self.run_active() && self.run_belongs_to(project, slug);
        let session = self
            .trees
            .get(project)
            .and_then(|tree| tree.missions.iter().find(|(s, _)| s == slug))
            .and_then(|(_, mission)| mission.session.clone());
        let Some(session) = session else {
            // No tmux session: the only thing that can be up is an
            // app-owned piped run, which is busy for its whole life.
            return if owned {
                MissionActivity::Working
            } else {
                MissionActivity::Idle
            };
        };
        let live = self.live_sessions.iter().any(|l| l == &session)
            || self.live_run_session().as_deref() == Some(session.as_str())
            || owned;
        activity_for(
            self.clock.monotonic_now(),
            live,
            self.session_activity.get(&session).copied(),
        )
    }

    /// The only app-owned repaint clock. Background jobs, terminal output
    /// and chat events wake egui at delivery; this deadline exists solely
    /// while a run/session needs liveness polling or an activity dot needs
    /// animation. With no live producer it returns `None`, so an idle window
    /// schedules zero frames.
    pub fn live_repaint_after(&self) -> Option<Duration> {
        let busiest = self
            .trees
            .iter()
            .flat_map(|(project, tree)| {
                tree.missions
                    .iter()
                    .map(move |(slug, _)| self.mission_activity(project, slug))
            })
            .max_by_key(|activity| match activity {
                MissionActivity::Working => 2,
                MissionActivity::Waiting => 1,
                MissionActivity::Idle => 0,
            })
            .unwrap_or(MissionActivity::Idle);

        match busiest {
            // PTY/file/job producers wake egui when new data arrives. The
            // clock is only a bounded liveness fallback for an app-owned
            // process, not an animation timer.
            MissionActivity::Working if self.run_active() => Some(Duration::from_millis(250)),
            MissionActivity::Working => Some(Duration::from_secs(2)),
            MissionActivity::Waiting => Some(Duration::from_secs(2)),
            MissionActivity::Idle if self.run_active() => Some(Duration::from_millis(250)),
            // A just-discovered session may precede the mission cache that
            // names it. Keep the slow ownership beat until the cache lands.
            MissionActivity::Idle if !self.live_sessions.is_empty() => Some(Duration::from_secs(2)),
            MissionActivity::Idle => None,
        }
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
    pub fn launch_mission(&mut self, project: &str, slug: &str) -> Result<(), Error> {
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::AdoptFresh);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        self.refuse_pending_mission_delete(project, slug)?;
        if let Err(error) = self.background_active_run() {
            self.run_cancellations.remove(&run_id);
            self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
            return Err(error);
        }
        let model = self.agent_default_model(project, &record.agent);
        self.launch(
            run_id.clone(),
            project,
            &record.agent,
            model.as_deref(),
            &prompt,
            pins_json.as_deref(),
            record.environment_session.as_deref(),
        )?;
        let session = self.live_run_session();
        let control_port = self.run.as_ref().and_then(|run| run.control_port());
        let child_run_id = self.run.as_ref().and_then(|run| run.launch_identity());
        if let Err(error) =
            self.bind_fresh_run(project, slug, session, child_run_id, control_port)
        {
            return Err(self.cleanup_failed_adoption(&run_id, error));
        }
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
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::DetachedFresh);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let prompt = self.mission_kickoff_prompt(project, slug);
        let model = self.agent_default_model(project, &record.agent);
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .get(&run_id)
            .cloned()
            .unwrap_or_default();
        // Same materialization as an adopted launch: the run's agent set is
        // this project's, rendered fresh.
        let mut session = match (|| {
            self.refuse_pending_mission_delete(project, slug)?;
            self.store.load_agent(project, &record.agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.spawn(
                &run_id,
                project,
                &record.agent,
                model.as_deref(),
                &prompt,
                pins_json.as_deref(),
                record.environment_session.as_deref(),
                &cancellation,
            )
        })() {
            Ok(session) => session,
            Err(error) => {
                self.run_cancellations.remove(&run_id);
                if cancellation.is_cancelled() {
                    self.finish_run(&run_id);
                } else {
                    self.fail_run(&run_id, RunPhaseKind::Starting, &error, true, false);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.refuse_pending_mission_delete(project, slug) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(
                &run_id,
                session,
                record.environment_session.as_deref(),
                error,
            ));
        }
        self.run_cancellations.remove(&run_id);
        let control_port = session.control_port();
        let child_run_id = session.launch_identity();
        let tmux = session
            .pty_attach_command()
            .and_then(|argv| AppState::pty_attach_session(&argv));
        match tmux {
            Some(name) => {
                // A detached TUI: record the session and let go. Discovery
                // takes it from here.
                if let Err(error) =
                    self.bind_fresh_run(
                        project,
                        slug,
                        Some(name),
                        child_run_id.clone(),
                        control_port,
                    )
                {
                    self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
                    let transcript = session.stop();
                    let combined = Error::Store(format!(
                        "{error}; spawned run stop transcript: {}; cleanup errors: {}",
                        transcript.transcript.display(),
                        transcript.cleanup_errors.join("; ")
                    ));
                    self.fail_run(&run_id, RunPhaseKind::Running, &combined, false, false);
                    return Err(combined);
                }
                drop(session);
                self.finish_run(&run_id);
            }
            None => {
                // Piped fallback: the child lives on the handle, so it
                // must be adopted or it leaks. This does take the pane —
                // there is no detached session to hand off.
                if let Err(error) = self.background_active_run() {
                    let cleanup = session.stop();
                    let combined = Error::Store(format!(
                        "could not background the current run: {error}; new run cleanup transcript: {}; cleanup errors: {}",
                        cleanup.transcript.display(),
                        cleanup.cleanup_errors.join("; ")
                    ));
                    self.fail_run(&run_id, RunPhaseKind::Starting, &combined, true, false);
                    return Err(combined);
                }
                self.adopt_run(session, run_id.clone());
                if let Err(error) =
                    self.bind_fresh_run(project, slug, None, child_run_id, control_port)
                {
                    return Err(self.cleanup_failed_adoption(&run_id, error));
                }
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
        self.refuse_duplicate_mission_run(project, slug)?;
        if self.jobs.is_some() {
            return self.schedule_launch(project, slug, LaunchMode::Resume);
        }
        let run_id = self.next_run_id(project, slug);
        let (record, pins_json) = self.prepare_launch(&run_id)?;
        let id = match record.opencode_session.clone() {
            Some(id) => id,
            None => {
                let error = Error::Store(
                    "no opencode session recorded for this mission — nothing to resume".into(),
                );
                self.run_cancellations.remove(&run_id);
                self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
                return Err(error);
            }
        };
        self.refuse_pending_mission_delete(project, slug)?;
        if let Err(error) = self.background_active_run() {
            self.run_cancellations.remove(&run_id);
            self.fail_run(&run_id, RunPhaseKind::Preparing, &error, true, false);
            return Err(error);
        }
        // Same materialization as a launch: the resumed conversation runs
        // against this project's agent set.
        let model = self.agent_default_model(project, &record.agent);
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        let cancellation = self
            .run_cancellations
            .get(&run_id)
            .cloned()
            .unwrap_or_default();
        let run = match (|| {
            self.refuse_pending_mission_delete(project, slug)?;
            self.store.load_agent(project, &record.agent)?;
            self.store.render_project_agents(project)?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.run_backend.resume(
                &run_id,
                project,
                &record.agent,
                model.as_deref(),
                &id,
                pins_json.as_deref(),
                record.environment_session.as_deref(),
                &cancellation,
            )
        })() {
            Ok(run) => run,
            Err(error) => {
                self.run_cancellations.remove(&run_id);
                if cancellation.is_cancelled() {
                    self.finish_run(&run_id);
                } else {
                    self.fail_run(&run_id, RunPhaseKind::Starting, &error, true, false);
                }
                return Err(error);
            }
        };
        if let Err(error) = self.refuse_pending_mission_delete(project, slug) {
            self.run_cancellations.remove(&run_id);
            return Err(self.reject_unadopted_run(
                &run_id,
                run,
                record.environment_session.as_deref(),
                error,
            ));
        }
        self.run_cancellations.remove(&run_id);
        let resumed_run_id = run.launch_identity();
        let control_port = run.control_port();
        self.adopt_run(run, run_id.clone());
        if let Some(session) = self.live_run_session() {
            if let Err(error) = self.bind_resumed_run(
                project,
                slug,
                Some(session),
                resumed_run_id,
                control_port,
            ) {
                return Err(self.cleanup_failed_adoption(&run_id, error));
            }
        }
        self.refresh_missions(project);
        Ok(())
    }

    fn schedule_launch(
        &mut self,
        project: &str,
        slug: &str,
        mode: LaunchMode,
    ) -> Result<(), Error> {
        let run_id = self.next_run_id(project, slug);
        let environment_id = run_id.clone();
        self.run_phases.insert(run_id.clone(), RunPhase::Preparing);
        let scope = self.job_scope(project, Some(run_id.clone()));
        let store = self.store.clone();
        let backend = self.run_backend.clone();
        let environment_runtime = self.environment_runtime.clone();
        let project = project.to_string();
        let slug = slug.to_string();
        let jobs = self.jobs.as_mut().expect("installed above");
        jobs.start(
            JobKind::LaunchPreparation,
            scope,
            Duration::from_secs(120),
            move |job_cancellation| {
                let cancellation = RunCancellation(job_cancellation);
                if cancellation.is_cancelled() {
                    return Err("launch preparation cancelled".into());
                }
                let mut mission = load_launchable_mission(&store, &project, &slug)
                    .map_err(|error| error.to_string())?;
                let mut effective_pins = corpus_observe::project_source_pins(&store, &project)
                    .map_err(|error| error.to_string())?;
                effective_pins.extend(mission.pins.clone());
                mission.pins = effective_pins;
                let prepared = backend
                    .prepare_source_pins(&store, &project, &mission.pins, &cancellation)
                    .map_err(|error| error.to_string())?;
                if cancellation.is_cancelled() {
                    return Err("launch preparation cancelled".into());
                }
                load_launchable_mission(&store, &project, &slug)
                    .map_err(|error| error.to_string())?;
                let pins_json = if prepared.is_empty() {
                    None
                } else {
                    Some(serde_json::to_string(&prepared).map_err(|error| error.to_string())?)
                };
                mission.environment_session = Some(environment_id.storage_key());
                store
                    .update_mission(&project, &slug, &mission)
                    .map_err(|error| error.to_string())?;
                if environment_runtime
                    .open(&store, environment_id.clone(), prepared.clone())
                    .map_err(|error| error.to_string())?
                    .is_none()
                {
                    mission.environment_session = None;
                    store
                        .update_mission(&project, &slug, &mission)
                        .map_err(|error| error.to_string())?;
                }
                if let Err(error) = load_launchable_mission(&store, &project, &slug) {
                    if let Some(key) = mission.environment_session.as_deref() {
                        if let Ok(project_record) = corpus_core::Project::load(&store, &project) {
                            let _ = corpus_core::close_environment_session_key(
                                &store,
                                &project_record.plugin,
                                key,
                            );
                        }
                    }
                    return Err(error.to_string());
                }
                store
                    .load_agent(&project, &mission.agent)
                    .map_err(|error| error.to_string())?;
                store
                    .render_project_agents(&project)
                    .map_err(|error| error.to_string())?;
                let final_guard = if cancellation.is_cancelled() {
                    Err("launch preparation cancelled".to_string())
                } else {
                    load_launchable_mission(&store, &project, &slug)
                        .map(drop)
                        .map_err(|error| error.to_string())
                };
                if let Err(error) = final_guard {
                    if let Some(key) = mission.environment_session.as_deref() {
                        if let Ok(project_record) = corpus_core::Project::load(&store, &project) {
                            let _ = corpus_core::close_environment_session_key(
                                &store,
                                &project_record.plugin,
                                key,
                            );
                        }
                    }
                    return Err(error);
                }
                let model = corpus_core::agent_default_model(&store, &project, &mission.agent);
                let session_result = match mode {
                    LaunchMode::Resume => {
                        let conversation =
                            mission.opencode_session.as_deref().ok_or_else(|| {
                                "no opencode session recorded for this mission — nothing to resume"
                                    .to_string()
                            })?;
                        backend.resume(
                            &run_id,
                            &project,
                            &mission.agent,
                            model.as_deref(),
                            conversation,
                            pins_json.as_deref(),
                            mission.environment_session.as_deref(),
                            &cancellation,
                        )
                    }
                    LaunchMode::AdoptFresh | LaunchMode::DetachedFresh => {
                        let prompt = store
                            .mission_brief(&project, &slug)
                            .map(|brief| brief.trim().to_string())
                            .unwrap_or_default();
                        backend.spawn(
                            &run_id,
                            &project,
                            &mission.agent,
                            model.as_deref(),
                            &prompt,
                            pins_json.as_deref(),
                            mission.environment_session.as_deref(),
                            &cancellation,
                        )
                    }
                };
                let session = match session_result {
                    Ok(session) => session,
                    Err(error) => {
                        if let Some(key) = mission.environment_session.as_deref() {
                            if let Ok(project_record) = corpus_core::Project::load(&store, &project)
                            {
                                let _ = corpus_core::close_environment_session_key(
                                    &store,
                                    &project_record.plugin,
                                    key,
                                );
                            }
                        }
                        return Err(error.to_string());
                    }
                };
                let notice = matches!(mode, LaunchMode::DetachedFresh)
                    .then(|| mission_label(mission.name.as_deref(), &slug));
                Ok(AppJobOutput::LaunchReady(LaunchReady {
                    session,
                    mode,
                    notice,
                    environment_session: mission.environment_session.clone(),
                }))
            },
        );
        Ok(())
    }

    fn apply_launch_ready(&mut self, run_id: &RunId, mut ready: LaunchReady) -> Result<(), Error> {
        let mission = load_launchable_mission(&self.store, &run_id.project, &run_id.mission);
        let rejection = match &mission {
            Ok(_) => None,
            Err(error) => Some(format!(
                "mission {}/{} is no longer launchable before adoption: {error}",
                run_id.project, run_id.mission
            )),
        };
        if let Some(rejection) = rejection {
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let stopped = ready.session.stop();
            let mut cleanup_errors = stopped.cleanup_errors;
            let environment_session = mission
                .ok()
                .and_then(|mission| mission.environment_session)
                .or_else(|| ready.environment_session.clone());
            if let Some(key) = environment_session.as_deref() {
                match corpus_core::Project::load(&self.store, &run_id.project) {
                    Ok(project) => {
                        if let Err(error) = corpus_core::close_environment_session_key(
                        &self.store,
                        &project.plugin,
                        key,
                    ) {
                            cleanup_errors.push(format!("environment cleanup failed: {error}"));
                        }
                    }
                    Err(error) => cleanup_errors.push(format!(
                        "cannot resolve environment for cleanup: {error}"
                    )),
                }
            }
            let error = Error::Store(if cleanup_errors.is_empty() {
                self.finish_run(run_id);
                format!("{rejection}; spawned run was stopped")
            } else {
                let message = format!(
                    "{rejection}; cleanup failed: {}",
                    cleanup_errors.join("; ")
                );
                self.fail_run(
                    run_id,
                    RunPhaseKind::Stopping,
                    &Error::Store(message.clone()),
                    true,
                    true,
                );
                message
            });
            return Err(error);
        }
        let notice = ready.notice.take();
        let control_port = ready.session.control_port();
        let child_run_id = ready.session.launch_identity();
        self.run_phases.insert(run_id.clone(), RunPhase::Starting);
        match ready.mode {
            LaunchMode::DetachedFresh => {
                let tmux = ready
                    .session
                    .pty_attach_command()
                    .and_then(|argv| AppState::pty_attach_session(&argv));
                if let Some(session) = tmux {
                    if let Err(error) =
                        self.bind_fresh_run(
                            &run_id.project,
                            &run_id.mission,
                            Some(session),
                            child_run_id.clone(),
                            control_port,
                        )
                    {
                        let cleanup = ready.session.stop();
                        let combined = Error::Store(format!(
                            "{error}; spawned run cleanup transcript: {}; cleanup errors: {}",
                            cleanup.transcript.display(),
                            cleanup.cleanup_errors.join("; ")
                        ));
                        self.fail_run(run_id, RunPhaseKind::Running, &combined, false, false);
                        return Err(combined);
                    }
                    self.finish_run(run_id);
                } else {
                    self.background_active_run()?;
                    self.adopt_run(ready.session, run_id.clone());
                    if let Err(error) = self.bind_fresh_run(
                        &run_id.project,
                        &run_id.mission,
                        None,
                        child_run_id.clone(),
                        control_port,
                    ) {
                        return Err(self.cleanup_failed_adoption(run_id, error));
                    }
                }
            }
            LaunchMode::AdoptFresh | LaunchMode::Resume => {
                if let Err(error) = self.background_active_run() {
                    let cleanup = ready.session.stop();
                    let combined = Error::Store(format!(
                        "could not background the current run: {error}; new run cleanup transcript: {}; cleanup errors: {}",
                        cleanup.transcript.display(),
                        cleanup.cleanup_errors.join("; ")
                    ));
                    self.fail_run(run_id, RunPhaseKind::Starting, &combined, true, false);
                    return Err(combined);
                }
                self.adopt_run(ready.session, run_id.clone());
                let binding = match ready.mode {
                    LaunchMode::AdoptFresh => self.bind_fresh_run(
                        &run_id.project,
                        &run_id.mission,
                        self.live_run_session(),
                        child_run_id,
                        control_port,
                    ),
                    LaunchMode::Resume => self.bind_resumed_run(
                        &run_id.project,
                        &run_id.mission,
                        self.live_run_session(),
                        child_run_id,
                        control_port,
                    ),
                    LaunchMode::DetachedFresh => unreachable!(),
                };
                if let Err(error) = binding {
                    return Err(self.cleanup_failed_adoption(run_id, error));
                }
            }
        }
        self.refresh_live_sessions();
        self.refresh_missions(&run_id.project);
        if let Some(mission) = notice {
            self.launch_notices.push(LaunchNotice {
                mission,
                result: Ok(()),
            });
        }
        Ok(())
    }

    /// The shared launch preamble: load the mission, resolve its rev
    /// pins to shas + fetch trees (loud failure here must never tear
    /// down a working run — that happens only after this returns).
    fn prepare_launch(&mut self, run_id: &RunId) -> Result<(Mission, Option<String>), Error> {
        self.run_phases.insert(run_id.clone(), RunPhase::Preparing);
        let cancellation = RunCancellation::default();
        self.run_cancellations
            .insert(run_id.clone(), cancellation.clone());
        let result = (|| {
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch preparation cancelled".into()));
            }
            let mut mission_record = load_launchable_mission(
                &self.store,
                &run_id.project,
                &run_id.mission,
            )?;
            let mut effective_pins =
                corpus_observe::project_source_pins(&self.store, &run_id.project)?;
            effective_pins.extend(mission_record.pins.clone());
            mission_record.pins = effective_pins;
            let prepared = self.run_backend.prepare_source_pins(
                &self.store,
                &run_id.project,
                &mission_record.pins,
                &cancellation,
            )?;
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch preparation cancelled".into()));
            }
            load_launchable_mission(&self.store, &run_id.project, &run_id.mission)?;
            let pins_json = if prepared.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&prepared)?)
            };
            mission_record.environment_session = Some(run_id.storage_key());
            self.store
                .update_mission(&run_id.project, &run_id.mission, &mission_record)?;
            if self
                .environment_runtime
                .open(&self.store, run_id.clone(), prepared)?
                .is_none()
            {
                mission_record.environment_session = None;
                self.store
                    .update_mission(&run_id.project, &run_id.mission, &mission_record)?;
            }
            let final_guard = if cancellation.is_cancelled() {
                Err(Error::Store("launch preparation cancelled".into()))
            } else {
                load_launchable_mission(&self.store, &run_id.project, &run_id.mission).map(drop)
            };
            if let Err(error) = final_guard {
                if let Some(key) = mission_record.environment_session.as_deref() {
                    if let Ok(project) = Project::load(&self.store, &run_id.project) {
                        let _ = corpus_core::close_environment_session_key(
                            &self.store,
                            &project.plugin,
                            key,
                        );
                    }
                }
                return Err(error);
            }
            Ok((mission_record, pins_json))
        })();
        if let Err(error) = &result {
            self.run_cancellations.remove(run_id);
            if cancellation.is_cancelled() {
                self.finish_run(run_id);
            } else {
                self.fail_run(run_id, RunPhaseKind::Preparing, error, true, false);
            }
        }
        result
    }

    /// Request cancellation of the newest preparation for this mission.
    /// The operation owns no child yet; the cancellation gate is checked
    /// before any spawn/adoption can occur.
    pub fn cancel_preparation(&self, project: &str, mission: &str) -> bool {
        if let Some(jobs) = &self.jobs {
            if let Some(run_id) = self
                .run_phases
                .keys()
                .filter(|id| id.project == project && id.mission == mission)
                .max_by_key(|id| id.generation)
            {
                if jobs.cancel_scope(JobKind::LaunchPreparation, run_id) {
                    return true;
                }
            }
        }
        let cancellation = self
            .run_cancellations
            .iter()
            .filter(|(id, _)| id.project == project && id.mission == mission)
            .max_by_key(|(id, _)| id.generation)
            .map(|(_, cancellation)| cancellation);
        if let Some(cancellation) = cancellation {
            cancellation.cancel();
            true
        } else {
            false
        }
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
    fn background_active_run(&mut self) -> Result<(), Error> {
        if !self.run_active() {
            return Ok(());
        }
        if self.jobs.is_none() {
            self.capture_opencode_session();
        }
        if self.live_pty_attach().is_none() {
            if let Some(attempt) = self.stop_run() {
                if let Some(error) = attempt.error {
                    return Err(error);
                }
            }
        } else {
            // A tmux run is durably detached. Relinquish the app-owned
            // handle and phase before another run is adopted; the mission's
            // recorded session is now its lifecycle truth.
            self.run.take();
            if let Some(run_id) = self.owned_run_id.take() {
                self.finish_run(&run_id);
            }
        }
        Ok(())
    }

    /// Ids already bound to other missions.
    ///
    /// Concurrent runs share one run dir, so `opencode session list` shows
    /// every live mission's conversation. Excluding what is already claimed
    /// is what stops a slow-booting run from adopting a neighbour's.
    fn claimed_conversations(&self, project: &str, except: &str) -> BTreeSet<String> {
        self.trees
            .get(project)
            .into_iter()
            .flat_map(|tree| tree.missions.iter())
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
            let claimed = self.claimed_conversations(project, &slug);
            let Some(launched_at_ms) = launch_stamp_ms(&session) else {
                continue;
            };
            let Ok(id) = self.session_service.find_for_launch(
                &self.store.project_run_dir(project),
                launched_at_ms,
                &claimed,
            ) else {
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

    /// Re-export the opencode transcript of every live mission that has just
    /// finished a turn — i.e. its session settled into `Waiting` (done
    /// working, parked at the prompt). This is what keeps the Cost panel
    /// honest for an ACTIVE conversation: usage updates at each turn
    /// boundary, not only at Stop (which a run killed with the app never
    /// reaches). Keyed by opencode session id, the export overwrites in
    /// place, so the file just grows more accurate turn by turn.
    ///
    /// Fires at most once per completed turn: the guard is the session's
    /// last paint (`session_activity`) being newer than our last export
    /// (`last_exported_at`). A session parked quiet since the last export
    /// has nothing new to record and is skipped; a failed export leaves the
    /// stamp untouched, so the next beat simply retries.
    fn sweep_usage_exports(&mut self, project: &str) {
        // (slug, opencode_session, tmux_session) for missions we could export.
        let pending: Vec<(String, String, String)> = self
            .missions
            .iter()
            .filter_map(|(slug, m)| {
                Some((
                    slug.clone(),
                    m.opencode_session.clone()?,
                    m.session.clone()?,
                ))
            })
            .filter(|(slug, _, _)| {
                matches!(
                    self.mission_activity(project, slug),
                    MissionActivity::Waiting
                )
            })
            .filter(|(_, _, tmux)| {
                // New output since our last export (or never exported) ⇒ a
                // turn happened. No activity reading ⇒ nothing to record.
                should_reexport(
                    self.session_activity.get(tmux).copied(),
                    self.last_exported_at.get(tmux).copied(),
                )
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        let mut changed = false;
        for (_slug, opencode, tmux) in pending {
            if self.run_backend.export_session(project, &opencode).is_ok() {
                self.last_exported_at
                    .insert(tmux, self.clock.monotonic_now());
                changed = true;
            }
        }
        if changed {
            // Fold the fresh exports into the Cost panel straight away.
            self.note_corpus_mutation(project);
            self.refresh_corpus_stats(project);
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
        mission.control = None;
        self.store.update_mission(project, slug, &mission)
    }

    fn bind_resumed_run(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
        run_id: Option<String>,
        control_port: Option<u16>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        mission.session = session;
        mission.control = run_id.zip(control_port).map(|(run_id, port)| {
            corpus_core::MissionControl { run_id, port }
        });
        self.store.update_mission(project, slug, &mission)
    }

    /// Bind a newly spawned run to its mission in one read-modify-write.
    /// A fresh run always starts a fresh opencode conversation, so the tmux
    /// session and old conversation id must never be committed separately.
    fn bind_fresh_run(
        &mut self,
        project: &str,
        slug: &str,
        session: Option<String>,
        child_run_id: Option<String>,
        control_port: Option<u16>,
    ) -> Result<(), Error> {
        let mut mission = self.store.load_mission(project, slug)?;
        let piped_child = session.is_none() && child_run_id.is_some();
        mission.session = session;
        mission.control = child_run_id
            .clone()
            .zip(control_port)
            .map(|(run_id, port)| corpus_core::MissionControl { run_id, port });
        mission.opencode_session = None;
        if let Some(dispatch) = mission
            .dispatch
            .as_mut()
            .filter(|dispatch| {
                dispatch.completion.is_none() && dispatch.child_run_id.is_none()
            })
        {
            dispatch.child_run_id = child_run_id;
            dispatch.live_seen = piped_child;
            dispatch.running_seen = piped_child;
        }
        self.store.update_mission(project, slug, &mission)
    }

    /// A process exists but its durable mission binding failed. The app
    /// already owns cleanup responsibility, so stop it before returning and
    /// preserve both the primary failure and cleanup evidence in the phase.
    fn cleanup_failed_adoption(&mut self, run_id: &RunId, primary: Error) -> Error {
        let cleanup = self.stop_run();
        let cleanup_pending = cleanup
            .as_ref()
            .is_some_and(|attempt| !attempt.cleanup_complete);
        let detail = match &cleanup {
            Some(attempt) => match &attempt.error {
                Some(error) => format!(
                    "{primary}; spawned run cleanup also failed: {error}; transcript: {}",
                    attempt.transcript.display()
                ),
                None => format!(
                    "{primary}; spawned run was stopped; transcript: {}",
                    attempt.transcript.display()
                ),
            },
            None => format!("{primary}; spawned run cleanup produced no transcript"),
        };
        let combined = Error::Store(detail);
        self.fail_run(
            run_id,
            RunPhaseKind::Running,
            &combined,
            cleanup_pending,
            cleanup_pending,
        );
        combined
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
        let Some(run_id) = self.owned_run_id.clone() else {
            return;
        };
        if !self.run_active() || self.run_phase(&run_id) != RunPhase::Running {
            return;
        }
        let known = self
            .store
            .load_mission(&run_id.project, &run_id.mission)
            .ok()
            .and_then(|m| m.opencode_session);
        if known.is_some() {
            return;
        }
        let claimed = self.claimed_conversations(&run_id.project, &run_id.mission);
        let Some(id) = self
            .run
            .as_mut()
            .and_then(|run| run.opencode_session_id(&claimed))
        else {
            return;
        };
        self.apply_discovered_conversation(&run_id, id);
    }

    /// Apply discovery only to the exact generation that requested it.
    /// Late background results from an older launch are harmless values.
    fn apply_discovered_conversation(&mut self, run_id: &RunId, id: String) -> bool {
        if self.owned_run_id.as_ref() != Some(run_id) || self.run_phase(run_id) != RunPhase::Running
        {
            return false;
        }
        if self
            .set_opencode_session(&run_id.project, &run_id.mission, Some(id))
            .is_err()
        {
            return false;
        }
        self.refresh_missions(&run_id.project);
        true
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

    /// Stop a mission's run: attempt transcript export and cleanup, surfacing
    /// each failure — whether the app owns the run or it survived an app
    /// relaunch. Clears the dead tmux session and returns the durable
    /// transcript path when known. The opencode session id STAYS on the
    /// record: stopping ends the attach, not the conversation, and that
    /// id is what `resume_mission` re-opens.
    pub fn stop_mission(&mut self, project: &str, slug: &str) -> Result<StopMissionResult, Error> {
        let mission = self.store.load_mission(project, slug)?;
        let session = mission.session.as_deref();
        if session.is_none() && !self.mission_environment_needs_cleanup(project, slug) {
            return Err(Error::Store(
                "no live session or environment on this mission — nothing to stop".into(),
            ));
        }
        if session.is_none() {
            let project_record = corpus_core::Project::load(&self.store, project)?;
            let key = mission
                .environment_session
                .as_deref()
                .ok_or_else(|| Error::Store("environment cleanup identity disappeared".into()))?;
            if matches!(
                self.store
                    .load_environment_session_key(&project_record.plugin, key),
                Err(corpus_core::Error::Io(ref error))
                    if error.kind() == std::io::ErrorKind::NotFound
            ) {
                // The mission key is written immediately before the durable
                // Opening record. If the app died in that tiny interval, no
                // plugin mutation was attempted and clearing the unmatched
                // key is the complete recovery action.
                let mut repaired = mission.clone();
                repaired.environment_session = None;
                self.store.update_mission(project, slug, &repaired)?;
                return Ok(StopMissionResult::Completed(String::new()));
            }
        }
        if self.jobs.is_some() {
            self.schedule_teardown(project, slug, &mission, session)?;
            return Ok(StopMissionResult::Scheduled);
        }
        if session.is_none() {
            let run_id = self.next_run_id(project, slug);
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let project_record = corpus_core::Project::load(&self.store, project)?;
            let environment_session = mission
                .environment_session
                .as_deref()
                .ok_or_else(|| Error::Store("environment cleanup identity disappeared".into()))?;
            match corpus_core::close_environment_session_key(
                &self.store,
                &project_record.plugin,
                environment_session,
            ) {
                Ok(()) => {
                    self.finish_run(&run_id);
                    return Ok(StopMissionResult::Completed(String::new()));
                }
                Err(error) => {
                    self.fail_run(&run_id, RunPhaseKind::Stopping, &error, true, true);
                    return Err(error);
                }
            }
        }
        let session = session.expect("checked above");
        let (path, mut error, mut cleanup_complete) = if self.live_run_session().as_deref()
            == Some(session)
        {
            let attempt = self.stop_run().ok_or_else(|| {
                Error::Store("run ownership disappeared before Stop could begin".into())
            })?;
            (
                Some(attempt.transcript.display().to_string()),
                attempt.error,
                attempt.cleanup_complete,
            )
        } else {
            let run_id = self.next_run_id(project, slug);
            self.run_phases.insert(run_id.clone(), RunPhase::Exporting);
            // A run that outlived the app: stop the writer before exporting
            // so OpenCode cannot return a partial JSON string. Both outcomes
            // are still reported independently.
            self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
            let cleanup_error = self
                .run_backend
                .kill_tmux_session(session)
                .err()
                .map(|error| format!("session cleanup failed: {error}"));
            let (exported, export_error) = match mission.opencode_session.as_deref() {
                Some(id) => match self.run_backend.export_session(project, id) {
                    Ok(path) => (Some(path.display().to_string()), None),
                    Err(error) => (None, Some(format!("transcript export failed: {error}"))),
                },
                None => (None, None),
            };
            let cleanup_complete = cleanup_error.is_none();
            let error = match (export_error, cleanup_error) {
                (None, None) => None,
                (Some(export), None) => Some(Error::Store(export)),
                (None, Some(cleanup)) => Some(Error::Store(cleanup)),
                (Some(export), Some(cleanup)) => Some(Error::Store(format!("{export}; {cleanup}"))),
            };
            if cleanup_complete {
                self.finish_run(&run_id);
                if let Some(error) = &error {
                    self.fail_run(&run_id, RunPhaseKind::Exporting, error, true, false);
                }
            } else {
                self.fail_run(
                    &run_id,
                    RunPhaseKind::Stopping,
                    error.as_ref().unwrap(),
                    true,
                    true,
                );
            }
            (exported, error, cleanup_complete)
        };
        if let Some(environment_session) = mission.environment_session.as_deref() {
            if let Ok(project_record) = corpus_core::Project::load(&self.store, project) {
                if let Err(environment_error) = corpus_core::close_environment_session_key(
                    &self.store,
                    &project_record.plugin,
                    environment_session,
                ) {
                    cleanup_complete = false;
                    error = Some(Error::Store(match error {
                        Some(existing) => {
                            format!("{existing}; environment cleanup failed: {environment_error}")
                        }
                        None => format!("environment cleanup failed: {environment_error}"),
                    }));
                }
            }
        }
        // Clear the durable binding only when cleanup actually completed.
        // A failed kill keeps the identity available for a retry.
        if cleanup_complete {
            self.set_tmux_session(project, slug, None)?;
        }
        self.refresh_live_sessions();
        if let Some(error) = error {
            return Err(error);
        }
        Ok(StopMissionResult::Completed(path.unwrap_or_default()))
    }

    fn schedule_teardown(
        &mut self,
        project: &str,
        slug: &str,
        mission: &Mission,
        tmux: Option<&str>,
    ) -> Result<(), Error> {
        let owned = tmux.is_some() && self.live_run_session().as_deref() == tmux;
        let run_id = if owned {
            self.owned_run_id.take().ok_or_else(|| {
                Error::Store("run ownership disappeared before Stop could begin".into())
            })?
        } else {
            self.next_run_id(project, slug)
        };
        self.run_phases.insert(run_id.clone(), RunPhase::Stopping);
        let retained = if owned { self.run.take() } else { None };
        let backend = self.run_backend.clone();
        let store = self.store.clone();
        let project_owned = project.to_string();
        let conversation = mission.opencode_session.clone();
        let environment_session = mission.environment_session.clone();
        let plugin_id = corpus_core::Project::load(&self.store, project)
            .ok()
            .map(|project| project.plugin);
        let tmux_owned = tmux.map(str::to_string);
        let session_operation = self.session_operation_leases.claim(project, slug);
        let scope = self.job_scope(project, Some(run_id));
        self.jobs.as_mut().expect("installed above").start(
            JobKind::SessionTeardown,
            scope,
            Duration::from_secs(30),
            move |_| {
                let _ownership = session_operation
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if let Some(mut session) = retained {
                    let outcome = session.stop();
                    let mut errors = outcome.cleanup_errors.clone();
                    if let Some(error) = outcome.export_error {
                        errors.insert(0, error);
                    }
                    let mut environment_cleanup_failed = false;
                    if let (Some(plugin_id), Some(environment_session)) =
                        (plugin_id.as_deref(), environment_session.as_deref())
                    {
                        if let Err(error) = corpus_core::close_environment_session_key(
                            &store,
                            plugin_id,
                            environment_session,
                        ) {
                            environment_cleanup_failed = true;
                            errors.push(format!("environment cleanup failed: {error}"));
                        }
                    }
                    let cleanup_complete =
                        outcome.cleanup_errors.is_empty() && !environment_cleanup_failed;
                    return Ok(AppJobOutput::TeardownReady(TeardownReady {
                        transcript: Some(outcome.transcript.display().to_string()),
                        error: (!errors.is_empty()).then(|| errors.join("; ")),
                        cleanup_complete,
                        retained: (!cleanup_complete).then_some(session),
                    }));
                }
                // Stabilize OpenCode's session data before asking the CLI to
                // serialize it. A live streaming response can otherwise end
                // in a successful but truncated export.
                let cleanup_error = tmux_owned.as_deref().and_then(|tmux| {
                    backend
                        .kill_tmux_session(tmux)
                        .err()
                        .map(|error| format!("session cleanup failed: {error}"))
                });
                let (transcript, export_error) = match conversation.as_deref() {
                    Some(conversation) => {
                        match backend.export_session(&project_owned, conversation) {
                            Ok(path) => (Some(path.display().to_string()), None),
                            Err(error) => {
                                (None, Some(format!("transcript export failed: {error}")))
                            }
                        }
                    }
                    None => (None, None),
                };
                let environment_error = match (plugin_id.as_deref(), environment_session.as_deref())
                {
                    (Some(plugin_id), Some(environment_session)) => {
                        corpus_core::close_environment_session_key(
                            &store,
                            plugin_id,
                            environment_session,
                        )
                        .err()
                        .map(|error| format!("environment cleanup failed: {error}"))
                    }
                    _ => None,
                };
                let cleanup_complete = cleanup_error.is_none() && environment_error.is_none();
                let error = [export_error, cleanup_error, environment_error]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>();
                Ok(AppJobOutput::TeardownReady(TeardownReady {
                    transcript,
                    error: (!error.is_empty()).then(|| error.join("; ")),
                    cleanup_complete,
                    retained: None,
                }))
            },
        );
        Ok(())
    }

    fn apply_teardown_ready(&mut self, run_id: &RunId, ready: TeardownReady) -> (bool, String) {
        let delete_key = (run_id.project.clone(), run_id.mission.clone());
        let delete_requested = self.pending_mission_deletes.contains(&delete_key);
        if ready.cleanup_complete {
            if let Err(error) = self.set_tmux_session(&run_id.project, &run_id.mission, None) {
                self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, false);
                return (true, error.to_string());
            }
            self.finish_run(run_id);
            self.run_meta = None;
            if delete_requested {
                if let Err(error) = self.store.delete_mission(&run_id.project, &run_id.mission) {
                    self.pending_mission_deletes.remove(&delete_key);
                    return (true, error.to_string());
                }
                self.pending_mission_deletes.remove(&delete_key);
                if self.selected_mission.as_deref() == Some(run_id.mission.as_str()) {
                    self.selected_mission = None;
                }
            }
        } else if let Some(session) = ready.retained {
            self.run = Some(session);
            self.owned_run_id = Some(run_id.clone());
            let error = Error::Store(
                ready
                    .error
                    .clone()
                    .unwrap_or_else(|| "session cleanup failed".into()),
            );
            self.fail_run(run_id, RunPhaseKind::Stopping, &error, true, true);
        }
        self.refresh_live_sessions();
        self.refresh_missions(&run_id.project);
        if let Some(error) = ready.error {
            if delete_requested && ready.cleanup_complete {
                (true, format!("mission deleted; {error}"))
            } else {
                (true, error)
            }
        } else {
            // Export still happens; the path belongs in the run store, not a
            // routine success notification.
            drop(ready.transcript);
            (
                false,
                if delete_requested {
                    "mission deleted"
                } else {
                    "mission stopped"
                }
                .into(),
            )
        }
    }

    /// The model the launch dialog pre-fills: the registry's curated
    /// tool-use default (an explicit arg — the engine never falls back
    /// to opencode's ambient model). None when the registry is empty.
    pub fn suggested_model(&self) -> Option<String> {
        corpus_core::ModelRegistry::load_default()
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

#[derive(Clone)]
struct DispatchDeliveryItem {
    project: String,
    slug: String,
    child_run_id: Option<String>,
    completion: corpus_core::MissionCompletion,
    delivery_attempt: u32,
    delivery_message_id: Option<String>,
}

/// Advance dispatched children from running to completed using the private
/// endpoint owned by each exact child TUI. A live-but-quiet terminal is not a
/// terminal event: intermediate assistant messages ending in `tool-calls`
/// remain active, while the first later non-tool finish is the restart-safe
/// whole-loop completion proof.
fn reconcile_dispatch_activity(
    store: &Store,
    service: &dyn SessionService,
    live: &[String],
) -> Result<(), String> {
    let projects = store.list_projects().map_err(|error| error.to_string())?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut failures = Vec::new();
    for (project, _) in projects {
        let missions = match store.list_missions(&project) {
            Ok(missions) => missions,
            Err(error) => {
                failures.push(format!("{project}: {error}"));
                continue;
            }
        };
        for (slug, mut mission) in missions {
            let Some(snapshot) = mission.dispatch.as_ref() else {
                continue;
            };
            if snapshot.completion.is_some() {
                continue;
            }
            let Some(child_run_id) = snapshot.child_run_id.as_deref() else {
                continue;
            };
            let exact_live = mission.session.as_deref() == Some(child_run_id)
                && live.iter().any(|session| session == child_run_id);
            if !exact_live {
                continue;
            }
            let Some(control) = mission
                .control
                .as_ref()
                .filter(|control| control.run_id == child_run_id)
            else {
                continue;
            };
            let Some(conversation) = mission.opencode_session.as_ref() else {
                continue;
            };
            let password = match corpus_core::opencode_control_password(store, child_run_id) {
                Ok(password) => password,
                Err(error) => {
                    failures.push(format!("{project}/{slug}: {error}"));
                    continue;
                }
            };
            let session = SessionRef {
                id: conversation.clone(),
                directory: store.project_run_dir(&project),
            };
            let launched_at_ms = launch_stamp_ms(child_run_id).unwrap_or(0);
            let turn_state = match service.session_turn_state(
                control,
                &password,
                &session,
                launched_at_ms,
            ) {
                Ok(state) => state,
                Err(error) => {
                    failures.push(format!("{project}/{slug}: {error}"));
                    continue;
                }
            };
            let dispatch = mission.dispatch.as_mut().expect("checked above");
            let changed = match turn_state {
                SessionTurnState::Active if !dispatch.running_seen => {
                    dispatch.running_seen = true;
                    true
                }
                SessionTurnState::Completed => {
                    dispatch.running_seen = true;
                    dispatch.completion =
                        Some(corpus_core::MissionCompletion::Completed { at: now });
                    true
                }
                SessionTurnState::Pending | SessionTurnState::Active => false,
            };
            if changed {
                if let Err(error) = store.update_mission(&project, &slug, &mission) {
                    failures.push(format!("{project}/{slug}: {error}"));
                }
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("mission completion delivery failed: {}", failures.join("; ")))
    }
}

/// Reconcile persisted completion deliveries, then group newly completed
/// children by launcher-proven parent and resume each exact curator. Intent,
/// admission, and acknowledgement are deliberately separate: a stored id can
/// wait safely while the curator is active, and only a terminal assistant
/// response marks delivery complete.
fn deliver_completed_dispatches(
    store: &Store,
    service: &dyn SessionService,
    live: &[String],
) -> Result<(), String> {
    let projects = store.list_projects().map_err(|error| error.to_string())?;
    let mut groups: BTreeMap<corpus_core::MissionRunRef, Vec<DispatchDeliveryItem>> =
        BTreeMap::new();
    for (project, _) in projects {
        let Ok(missions) = store.list_missions(&project) else {
            continue;
        };
        for (slug, mission) in missions {
            let Some(dispatch) = mission.dispatch else {
                continue;
            };
            let Some(completion) = dispatch.completion else {
                continue;
            };
            // Records written by the old admission-is-delivery implementation
            // have `delivered=true` but no message id. They were never
            // acknowledged and must be repaired rather than silently skipped.
            if dispatch.delivered && dispatch.delivery_message_id.is_some() {
                continue;
            }
            groups
                .entry(dispatch.parent)
                .or_default()
                .push(DispatchDeliveryItem {
                    project: project.clone(),
                    slug,
                    child_run_id: dispatch.child_run_id,
                    completion,
                    delivery_attempt: dispatch.delivery_attempt,
                    delivery_message_id: dispatch.delivery_message_id,
                });
        }
    }
    if groups.is_empty() {
        return Ok(());
    }
    let mut failures = Vec::new();
    for (parent, mut children) in groups {
        children.sort_by(|left, right| {
            (&left.project, &left.slug).cmp(&(&right.project, &right.slug))
        });
        let Ok(parent_mission) = store.load_mission(&parent.project, &parent.mission) else {
            continue;
        };
        let Some(control) = parent_mission.control.as_ref() else {
            continue;
        };
        if parent_mission.session.as_deref() != Some(parent.run_id.as_str())
            || control.run_id != parent.run_id
            || !live.iter().any(|session| session == &parent.run_id)
        {
            continue;
        }
        let Some(conversation) = parent_mission.opencode_session.as_ref() else {
            continue;
        };
        let Ok(password) = corpus_core::opencode_control_password(store, &control.run_id) else {
            continue;
        };

        let session = SessionRef {
            id: conversation.clone(),
            directory: store.project_run_dir(&parent.project),
        };

        let mut admitted: BTreeMap<String, Vec<DispatchDeliveryItem>> = BTreeMap::new();
        let mut pending = Vec::new();
        for child in children {
            if let Some(message_id) = child.delivery_message_id.clone() {
                admitted.entry(message_id).or_default().push(child);
            } else {
                pending.push(child);
            }
        }
        for (message_id, attempted) in admitted {
            match service.prompt_delivery_state(
                control,
                &password,
                &session,
                &message_id,
            ) {
                Ok(PromptDeliveryState::Acknowledged) => {
                    for child in &attempted {
                        mark_dispatch_acknowledged(store, &parent, child, &message_id);
                    }
                }
                Ok(PromptDeliveryState::Failed { error, retry_ready }) => {
                    // Keep the failed admission durable. Re-posting the same
                    // id cannot restart it, while immediately minting ids in a
                    // loop can burn credits. A deliberate model switch is the
                    // event that permits attempt N+1.
                    if retry_ready {
                        for child in &attempted {
                            mark_dispatch_retryable(store, &parent, child, &message_id);
                        }
                    }
                    failures.push(format!(
                        "{}/{}: curator did not handle completion prompt: {error}{}",
                        parent.project,
                        parent.mission,
                        if retry_ready { "; retrying after model switch" } else { "" }
                    ));
                }
                Ok(PromptDeliveryState::Pending) => {
                    // A persisted id with no legacy user message is either a
                    // delivery deferred while the curator was active or an
                    // admission made by the unusable V2 queue path. Re-submit
                    // the same deterministic id; the adapter preflights the
                    // transcript, so this is idempotent.
                    let prompt = completion_prompt(&attempted);
                    if let Err(error) =
                        service.queue_prompt(control, &password, &session, &message_id, &prompt)
                    {
                        failures.push(format!(
                            "{}/{}: {error}",
                            parent.project, parent.mission
                        ));
                    }
                }
                Ok(PromptDeliveryState::Active) => {}
                Err(error) => failures.push(format!(
                    "{}/{}: {error}",
                    parent.project, parent.mission
                )),
            }
        }
        if pending.is_empty() {
            continue;
        }

        let attempt = pending
            .iter()
            .map(|child| child.delivery_attempt)
            .max()
            .unwrap_or(0)
            .saturating_add(1);
        let signature = pending
            .iter()
            .map(|child| {
                format!(
                    "{}/{}:{}:{}",
                    child.project,
                    child.slug,
                    child.child_run_id.as_deref().unwrap_or("launch_failed"),
                    serde_json::to_string(&child.completion).unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("|");
        let message_id = format!(
            "msg_corpus{}",
            corpus_core::fnv1a_hex(
                format!(
                    "{}/{}/{}|attempt={attempt}|{signature}",
                    parent.project, parent.mission, parent.run_id,
                )
                .as_bytes()
            )
        );
        let prompt = completion_prompt(&pending);
        match service.queue_prompt(control, &password, &session, &message_id, &prompt) {
            Ok(()) => {
                for child in &pending {
                    if !mark_dispatch_admitted(store, &parent, child, attempt, &message_id) {
                        failures.push(format!(
                            "{}/{}: admitted completion prompt but could not persist its state",
                            child.project, child.slug
                        ));
                    }
                }
            }
            Err(error) => {
                failures.push(format!("{}/{}: {error}", parent.project, parent.mission));
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("mission completion delivery failed: {}", failures.join("; ")))
    }
}

fn mark_dispatch_admitted(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    attempt: u32,
    message_id: &str,
) -> bool {
    let Ok(mut mission) = store.load_mission(&child.project, &child.slug) else {
        return false;
    };
    let Some(dispatch) = mission.dispatch.as_mut() else {
        return false;
    };
    if &dispatch.parent != parent
        || dispatch.child_run_id != child.child_run_id
        || dispatch.completion.as_ref() != Some(&child.completion)
        || dispatch.delivery_message_id.is_some()
    {
        return false;
    }
    dispatch.delivery_attempt = attempt;
    dispatch.delivery_message_id = Some(message_id.to_string());
    dispatch.delivered = false;
    store
        .update_mission(&child.project, &child.slug, &mission)
        .is_ok()
}

fn mark_dispatch_acknowledged(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    message_id: &str,
) -> bool {
    let Ok(mut mission) = store.load_mission(&child.project, &child.slug) else {
        return false;
    };
    let Some(dispatch) = mission.dispatch.as_mut() else {
        return false;
    };
    if &dispatch.parent != parent
        || dispatch.child_run_id != child.child_run_id
        || dispatch.completion.as_ref() != Some(&child.completion)
        || dispatch.delivery_message_id.as_deref() != Some(message_id)
        || dispatch.delivered
    {
        return false;
    }
    dispatch.delivered = true;
    store
        .update_mission(&child.project, &child.slug, &mission)
        .is_ok()
}

fn mark_dispatch_retryable(
    store: &Store,
    parent: &corpus_core::MissionRunRef,
    child: &DispatchDeliveryItem,
    message_id: &str,
) -> bool {
    let Ok(mut mission) = store.load_mission(&child.project, &child.slug) else {
        return false;
    };
    let Some(dispatch) = mission.dispatch.as_mut() else {
        return false;
    };
    if &dispatch.parent != parent
        || dispatch.child_run_id != child.child_run_id
        || dispatch.completion.as_ref() != Some(&child.completion)
        || dispatch.delivery_message_id.as_deref() != Some(message_id)
        || dispatch.delivered
    {
        return false;
    }
    dispatch.delivery_message_id = None;
    store
        .update_mission(&child.project, &child.slug, &mission)
        .is_ok()
}

fn completion_summary(completion: &corpus_core::MissionCompletion) -> String {
    match completion {
        corpus_core::MissionCompletion::Completed { .. } => "completed".into(),
        corpus_core::MissionCompletion::UnexpectedExit { .. } => "exited unexpectedly".into(),
        corpus_core::MissionCompletion::LaunchFailed { error, .. } => {
            let bounded = error.chars().take(240).collect::<String>();
            format!("launch failed: {bounded}")
        }
    }
}

fn completion_prompt(children: &[DispatchDeliveryItem]) -> String {
    let mut prompt = String::from(
        "[Corpus mission completions]\nThe missions you dispatched have finished. Continue your current work using these results; do not poll mission status.\n",
    );
    for child in children {
        prompt.push_str(&format!(
            "- {}/{}: {}\n",
            child.project,
            child.slug,
            completion_summary(&child.completion)
        ));
    }
    prompt
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

/// A selected-only probe returns unprobed catalog rows for every other
/// plugin. Preserve their last checked result rather than turning a healthy
/// cached badge back into "unknown" whenever another picker entry is probed.
fn merge_plugin_statuses(
    previous: &[PluginStatus],
    discovered: Vec<PluginStatus>,
) -> Vec<PluginStatus> {
    discovered
        .into_iter()
        .map(|status| {
            if status.probed {
                status
            } else {
                previous
                    .iter()
                    .find(|old| old.name == status.name && old.probed)
                    .cloned()
                    .unwrap_or(status)
            }
        })
        .collect()
}

fn global_job_scope() -> JobScope {
    JobScope {
        project: String::new(),
        project_generation: 0,
        corpus_revision: None,
        run_id: None,
    }
}

fn plugin_work_active(jobs: &JobSet<AppJobOutput>) -> bool {
    [
        JobKind::PluginInstall,
        JobKind::PluginSetup,
        JobKind::PluginDoctor,
        JobKind::PluginStop,
    ]
    .into_iter()
    .any(|kind| jobs.is_kind_active(kind))
}

/// Load durable leases and evaluate only drift that can be proven without a
/// fetch: immutable plugin identity, manifest-default pins, and literal SHA
/// pins. Named custom revs remain visible as recorded identities but are not
/// guessed from ambient network state.
fn prepared_plugin_leases(
    store: &Store,
    project: Option<&str>,
    plugin_id: Option<&str>,
    statuses: &[PluginStatus],
) -> Vec<PluginLeaseView> {
    let (Some(project), Some(plugin_id)) = (project, plugin_id) else {
        return Vec::new();
    };
    let selected = statuses.iter().find(|status| status.name == plugin_id);
    let manifest = corpus_core::find_plugin(plugin_id).ok().flatten();
    let mut leases = Vec::new();
    let missions: BTreeMap<String, Mission> = store
        .list_missions(project)
        .unwrap_or_default()
        .into_iter()
        .collect();
    for record in store
        .list_environment_sessions(plugin_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|record| record.id.project == project)
    {
        if record.state == corpus_core::EnvironmentSessionState::Closed {
            continue;
        }
        let mission_slug = record.id.mission.clone();
        let mission = missions.get(&mission_slug);
        let mut drift = Vec::new();
        if mission.is_none() {
            drift.push("mission record is missing; automatic orphan cleanup pending".into());
        }
        if record.plugin_id != plugin_id {
            drift.push(format!(
                "plugin id {} != selected {plugin_id}",
                record.plugin_id
            ));
        }
        if let Some(status) = selected {
            if status.version.as_deref() != Some(record.plugin_version.as_str()) {
                drift.push(format!(
                    "plugin version {} != selected {}",
                    record.plugin_version,
                    status.version.as_deref().unwrap_or("unknown")
                ));
            }
            if status.bundle_digest.as_deref() != Some(record.plugin_digest.as_str()) {
                drift.push("plugin bundle digest differs from selected install".into());
            }
            if let (Some(prepared), Some(lease)) = (
                status.prepared.environment_lock.as_deref(),
                record.environment_lock.as_deref(),
            ) {
                if prepared != lease {
                    drift.push("environment lock differs from prepared environment".into());
                }
            }
        }
        if let (Some(plugin), Some(mission)) = (manifest.as_ref(), mission) {
            for source in &plugin.manifest.sources {
                let chosen = mission
                    .pins
                    .get(&source.id)
                    .map(String::as_str)
                    .unwrap_or(source.default_rev.as_str());
                let expected = if corpus_core::is_commit_sha(chosen) {
                    Some(chosen)
                } else if chosen == source.default_rev {
                    Some(source.default_sha.as_str())
                } else {
                    None
                };
                if let (Some(expected), Some(active)) =
                    (expected, record.source_shas.get(&source.id))
                {
                    if expected != active {
                        drift.push(format!(
                            "{} pin resolves to {} but lease runs {}",
                            source.id, expected, active
                        ));
                    }
                }
            }
        }
        leases.push(PluginLeaseView {
            session_key: record.id.storage_key(),
            mission: mission_label(mission.and_then(|record| record.name.as_deref()), &mission_slug),
            mission_slug,
            orphaned: mission.is_none(),
            state: record.state,
            plugin_version: record.plugin_version,
            plugin_digest: record.plugin_digest,
            source_shas: record.source_shas,
            environment_lock: record.environment_lock,
            image_digest: record.image_digest,
            drift,
            error: record.error,
        });
    }
    leases.sort_by(|a, b| (&a.mission, &a.mission_slug).cmp(&(&b.mission, &b.mission_slug)));
    leases
}

fn orphan_environment_sessions(store: &Store) -> Vec<(String, String)> {
    store
        .list_all_environment_sessions()
        .unwrap_or_default()
        .into_iter()
        // A failed automatic close remains durable and operator-visible. Do
        // not hammer a broken Docker/plugin runtime every reconciliation
        // beat; the retry action explicitly re-enters cleanup.
        .filter(|record| {
            !matches!(
                record.state,
                corpus_core::EnvironmentSessionState::Closed
                    | corpus_core::EnvironmentSessionState::Failed
            )
        })
        .filter(|record| {
            store
                .load_mission(&record.id.project, &record.id.mission)
                .is_err()
        })
        .map(|record| (record.plugin_id, record.id.storage_key()))
        .collect()
}

fn plugin_recovery_hint(error: &str) -> Option<&'static str> {
    let error = error.to_ascii_lowercase();
    if error.contains("sessions_active") || error.contains("session(s) are active") {
        Some("Delete every live mission lease, then retry environment Stop.")
    } else if error.contains("source_missing")
        || error.contains("source identity mismatch")
        || error.contains("target identity")
        || error.contains("environment lock")
    {
        Some("Review the project source pins and selected plugin version, then run Setup and Doctor again.")
    } else if error.contains("isolation") || error.contains("cross_session") {
        Some("Stop the affected mission lease; retry only after Doctor confirms isolation.")
    } else if error.contains("cleanup") || error.contains("could not stop") {
        Some("Retry Stop. If a mission lease remains, delete that mission to retry its cleanup.")
    } else if error.contains("docker") {
        Some("Start Docker and retry Setup or Doctor.")
    } else if error.contains("immutable") || error.contains("already installed with digest") {
        Some("Install a new plugin version; immutable installed versions cannot be overwritten.")
    } else {
        None
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

/// Strip terminal control sequences once, when a fallback line enters app
/// state. Rendering the same long tail must not repeat this scan every frame.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    let b = c as u8;
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
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
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn write_finding_fixture(store: &Store, project: &str, name: &str, title: &str) {
        let path = store
            .project_corpus_dir(project)
            .join("findings")
            .join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, format!("# {title}\n")).unwrap();
    }

    fn finding_titles(state: &AppState) -> Vec<String> {
        match state.finding_discovery() {
            FindingDiscovery::Ready(cards) => cards.iter().map(|card| card.title.clone()).collect(),
            FindingDiscovery::Failed { last_good, .. } => {
                last_good.iter().map(|card| card.title.clone()).collect()
            }
            FindingDiscovery::Loading => Vec::new(),
        }
    }

    fn wait_for_finding_titles(state: &mut AppState, expected: &[&str]) {
        for _ in 0..300 {
            state.poll_background_jobs();
            let titles = finding_titles(state);
            if expected
                .iter()
                .all(|title| titles.iter().any(|value| value == title))
                && titles.len() == expected.len()
            {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!(
            "finding projection did not converge: expected {expected:?}, got {:?}",
            finding_titles(state)
        );
    }

    struct ManualClock {
        epoch: Instant,
        elapsed: std::sync::Mutex<Duration>,
        unix: u64,
    }

    impl ManualClock {
        fn new(unix: u64) -> Self {
            Self {
                epoch: Instant::now(),
                elapsed: std::sync::Mutex::new(Duration::ZERO),
                unix,
            }
        }

        fn advance(&self, duration: Duration) {
            *self.elapsed.lock().unwrap() += duration;
        }
    }

    impl Clock for ManualClock {
        fn monotonic_now(&self) -> Instant {
            self.epoch + *self.elapsed.lock().unwrap()
        }

        fn unix_seconds(&self) -> u64 {
            self.unix
        }
    }

    #[derive(Debug, Clone)]
    struct QueueCall {
        run_id: String,
        password: String,
        session_id: String,
        message_id: String,
        prompt: String,
    }

    struct RecordingQueueService {
        calls: Mutex<Vec<QueueCall>>,
        fail: AtomicBool,
        active: AtomicBool,
        prompt_state: Mutex<PromptDeliveryState>,
    }

    impl Default for RecordingQueueService {
        fn default() -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                fail: AtomicBool::new(false),
                active: AtomicBool::new(false),
                prompt_state: Mutex::new(PromptDeliveryState::Acknowledged),
            }
        }
    }

    impl SessionService for RecordingQueueService {
        fn health(
            &self,
        ) -> Result<crate::session_service::ServiceHealth, String> {
            Ok(crate::session_service::ServiceHealth {
                backend: crate::session_service::SessionBackend::Http,
                version: crate::session_service::MINIMUM_OPENCODE_VERSION.into(),
                compatible: true,
            })
        }

        fn list(
            &self,
            _directory: &std::path::Path,
        ) -> Result<Vec<crate::session_service::SessionSummary>, String> {
            Ok(Vec::new())
        }

        fn messages(
            &self,
            _session: &SessionRef,
        ) -> Result<Vec<crate::session_service::SessionMessage>, String> {
            Ok(Vec::new())
        }

        fn queue_prompt(
            &self,
            control: &corpus_core::MissionControl,
            password: &str,
            session: &SessionRef,
            message_id: &str,
            prompt: &str,
        ) -> Result<(), String> {
            if self.fail.load(Ordering::Relaxed) {
                return Err("injected queue failure".into());
            }
            self.calls.lock().unwrap().push(QueueCall {
                run_id: control.run_id.clone(),
                password: password.to_string(),
                session_id: session.id.clone(),
                message_id: message_id.to_string(),
                prompt: prompt.to_string(),
            });
            Ok(())
        }

        fn session_turn_state(
            &self,
            _control: &corpus_core::MissionControl,
            _password: &str,
            _session: &SessionRef,
            _launched_at_ms: u64,
        ) -> Result<SessionTurnState, String> {
            Ok(if self.active.load(Ordering::Relaxed) {
                SessionTurnState::Active
            } else {
                SessionTurnState::Completed
            })
        }

        fn prompt_delivery_state(
            &self,
            _control: &corpus_core::MissionControl,
            _password: &str,
            _session: &SessionRef,
            _message_id: &str,
        ) -> Result<PromptDeliveryState, String> {
            Ok(self.prompt_state.lock().unwrap().clone())
        }
    }

    struct FakeRun {
        lines: VecDeque<RunLine>,
        exit: Option<i32>,
        stop_export_error: bool,
        stop_cleanup_error: bool,
        stops: Arc<AtomicUsize>,
    }

    impl ActiveRun for FakeRun {
        fn poll_line(&mut self) -> Option<RunLine> {
            self.lines.pop_front()
        }

        fn try_exit_code(&mut self) -> Option<i32> {
            self.exit.take()
        }

        fn pty_attach_command(&self) -> Option<Vec<String>> {
            Some(vec![
                "tmux".into(),
                "attach".into(),
                "-t".into(),
                "fake-run".into(),
            ])
        }

        fn stop(&mut self) -> StopOutcome {
            self.stops.fetch_add(1, Ordering::Relaxed);
            StopOutcome {
                transcript: PathBuf::from("fake-transcript.log"),
                export_error: self
                    .stop_export_error
                    .then(|| "injected active export failure".into()),
                cleanup_errors: self
                    .stop_cleanup_error
                    .then(|| "injected active cleanup failure".into())
                    .into_iter()
                    .collect(),
            }
        }

        fn opencode_session_id(&mut self, _claimed: &BTreeSet<String>) -> Option<String> {
            Some("fake-conversation".into())
        }

        fn launch_identity(&self) -> Option<String> {
            Some("fake-run".into())
        }

        fn control_port(&self) -> Option<u16> {
            Some(43_111)
        }
    }

    #[derive(Default)]
    struct FakeRunBackend {
        spawns: AtomicUsize,
        exports: AtomicUsize,
        kills: AtomicUsize,
        block_export: AtomicBool,
        export_in_progress: AtomicBool,
        teardown_overlap: AtomicBool,
        fail_spawn: AtomicBool,
        fail_export: AtomicBool,
        fail_kill: AtomicBool,
        fail_active_cleanup: AtomicBool,
        cancel_during_prepare: AtomicBool,
        cancel_before_spawn: AtomicBool,
        remove_mission_on_spawn: std::sync::Mutex<Option<PathBuf>>,
        stops: Arc<AtomicUsize>,
    }

    impl RunBackend for FakeRunBackend {
        fn spawn(
            &self,
            _run_id: &RunId,
            _project: &str,
            _agent: &str,
            _model: Option<&str>,
            _mission: &str,
            _source_pins_json: Option<&str>,
            _environment_session: Option<&str>,
            cancellation: &RunCancellation,
        ) -> Result<Box<dyn ActiveRun>, Error> {
            if self.cancel_before_spawn.load(Ordering::Relaxed) {
                cancellation.cancel();
            }
            if cancellation.is_cancelled() {
                return Err(Error::Store("launch start cancelled".into()));
            }
            self.spawns.fetch_add(1, Ordering::Relaxed);
            if self.fail_spawn.load(Ordering::Relaxed) {
                return Err(Error::Store("injected spawn failure".into()));
            }
            if let Some(path) = self.remove_mission_on_spawn.lock().unwrap().take() {
                std::fs::remove_file(path).unwrap();
            }
            Ok(Box::new(FakeRun {
                lines: VecDeque::from([RunLine {
                    stderr: false,
                    text: "fake output".into(),
                }]),
                exit: Some(0),
                stop_export_error: false,
                stop_cleanup_error: self.fail_active_cleanup.load(Ordering::Relaxed),
                stops: self.stops.clone(),
            }))
        }

        fn resume(
            &self,
            run_id: &RunId,
            project: &str,
            agent: &str,
            model: Option<&str>,
            _opencode_session_id: &str,
            source_pins_json: Option<&str>,
            environment_session: Option<&str>,
            cancellation: &RunCancellation,
        ) -> Result<Box<dyn ActiveRun>, Error> {
            self.spawn(
                run_id,
                project,
                agent,
                model,
                "",
                source_pins_json,
                environment_session,
                cancellation,
            )
        }

        fn prepare_source_pins(
            &self,
            _store: &Store,
            _project: &str,
            pins: &BTreeMap<String, String>,
            cancellation: &RunCancellation,
        ) -> Result<BTreeMap<String, String>, Error> {
            if self.cancel_during_prepare.load(Ordering::Relaxed) {
                cancellation.cancel();
            }
            Ok(pins.clone())
        }

        fn export_session(
            &self,
            _project: &str,
            _opencode_session_id: &str,
        ) -> Result<PathBuf, Error> {
            self.exports.fetch_add(1, Ordering::Relaxed);
            self.export_in_progress.store(true, Ordering::Release);
            while self.block_export.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(1));
            }
            self.export_in_progress.store(false, Ordering::Release);
            if self.fail_export.load(Ordering::Relaxed) {
                return Err(Error::Store("injected detached export failure".into()));
            }
            Ok(PathBuf::from("fake-export.json"))
        }

        fn kill_tmux_session(&self, _session: &str) -> Result<(), Error> {
            self.kills.fetch_add(1, Ordering::Relaxed);
            if self.export_in_progress.load(Ordering::Acquire) {
                self.teardown_overlap.store(true, Ordering::Release);
            }
            if self.fail_kill.load(Ordering::Relaxed) {
                Err(Error::Store("injected tmux cleanup failure".into()))
            } else {
                Ok(())
            }
        }
    }

    struct FakeSessionCatalog;

    impl SessionCatalog for FakeSessionCatalog {
        fn live_tui_sessions(&self) -> Vec<String> {
            vec!["fake-run".into()]
        }

        fn raw_log(&self, _store: &Store, _project: &str, _session: &str) -> Option<PathBuf> {
            None
        }
    }

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
        assert_eq!(
            mission_label(Some("recon sweep"), "cdk-recon"),
            "recon sweep"
        );
        // No name, human slug: show the slug (the curator's mission id).
        assert_eq!(mission_label(None, "cdk-proto-attack"), "cdk-proto-attack");
        assert_eq!(
            mission_label(Some("  "), "cdk-proto-attack"),
            "cdk-proto-attack"
        );
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

    #[test]
    fn selected_plugin_probe_preserves_other_cached_health() {
        let status = |name: &str, probed: bool, ready: bool| PluginStatus {
            name: name.into(),
            version: None,
            description: None,
            probed,
            ready,
            notes: if probed {
                "checked".into()
            } else {
                "not probed".into()
            },
            running_version: None,
            expected_tag: None,
            protocol: Some(corpus_core::ENVIRONMENT_PROTOCOL_V1.into()),
            capabilities: Vec::new(),
            origin: corpus_core::PluginOrigin::Installed,
            bundle_digest: Some(format!("sha256:{name}")),
            prepared: corpus_core::PluginPreparedStatus::default(),
        };
        let previous = vec![status("a", true, true), status("b", false, false)];
        let next = vec![status("a", false, false), status("b", true, false)];
        let merged = merge_plugin_statuses(&previous, next);
        assert!(merged[0].probed && merged[0].ready);
        assert!(merged[1].probed && !merged[1].ready);
    }

    #[test]
    fn prepared_lease_projection_exposes_identity_drift_and_hides_closed_leases() {
        let root =
            std::env::temp_dir().join(format!("corpus-plugin-lease-view-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "fixture-regtest").unwrap();
        store
            .create_agent_with_role("p", "tester", corpus_core::AgentRole::Tester)
            .unwrap();
        let mission_slug = "f44eb586-1537-40d8-921e-d0a1e4182c89";
        let id = RunId {
            project: "p".into(),
            mission: mission_slug.into(),
            generation: 1,
        };
        let mission = Mission {
            agent: "tester".into(),
            pins: BTreeMap::new(),
            budget: None,
            created: 1,
            name: None,
            session: None,
            control: None,
            opencode_session: None,
            environment_session: Some(id.storage_key()),
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        };
        store
            .write_mission("p", mission_slug, &mission, "probe")
            .unwrap();
        let mut record = corpus_core::EnvironmentSessionRecord {
            id,
            plugin_id: "fixture-regtest".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: "sha256:old".into(),
            state: corpus_core::EnvironmentSessionState::Ready,
            source_shas: BTreeMap::from([("target".into(), "a".repeat(40))]),
            environment_lock: Some("lock:old".into()),
            image_digest: Some("sha256:target".into()),
            created: 1,
            updated: 1,
            error: None,
        };
        store.save_environment_session(&record).unwrap();
        let statuses = vec![PluginStatus {
            name: "fixture-regtest".into(),
            version: Some("2.0.0".into()),
            description: None,
            probed: true,
            ready: true,
            notes: "ready".into(),
            running_version: None,
            expected_tag: None,
            protocol: Some(corpus_core::ENVIRONMENT_PROTOCOL_V1.into()),
            capabilities: vec!["sessions".into()],
            origin: corpus_core::PluginOrigin::Installed,
            bundle_digest: Some("sha256:new".into()),
            prepared: corpus_core::PluginPreparedStatus {
                environment_lock: Some("lock:new".into()),
                ..Default::default()
            },
        }];
        let leases = prepared_plugin_leases(&store, Some("p"), Some("fixture-regtest"), &statuses);
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].mission, "new");
        assert_eq!(leases[0].mission_slug, mission_slug);
        assert_eq!(leases[0].image_digest.as_deref(), Some("sha256:target"));
        assert_eq!(leases[0].drift.len(), 3, "{:?}", leases[0].drift);

        record.state = corpus_core::EnvironmentSessionState::Closed;
        store.save_environment_session(&record).unwrap();
        assert!(
            prepared_plugin_leases(&store, Some("p"), Some("fixture-regtest"), &statuses,)
                .is_empty()
        );

        record.id.mission = "deleted-mission".into();
        record.state = corpus_core::EnvironmentSessionState::Ready;
        store.save_environment_session(&record).unwrap();
        let orphan = prepared_plugin_leases(
            &store,
            Some("p"),
            Some("fixture-regtest"),
            &statuses,
        );
        assert_eq!(orphan.len(), 1);
        assert_eq!(orphan[0].mission, "deleted-mission");
        assert!(orphan[0]
            .drift
            .iter()
            .any(|drift| drift.contains("automatic orphan cleanup pending")));
        assert_eq!(
            orphan_environment_sessions(&store),
            vec![(
                "fixture-regtest".to_string(),
                record.id.storage_key()
            )]
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn plugin_failures_map_to_actionable_recovery() {
        assert!(
            plugin_recovery_hint("sessions_active: 2 environment session(s) are active")
                .unwrap()
                .contains("mission lease")
        );
        assert!(plugin_recovery_hint("source identity mismatch")
            .unwrap()
            .contains("source pins"));
        assert!(plugin_recovery_hint("cross_session isolation failed")
            .unwrap()
            .contains("isolation"));
        assert!(plugin_recovery_hint("cleanup_failed")
            .unwrap()
            .contains("Retry Stop"));
    }

    #[test]
    fn ansi_is_removed_once_at_ingest_and_tail_bound_is_finite() {
        assert_eq!(strip_ansi("plain \u{1b}[31mred\u{1b}[0m"), "plain red");
        assert_eq!(MAX_RUN_LINES, 4_000);
    }

    fn mission(created: u64) -> Mission {
        Mission {
            agent: "operator".to_string(),
            pins: std::collections::BTreeMap::new(),
            budget: None,
            created,
            name: None,
            session: None,
            control: None,
            opencode_session: None,
            environment_session: None,
            launch_requested: None,
            delete_requested: None,
            dispatch: None,
        }
    }

    #[test]
    fn only_a_painting_session_counts_as_working() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        // Nothing up: the session state below is irrelevant.
        assert_eq!(activity_for(now, false, Some(now)), MissionActivity::Idle);
        // Live and painting right now — the pulse is earned.
        assert_eq!(activity_for(now, true, Some(now)), MissionActivity::Working);
        // Live but quiet past the window: an opencode TUI parked at its
        // prompt. This is the case that used to pulse forever.
        let stale = now - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1);
        assert_eq!(
            activity_for(now, true, Some(stale)),
            MissionActivity::Waiting
        );
        // Live with no capture to read: absence of evidence, not work.
        assert_eq!(activity_for(now, true, None), MissionActivity::Waiting);
    }

    #[test]
    fn idle_state_owns_no_repaint_deadline() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-idle-repaint-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let mut state = AppState::with_runtime(
            Store::new(root.clone()),
            Arc::new(ManualClock::new(1_700_000_123)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        assert_eq!(state.live_repaint_after(), None);

        state.live_sessions.push("corpus-unmapped".into());
        assert_eq!(state.live_repaint_after(), Some(Duration::from_secs(2)));
        state.live_sessions.clear();
        state.run = Some(Box::new(FakeRun {
            lines: VecDeque::new(),
            exit: None,
            stop_export_error: false,
            stop_cleanup_error: false,
            stops: Arc::new(AtomicUsize::new(0)),
        }));
        assert_eq!(state.live_repaint_after(), Some(Duration::from_millis(250)));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn external_working_session_does_not_create_an_animation_loop() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-working-repaint-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let clock = Arc::new(ManualClock::new(1_700_000_123));
        let mut state = AppState::with_runtime(
            Store::new(root.clone()),
            clock.clone(),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        let mut record = mission(1);
        record.session = Some("external-run".into());
        state.trees.insert(
            "p".into(),
            ProjectTree {
                agents: Vec::new(),
                missions: vec![("mission".into(), record)],
            },
        );
        state.live_sessions.push("external-run".into());
        state
            .session_activity
            .insert("external-run".into(), clock.monotonic_now());

        assert_eq!(state.live_repaint_after(), Some(Duration::from_secs(2)));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn injected_clock_controls_persisted_time_and_poll_throttles() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-clock-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store
            .create_project("clock-test", "Clock test", "cdk-regtest")
            .unwrap();
        store
            .create_agent_with_role(
                "clock-test",
                "researcher",
                corpus_core::AgentRole::Researcher,
            )
            .unwrap();
        let clock = Arc::new(ManualClock::new(1_700_000_123));
        let mut state = AppState::with_runtime(
            store.clone(),
            clock.clone(),
            Arc::new(CoreRunBackend),
            Arc::new(CoreSessionCatalog),
        );

        let mission = state
            .create_mission("clock-test", "researcher", "test brief")
            .unwrap();
        assert_eq!(
            store.load_mission("clock-test", &mission).unwrap().created,
            1_700_000_123
        );

        state.poll_launch_requests();
        let first_poll = state.launch_requests_polled_at.unwrap();
        clock.advance(STORE_BACKSTOP - Duration::from_millis(1));
        state.poll_launch_requests();
        assert_eq!(state.launch_requests_polled_at, Some(first_poll));
        clock.advance(Duration::from_millis(1));
        state.poll_launch_requests();
        assert!(state.launch_requests_polled_at.unwrap() > first_poll);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciler_consumes_a_durable_delete_request() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-delete-request-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.delete_requested = Some(MissionDeleteRequest { requested_at: 2 });
        store.write_mission("p", "delete-me", &record, "brief").unwrap();
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(3)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );

        state.poll_launch_requests();

        assert!(store.load_mission("p", "delete-me").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reconciler_cascades_durable_agent_and_project_delete_requests() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-parent-delete-request-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("agents", "Agents", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("agents", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        store
            .write_mission("agents", "child", &mission(1), "brief")
            .unwrap();
        store
            .create_project("project", "Project", "cdk-regtest")
            .unwrap();
        store
            .create_agent_with_role("project", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        store
            .write_mission("project", "child", &mission(1), "brief")
            .unwrap();
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(3)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        // Simulate requests authored by another process after the app built
        // its cache. The reconciliation scan must read parent flags from the
        // durable store, not wait for a UI refresh.
        store.request_agent_delete("agents", "operator").unwrap();
        store.request_project_delete("project").unwrap();
        state.poll_launch_requests();

        assert!(store.load_agent("agents", "operator").is_err());
        assert!(store.load_mission("agents", "child").is_err());
        assert!(Project::load(&store, "agents").is_ok());
        assert!(Project::load(&store, "project").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn consuming_a_launch_request_preserves_its_exact_parent_origin() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-launch-origin-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let origin = corpus_core::MissionRunRef {
            project: "p".into(),
            mission: "curator-a".into(),
            run_id: "p1-p-m9-curator-a-g3".into(),
        };
        let mut child = mission(1_700_000_123);
        child.launch_requested = Some(corpus_core::MissionLaunchRequest {
            requested_at: 1_700_000_124,
            requested_by: Some(origin.clone()),
        });
        store.write_mission("p", "child", &child, "work").unwrap();

        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(1_700_000_125)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.clear_launch_request("p", "child", true).unwrap();
        let stored = store.load_mission("p", "child").unwrap();
        assert_eq!(stored.launch_requested, None);
        assert_eq!(
            stored.dispatch.as_ref().map(|dispatch| &dispatch.parent),
            Some(&origin)
        );
        state
            .bind_fresh_run(
                "p",
                "child",
                Some("corpus-worker-1700000125".into()),
                Some("corpus-worker-1700000125".into()),
                Some(43_111),
            )
            .unwrap();
        assert_eq!(
            store
                .load_mission("p", "child")
                .unwrap()
                .dispatch
                .as_ref()
                .and_then(|dispatch| dispatch.child_run_id.as_deref()),
            Some("corpus-worker-1700000125")
        );
        assert_eq!(
            store.load_mission("p", "child").unwrap().control,
            Some(corpus_core::MissionControl {
                run_id: "corpus-worker-1700000125".into(),
                port: 43_111,
            })
        );

        let mut live = store.load_mission("p", "child").unwrap();
        live.launch_requested = Some(corpus_core::MissionLaunchRequest {
            requested_at: 1_700_000_126,
            requested_by: Some(corpus_core::MissionRunRef {
                project: "p".into(),
                mission: "curator-b".into(),
                run_id: "corpus-curator-b-1700000126".into(),
            }),
        });
        store.update_mission("p", "child", &live).unwrap();
        state.clear_launch_request("p", "child", false).unwrap();
        assert_eq!(
            store
                .load_mission("p", "child")
                .unwrap()
                .dispatch
                .map(|dispatch| dispatch.parent),
            Some(origin),
            "an already-live child cannot be silently reassigned"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn child_completion_uses_exact_process_activity_not_terminal_quiet() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-dispatch-completion-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let session = "corpus-worker-1700000000";
        let mut child = mission(1_700_000_000);
        child.session = Some(session.into());
        child.control = Some(corpus_core::MissionControl {
            run_id: session.into(),
            port: 41_001,
        });
        child.opencode_session = Some("ses_child".into());
        child.dispatch = Some(corpus_core::MissionDispatch {
            parent: corpus_core::MissionRunRef {
                project: "p".into(),
                mission: "curator-a".into(),
                run_id: "corpus-curator-1699999990".into(),
            },
            child_run_id: Some(session.into()),
            live_seen: false,
            running_seen: false,
            completion: None,
            delivery_attempt: 0,
            delivery_message_id: None,
            delivered: false,
        });
        store.write_mission("p", "child", &child, "work").unwrap();
        let clock = Arc::new(ManualClock::new(1_700_000_100));
        let mut state = AppState::with_runtime(
            store.clone(),
            clock,
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.live_sessions = vec![session.into()];

        let raw = store
            .project_corpus_dir("p")
            .join(corpus_core::RUNS)
            .join("1700000000-worker.raw");
        std::fs::create_dir_all(raw.parent().unwrap()).unwrap();
        std::fs::write(&raw, "").unwrap();

        // pipe-pane creates an empty capture immediately; that alone is not
        // evidence the child entered a turn.
        state.reconcile_mission_dispatches();
        let parked = store.load_mission("p", "child").unwrap().dispatch.unwrap();
        assert!(parked.live_seen);
        assert!(!parked.running_seen);
        assert_eq!(parked.completion, None);

        // Terminal output is a display signal only. Even after output, a
        // quiet interval cannot declare the child complete or running.
        std::fs::write(&raw, "working\n").unwrap();
        state.reconcile_mission_dispatches();
        assert!(!store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .running_seen);
        std::fs::remove_file(raw).unwrap();
        state.reconcile_mission_dispatches();
        assert_eq!(
            store
                .load_mission("p", "child")
                .unwrap()
                .dispatch
                .unwrap()
                .completion,
            None
        );

        // Only the exact owning OpenCode process may prove the foreground
        // turn started and then parked.
        let service = RecordingQueueService::default();
        service.active.store(true, Ordering::Relaxed);
        reconcile_dispatch_activity(&store, &service, &[session.into()]).unwrap();
        assert!(store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .running_seen);
        service.active.store(false, Ordering::Relaxed);
        reconcile_dispatch_activity(&store, &service, &[session.into()]).unwrap();
        let completed = store.load_mission("p", "child").unwrap().dispatch.unwrap();
        assert!(matches!(
            completed.completion.as_ref(),
            Some(corpus_core::MissionCompletion::Completed { .. })
        ));
        state.reconcile_mission_dispatches();
        let mut restarted = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(1_700_000_999)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        restarted.live_sessions = vec![session.into()];
        restarted.reconcile_mission_dispatches();
        assert_eq!(
            store
                .load_mission("p", "child")
                .unwrap()
                .dispatch
                .unwrap()
                .completion,
            completed.completion
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn disappeared_child_and_launch_failure_each_record_once() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-dispatch-failures-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        for slug in ["vanished", "failed"] {
            let mut child = mission(1_700_000_000);
            child.session = (slug == "vanished").then(|| "corpus-worker-1700000000".into());
            child.dispatch = Some(corpus_core::MissionDispatch {
                parent: corpus_core::MissionRunRef {
                    project: "p".into(),
                    mission: "curator".into(),
                    run_id: "corpus-curator-1699999990".into(),
                },
                child_run_id: child.session.clone(),
                live_seen: slug == "vanished",
                running_seen: false,
                completion: None,
                delivery_attempt: 0,
                delivery_message_id: None,
                delivered: false,
            });
            store.write_mission("p", slug, &child, "work").unwrap();
        }
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(1_700_000_100)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.live_sessions.clear();
        state.reconcile_mission_dispatches();
        assert_eq!(
            store
                .load_mission("p", "vanished")
                .unwrap()
                .dispatch
                .unwrap()
                .completion,
            Some(corpus_core::MissionCompletion::UnexpectedExit {
                at: 1_700_000_100
            })
        );

        state.record_dispatch_launch_failure("p", "failed", "boom");
        state.record_dispatch_launch_failure("p", "failed", "different retry");
        assert_eq!(
            store
                .load_mission("p", "failed")
                .unwrap()
                .dispatch
                .unwrap()
                .completion,
            Some(corpus_core::MissionCompletion::LaunchFailed {
                at: 1_700_000_100,
                error: "boom".into()
            })
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn file_events_only_make_coarse_reconciliation_domains_due() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-file-events-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        let clock = Arc::new(ManualClock::new(1_700_000_123));
        let mut state = AppState::with_runtime(
            store.clone(),
            clock.clone(),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("p".into());
        let now = clock.monotonic_now();
        state.corpus_polled_at = Some(now);
        state.launch_requests_polled_at = Some(now);
        state.session_activity_polled_at = Some(now);
        state.file_invalidations = Some(Box::new(
            crate::file_watch::FakeFileInvalidationSource::new(
                crate::file_watch::FileInvalidations {
                    metadata: BTreeSet::from(["p".into()]),
                    corpus: BTreeSet::from(["p".into()]),
                    activity: BTreeSet::from(["p".into()]),
                    ..crate::file_watch::FileInvalidations::default()
                },
            ),
        ));

        assert_eq!(state.poll_file_invalidations(), None);
        assert_eq!(state.corpus_polled_at, None);
        assert_eq!(state.corpus_revision("p"), 1);
        assert_eq!(state.launch_requests_polled_at, None);
        assert!(state.session_activity_dirty);
        // The bounded fake drains exactly once; no unbounded event queue is
        // retained in app state.
        assert_eq!(state.poll_file_invalidations(), None);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finding_projection_never_crosses_project_navigation() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-finding-navigation-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store.create_project("q", "Q", "cdk-regtest").unwrap();
        write_finding_fixture(&store, "p", "one.md", "Only P");
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.select_project("p");
        assert_eq!(finding_titles(&state), ["Only P"]);

        state.install_job_runtime(eframe::egui::Context::default());
        state.select_project("q");
        assert!(matches!(
            state.finding_discovery(),
            FindingDiscovery::Loading
        ));
        assert!(finding_titles(&state).is_empty());
        wait_for_finding_titles(&mut state, &[]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finding_failure_retains_only_the_same_projects_last_good_cards() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-finding-failure-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        write_finding_fixture(&store, "p", "one.md", "Last good");
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.select_project("p");
        state.fail_findings("p", "injected failure");

        match state.finding_discovery() {
            FindingDiscovery::Failed { message, last_good } => {
                assert_eq!(message, "injected failure");
                assert_eq!(last_good.len(), 1);
                assert_eq!(last_good[0].title, "Last good");
            }
            other => panic!("expected failed discovery, got {other:?}"),
        }
        state.prepare_findings_project("another");
        assert!(matches!(
            state.finding_discovery(),
            FindingDiscovery::Loading
        ));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn corpus_wipe_advances_guards_and_clears_selected_findings_immediately() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-finding-wipe-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        write_finding_fixture(&store, "p", "one.md", "Gone");
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.select_project("p");
        assert_eq!(finding_titles(&state), ["Gone"]);

        let project = state.wipe_project_corpus("p").unwrap();
        assert_eq!(project.corpus_generation, 1);
        assert_eq!(state.projects[0].1.corpus_generation, 1);
        assert_eq!(state.corpus_revision("p"), 1);
        assert!(finding_titles(&state).is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn finding_projection_reconciles_events_and_the_timed_backstop() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-finding-reconcile-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        write_finding_fixture(&store, "p", "one.md", "One");
        let clock = Arc::new(ManualClock::new(0));
        let mut state = AppState::with_runtime(
            store.clone(),
            clock.clone(),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("p".into());
        state.install_job_runtime(eframe::egui::Context::default());

        write_finding_fixture(&store, "p", "nested/two.md", "Two");
        state.file_invalidations = Some(Box::new(
            crate::file_watch::FakeFileInvalidationSource::new(
                crate::file_watch::FileInvalidations {
                    corpus: BTreeSet::from(["p".into()]),
                    ..crate::file_watch::FileInvalidations::default()
                },
            ),
        ));
        state.poll_file_invalidations();
        state.poll_project_scope();
        wait_for_finding_titles(&mut state, &["One", "Two"]);

        write_finding_fixture(&store, "p", "one.md", "One edited");
        std::fs::remove_file(store.project_corpus_dir("p").join("findings/nested/two.md")).unwrap();
        state.file_invalidations = Some(Box::new(
            crate::file_watch::FakeFileInvalidationSource::new(
                crate::file_watch::FileInvalidations {
                    corpus: BTreeSet::from(["p".into()]),
                    ..crate::file_watch::FileInvalidations::default()
                },
            ),
        ));
        state.poll_file_invalidations();
        state.poll_project_scope();
        wait_for_finding_titles(&mut state, &["One edited"]);

        // No event: the existing ten-second project reconciliation still
        // discovers the change, without a findings-specific timer.
        write_finding_fixture(&store, "p", "three.md", "Three");
        clock.advance(STORE_BACKSTOP);
        state.poll_project_scope();
        wait_for_finding_titles(&mut state, &["One edited", "Three"]);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stale_corpus_revision_is_rescheduled_after_the_active_key_clears() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-finding-revision-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        write_finding_fixture(&store, "p", "one.md", "Fresh");
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("p".into());
        state.install_job_runtime(eframe::egui::Context::default());
        let stale_scope = state.corpus_job_scope("p");
        state.note_corpus_mutation("p");

        assert!(state.retry_stale_corpus_job(JobKind::CorpusSummary, &stale_scope));
        wait_for_finding_titles(&mut state, &["Fresh"]);
        assert_eq!(state.corpus_revision("p"), 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn selecting_a_mission_never_spawns_or_prepares_a_run() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-mission-navigation-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(1_700_000_123)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        state.select_mission("p", "mission");

        assert_eq!(state.selected_mission.as_deref(), Some("mission"));
        assert_eq!(state.current_screen, Screen::Missions);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
        assert!(!state.run_active());
        assert!(
            state.run_generations.is_empty(),
            "navigation created run identity"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn delete_pending_mission_cannot_launch_through_app_state() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-delete-launch-guard-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        record.delete_requested = Some(MissionDeleteRequest { requested_at: 2 });
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(3)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        let error = state.launch_mission("p", "mission").unwrap_err();
        assert!(error.to_string().contains("pending deletion"), "{error}");
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
        assert!(state.run_generations.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parent_pending_deletion_cannot_launch_a_child_mission() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-parent-delete-launch-guard-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        for project in ["agent-parent", "project-parent"] {
            store
                .create_project(project, "P", "cdk-regtest")
                .unwrap();
            store
                .create_agent_with_role(project, "runner", corpus_core::AgentRole::Tester)
                .unwrap();
            let mut record = mission(1);
            record.agent = "runner".into();
            store
                .write_mission(project, "mission", &record, "brief")
                .unwrap();
        }
        store
            .request_agent_delete("agent-parent", "runner")
            .unwrap();
        store.request_project_delete("project-parent").unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(3)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        let agent_error = state
            .launch_mission("agent-parent", "mission")
            .unwrap_err();
        assert!(agent_error.to_string().contains("agent"), "{agent_error}");
        assert!(agent_error.to_string().contains("pending deletion"));
        let project_error = state
            .launch_mission("project-parent", "mission")
            .unwrap_err();
        assert!(project_error.to_string().contains("project"), "{project_error}");
        assert!(project_error.to_string().contains("pending deletion"));
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
        assert!(state.run_generations.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_deletion_before_async_adoption_stops_the_spawned_run() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-project-delete-adoption-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        store.request_project_delete("p").unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(3)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        let run_id = RunId {
            project: "p".into(),
            mission: "mission".into(),
            generation: 1,
        };
        let ready = LaunchReady {
            session: Box::new(FakeRun {
                lines: VecDeque::new(),
                exit: None,
                stop_export_error: false,
                stop_cleanup_error: false,
                stops: backend.stops.clone(),
            }),
            mode: LaunchMode::AdoptFresh,
            notice: None,
            environment_session: None,
        };

        let error = state.apply_launch_ready(&run_id, ready).unwrap_err();
        assert!(error.to_string().contains("project p is pending deletion"));
        assert_eq!(backend.stops.load(Ordering::Relaxed), 1);
        assert!(!state.run_active());
        assert_eq!(state.run_phase(&run_id), RunPhase::Idle);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn injected_run_and_session_adapters_drive_lifecycle_without_children() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-run-seam-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        store
            .create_project("other", "Other", "cdk-regtest")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(1_700_000_123)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        let run_id = state.next_run_id("p", "mission");
        state
            .launch(
                run_id.clone(),
                "p",
                "runner",
                Some("fake/model"),
                "brief",
                None,
                None,
            )
            .unwrap();
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 1);
        assert_eq!(state.run_phase(&run_id), RunPhase::Running);
        assert_eq!(
            state
                .delete_mission("p", "mission")
                .unwrap_err()
                .to_string(),
            "store error: mission launch or teardown is still in progress"
        );
        state.delete_project("p").unwrap();
        assert!(Project::load(&state.store, "p")
            .unwrap()
            .delete_requested
            .is_some());
        assert_eq!(state.live_pty_attach().unwrap().last().unwrap(), "fake-run");

        // Presentation state cannot redirect run-owned discovery.
        state.selected_project = Some("other".into());
        state.capture_opencode_session();
        assert_eq!(
            state
                .store
                .load_mission("p", "mission")
                .unwrap()
                .opencode_session
                .as_deref(),
            Some("fake-conversation")
        );
        state.current_screen = Screen::Projects;
        state.poll_run();
        assert_eq!(state.run_phase(&run_id), RunPhase::Idle);
        assert_eq!(state.run_exit.as_ref().map(|exit| exit.code), Some(0));
        assert!(
            state.run_lines.is_empty(),
            "embedded PTY output must not be duplicated in the fallback tail"
        );

        state.refresh_live_sessions();
        assert_eq!(state.live_sessions, ["fake-run"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn installed_job_runtime_prepares_and_spawns_without_blocking_the_action() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-async-launch-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .set_project_pins(
                "p",
                BTreeMap::from([("target".into(), "project-default".into())]),
            )
            .unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("p".into());
        state.install_job_runtime(eframe::egui::Context::default());

        state.launch_mission("p", "mission").unwrap();
        let duplicate = state.launch_mission("p", "mission").unwrap_err();
        assert!(duplicate
            .to_string()
            .contains("already has a run operation"));
        assert_eq!(
            state
                .run_generations
                .get(&("p".to_string(), "mission".to_string())),
            Some(&1),
            "a duplicate click must not mint a stale run generation"
        );
        let run_id = state
            .run_phases
            .keys()
            .find(|id| id.project == "p" && id.mission == "mission")
            .cloned()
            .unwrap();
        assert_eq!(state.run_phase(&run_id), RunPhase::Preparing);
        for _ in 0..200 {
            state.poll_background_jobs();
            if state.run_phase(&run_id) == RunPhase::Running {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(state.run_phase(&run_id), RunPhase::Running);
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 1);
        assert!(state.run_belongs_to("p", "mission"));
        assert_eq!(
            store
                .load_mission("p", "mission")
                .unwrap()
                .pins
                .get("target")
                .map(String::as_str),
            Some("project-default"),
            "launch must repair an empty mission created by a stale curator MCP"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn installed_job_runtime_deletes_only_after_teardown_completes() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-async-stop-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        let run_id = state.next_run_id("p", "mission");
        state
            .launch(
                run_id.clone(),
                "p",
                "runner",
                Some("fake/model"),
                "brief",
                None,
                None,
            )
            .unwrap();
        state
            .set_tmux_session("p", "mission", Some("fake-run".into()))
            .unwrap();
        state.install_job_runtime(eframe::egui::Context::default());
        assert!(matches!(
            state.delete_mission("p", "mission").unwrap(),
            DeleteMissionResult::Scheduled
        ));
        assert!(
            store
                .load_mission("p", "mission")
                .unwrap()
                .delete_requested
                .is_some(),
            "delete intent is durable before asynchronous teardown finishes"
        );
        assert!(state.mission_delete_pending("p", "mission"));
        assert_eq!(state.run_phase(&run_id), RunPhase::Stopping);
        for _ in 0..200 {
            state.poll_background_jobs();
            if state.run_phase(&run_id) == RunPhase::Idle {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(state.run_phase(&run_id), RunPhase::Idle);
        assert!(!state.mission_delete_pending("p", "mission"));
        assert!(store.load_mission("p", "mission").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn lifecycle_records_prepare_and_spawn_failures() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-run-failure-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        let prepare_error = state.launch_mission("missing", "mission").unwrap_err();
        let prepare_id = RunId {
            project: "missing".into(),
            mission: "mission".into(),
            generation: 1,
        };
        assert!(matches!(
            state.run_phase(&prepare_id),
            RunPhase::Failed {
                at: RunPhaseKind::Preparing,
                ref message,
                recoverable: true,
                ..
            } if message == &prepare_error.to_string()
        ));

        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        backend.fail_spawn.store(true, Ordering::Relaxed);
        let start_id = state.next_run_id("p", "mission");
        let error = state
            .launch(
                start_id.clone(),
                "p",
                "runner",
                Some("fake/model"),
                "brief",
                None,
                None,
            )
            .unwrap_err();
        assert_eq!(error.to_string(), "store error: injected spawn failure");
        assert!(matches!(
            state.run_phase(&start_id),
            RunPhase::Failed {
                at: RunPhaseKind::Starting,
                recoverable: true,
                ..
            }
        ));
        assert!(!state.run_active());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_preparation_never_spawns_or_adopts() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-cancel-prepare-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        backend.cancel_during_prepare.store(true, Ordering::Relaxed);
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );

        let error = state.launch_mission("p", "mission").unwrap_err();
        assert_eq!(
            error.to_string(),
            "store error: launch preparation cancelled"
        );
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
        assert!(!state.run_active());
        let run_id = RunId {
            project: "p".into(),
            mission: "mission".into(),
            generation: 1,
        };
        assert_eq!(state.run_phase(&run_id), RunPhase::Idle);
        assert!(!state.cancel_preparation("p", "mission"));

        backend
            .cancel_during_prepare
            .store(false, Ordering::Relaxed);
        backend.cancel_before_spawn.store(true, Ordering::Relaxed);
        let error = state.launch_mission("p", "mission").unwrap_err();
        assert_eq!(error.to_string(), "store error: launch start cancelled");
        assert_eq!(backend.spawns.load(Ordering::Relaxed), 0);
        let starting_id = RunId {
            generation: 2,
            ..run_id
        };
        assert_eq!(state.run_phase(&starting_id), RunPhase::Idle);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_mission_binding_cleans_up_the_adopted_run() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-bind-failure-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        *backend.remove_mission_on_spawn.lock().unwrap() =
            Some(store.project_missions_dir("p").join("mission.md"));
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            backend,
            Arc::new(FakeSessionCatalog),
        );

        let error = state.launch_mission("p", "mission").unwrap_err();
        assert!(
            error.to_string().contains("spawned run was stopped"),
            "{error}"
        );
        assert!(error.to_string().contains("fake-transcript.log"), "{error}");
        assert!(!state.run_active());
        let run_id = RunId {
            project: "p".into(),
            mission: "mission".into(),
            generation: 1,
        };
        assert!(matches!(
            state.run_phase(&run_id),
            RunPhase::Failed {
                at: RunPhaseKind::Running,
                recoverable: false,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn asynchronous_launch_is_stopped_when_deletion_removes_its_mission() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-async-launch-delete-race-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        *backend.remove_mission_on_spawn.lock().unwrap() =
            Some(store.project_missions_dir("p").join("mission.md"));
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        state.install_job_runtime(eframe::egui::Context::default());

        state.launch_mission("p", "mission").unwrap();
        for _ in 0..200 {
            state.poll_background_jobs();
            if backend.stops.load(Ordering::Relaxed) != 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(backend.stops.load(Ordering::Relaxed), 1);
        assert!(!state.run_active());
        assert_eq!(
            state.latest_run_phase("p", "mission"),
            RunPhase::Idle,
            "successful cleanup must not leave a blocking launch phase"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn detached_stop_preserves_identity_when_cleanup_fails_then_allows_retry() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-stop-retry-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_project("other", "Other", "cdk-regtest")
            .unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        record.session = Some("fake-run".into());
        record.opencode_session = Some("fake-conversation".into());
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let backend = Arc::new(FakeRunBackend::default());
        backend.fail_export.store(true, Ordering::Relaxed);
        backend.fail_kill.store(true, Ordering::Relaxed);
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("other".into());

        let error = state.stop_mission("p", "mission").unwrap_err();
        assert!(
            error.to_string().contains("detached export failure"),
            "{error}"
        );
        assert!(
            error.to_string().contains("tmux cleanup failure"),
            "{error}"
        );
        assert_eq!(
            store
                .load_mission("p", "mission")
                .unwrap()
                .session
                .as_deref(),
            Some("fake-run"),
            "failed cleanup keeps durable retry identity"
        );
        let first = RunId {
            project: "p".into(),
            mission: "mission".into(),
            generation: 1,
        };
        assert!(matches!(
            state.run_phase(&first),
            RunPhase::Failed {
                at: RunPhaseKind::Stopping,
                cleanup_pending: true,
                ..
            }
        ));
        let delete_error = state.delete_mission("p", "mission").unwrap_err();
        assert!(
            delete_error.to_string().contains("tmux cleanup failure"),
            "{delete_error}"
        );
        assert!(store.load_mission("p", "mission").is_ok());

        backend.fail_kill.store(false, Ordering::Relaxed);
        let export_error = state.stop_mission("p", "mission").unwrap_err();
        assert!(export_error.to_string().contains("detached export failure"));
        assert_eq!(store.load_mission("p", "mission").unwrap().session, None);
        let second = RunId {
            generation: 3,
            ..first
        };
        assert!(matches!(
            state.run_phase(&second),
            RunPhase::Failed {
                at: RunPhaseKind::Exporting,
                cleanup_pending: false,
                ..
            }
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn restarted_app_recovers_a_durable_detached_session() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-restart-session-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        record.session = Some("fake-run".into());
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();

        // A new AppState owns no process handle; the durable record plus
        // session catalog is sufficient to recover attachment and status.
        let mut state = AppState::with_runtime(
            store,
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.refresh_live_sessions();
        assert!(!state.run_active());
        assert_eq!(state.live_sessions, ["fake-run"]);
        assert_eq!(
            state.mission_activity("p", "mission"),
            MissionActivity::Waiting
        );
        assert!(AppState::session_attach_command("fake-run").is_some());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn every_inflight_phase_blocks_deletion() {
        for phase in [
            RunPhase::Preparing,
            RunPhase::Starting,
            RunPhase::Running,
            RunPhase::Stopping,
            RunPhase::Exporting,
        ] {
            assert!(phase.blocks_deletion(), "{phase:?}");
        }
        assert!(!RunPhase::Idle.blocks_deletion());
        assert!(RunPhase::Idle.allows_delete_action());
        assert!(RunPhase::Running.allows_delete_action());
        for phase in [
            RunPhase::Preparing,
            RunPhase::Starting,
            RunPhase::Stopping,
            RunPhase::Exporting,
        ] {
            assert!(!phase.allows_delete_action(), "{phase:?}");
        }
        for at in [
            RunPhaseKind::Preparing,
            RunPhaseKind::Starting,
            RunPhaseKind::Running,
            RunPhaseKind::Stopping,
            RunPhaseKind::Exporting,
        ] {
            let failed = RunPhase::Failed {
                at,
                message: "visible failure".into(),
                recoverable: true,
                cleanup_pending: false,
            };
            assert!(!failed.blocks_deletion());
            assert!(failed.allows_delete_action());
        }
    }

    #[test]
    fn run_identity_generation_increments_per_project_mission() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-run-id-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let mut state = AppState::with_runtime(
            Store::new(root.clone()),
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        let first = state.next_run_id("p", "m");
        let second = state.next_run_id("p", "m");
        let other_project = state.next_run_id("q", "m");
        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert_eq!(other_project.generation, 1);
        assert_ne!(first, second);
        assert_ne!(first, other_project);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn job_scope_guard_rejects_navigation_and_generation_staleness() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-job-scope-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store.create_project("q", "Q", "cdk-regtest").unwrap();
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        state.selected_project = Some("p".into());
        let project_scope = crate::jobs::JobScope {
            project: "p".into(),
            project_generation: 0,
            corpus_revision: None,
            run_id: None,
        };
        assert!(state.job_scope_current(&project_scope));
        state.selected_project = Some("q".into());
        assert!(!state.job_scope_current(&project_scope));

        let run_id = state.next_run_id("p", "mission");
        let run_scope = crate::jobs::JobScope {
            project: "p".into(),
            project_generation: 0,
            corpus_revision: None,
            run_id: Some(run_id.clone()),
        };
        assert!(
            state.job_scope_current(&run_scope),
            "run work follows stable identity, not navigation"
        );
        state.next_run_id("p", "mission");
        assert!(!state.job_scope_current(&run_scope));

        store.wipe_project_corpus("p").unwrap();
        state.refresh();
        assert!(!state.job_scope_current(&project_scope));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn late_discovery_from_an_old_generation_is_discarded() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-late-generation-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.agent = "runner".into();
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        let old = state.next_run_id("p", "mission");
        let current = state.next_run_id("p", "mission");
        state.owned_run_id = Some(current.clone());
        state.run_phases.insert(current.clone(), RunPhase::Running);

        assert!(!state.apply_discovered_conversation(&old, "old-session".into()));
        assert_eq!(
            store.load_mission("p", "mission").unwrap().opencode_session,
            None
        );
        assert!(state.apply_discovered_conversation(&current, "current-session".into()));
        assert_eq!(
            store
                .load_mission("p", "mission")
                .unwrap()
                .opencode_session
                .as_deref(),
            Some("current-session")
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn reexport_fires_once_per_turn() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let earlier = now - Duration::from_secs(30);
        // Never exported, but the session painted output: capture it.
        assert!(should_reexport(Some(now), None));
        // Painted more recently than our last export: a new turn happened.
        assert!(should_reexport(Some(now), Some(earlier)));
        // Nothing painted since we last exported: the turn is already
        // recorded — do not re-export every beat while it sits quiet.
        assert!(!should_reexport(Some(earlier), Some(now)));
        assert!(!should_reexport(Some(now), Some(now)));
        // No activity reading at all: nothing to record.
        assert!(!should_reexport(None, None));
        assert!(!should_reexport(None, Some(earlier)));
    }

    #[test]
    fn checkpoint_export_waits_for_quiet_and_yields_to_deletion_and_backoff() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let paint = now - Duration::from_secs(5);
        let old_export = now - Duration::from_secs(30);

        assert!(checkpoint_export_due(
            false,
            MissionActivity::Waiting,
            Some(paint),
            Some(old_export),
            None,
            now,
        ));
        assert!(!checkpoint_export_due(
            false,
            MissionActivity::Working,
            Some(now),
            Some(old_export),
            None,
            now,
        ));
        assert!(!checkpoint_export_due(
            true,
            MissionActivity::Waiting,
            Some(paint),
            Some(old_export),
            None,
            now,
        ));
        assert!(!checkpoint_export_due(
            false,
            MissionActivity::Waiting,
            Some(paint),
            Some(old_export),
            Some(now + Duration::from_secs(1)),
            now,
        ));
    }

    #[test]
    fn session_operation_leases_are_shared_only_within_one_mission() {
        let leases = SessionOperationLeases::default();
        let first = leases.claim("p", "mission");
        let same = leases.claim("p", "mission");
        let other_mission = leases.claim("p", "other");
        let other_project = leases.claim("other", "mission");

        assert!(Arc::ptr_eq(&first, &same));
        assert!(!Arc::ptr_eq(&first, &other_mission));
        assert!(!Arc::ptr_eq(&first, &other_project));
    }

    #[test]
    fn checkpoint_waiting_on_a_lease_yields_to_durable_project_deletion() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-export-delete-lease-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.session = Some("fake-run".into());
        record.opencode_session = Some("fake-conversation".into());
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let clock = Arc::new(ManualClock::new(2));
        let backend = Arc::new(FakeRunBackend::default());
        let mut state = AppState::with_runtime(
            store.clone(),
            clock.clone(),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        state.refresh();
        state.live_sessions = vec!["fake-run".into()];
        state.session_activity.insert(
            "fake-run".into(),
            clock.monotonic_now()
                - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1),
        );
        let lease = state.session_operation_leases.claim("p", "mission");
        let ownership = lease.lock().unwrap();
        state.install_job_runtime(eframe::egui::Context::default());
        state.schedule_session_maintenance("p");

        store.request_project_delete("p").unwrap();
        drop(ownership);
        for _ in 0..200 {
            state.poll_background_jobs();
            if state
                .jobs
                .as_ref()
                .is_none_or(|jobs| !jobs.is_kind_active(JobKind::SessionExport))
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }

        assert_eq!(backend.exports.load(Ordering::Relaxed), 0);
        assert!(
            Project::load(&store, "p")
                .unwrap()
                .delete_requested
                .is_some()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn project_deletion_cascade_waits_for_an_inflight_checkpoint() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-export-teardown-lease-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.clone());
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut record = mission(1);
        record.session = Some("fake-run".into());
        record.opencode_session = Some("fake-conversation".into());
        store
            .write_mission("p", "mission", &record, "brief")
            .unwrap();
        let clock = Arc::new(ManualClock::new(2));
        let backend = Arc::new(FakeRunBackend::default());
        backend.block_export.store(true, Ordering::Release);
        let mut state = AppState::with_runtime(
            store.clone(),
            clock.clone(),
            backend.clone(),
            Arc::new(FakeSessionCatalog),
        );
        state.refresh();
        state.live_sessions = vec!["fake-run".into()];
        state.session_activity.insert(
            "fake-run".into(),
            clock.monotonic_now()
                - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1),
        );
        state.install_job_runtime(eframe::egui::Context::default());
        state.schedule_session_maintenance("p");
        for _ in 0..200 {
            if backend.export_in_progress.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(backend.export_in_progress.load(Ordering::Acquire));

        state.delete_project("p").unwrap();
        state.poll_launch_requests();
        for _ in 0..200 {
            state.poll_background_jobs();
            if state.mission_delete_pending("p", "mission") {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(state.mission_delete_pending("p", "mission"));
        assert_eq!(backend.kills.load(Ordering::Relaxed), 0);
        assert!(!backend.teardown_overlap.load(Ordering::Acquire));

        backend.block_export.store(false, Ordering::Release);
        for _ in 0..300 {
            state.poll_background_jobs();
            if store.load_mission("p", "mission").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(store.load_mission("p", "mission").is_err());
        assert_eq!(backend.kills.load(Ordering::Relaxed), 1);
        assert!(!backend.teardown_overlap.load(Ordering::Acquire));

        clock.advance(STORE_BACKSTOP);
        state.poll_launch_requests();
        for _ in 0..200 {
            state.poll_background_jobs();
            if Project::load(&store, "p").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(Project::load(&store, "p").is_err());
        let _ = std::fs::remove_dir_all(root);
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

    #[test]
    fn completion_delivery_groups_children_for_each_exact_curator() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-delivery-groups-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();

        let parents = [
            ("curator-a", "run-a", "ses_a", 41_001_u16),
            ("curator-b", "run-b", "ses_b", 41_002_u16),
            ("curator-stale", "old-run", "ses_stale", 41_003_u16),
        ];
        for (slug, run_id, conversation, port) in parents {
            let mut parent = mission(1);
            parent.session = Some(if slug == "curator-stale" {
                "new-run".into()
            } else {
                run_id.into()
            });
            parent.control = Some(corpus_core::MissionControl {
                run_id: run_id.into(),
                port,
            });
            parent.opencode_session = Some(conversation.into());
            store.write_mission("p", slug, &parent, "curate").unwrap();
        }

        let children = [
            ("child-a1", "curator-a", "run-a"),
            ("child-a2", "curator-a", "run-a"),
            ("child-b1", "curator-b", "run-b"),
            ("child-stale", "curator-stale", "old-run"),
        ];
        for (slug, parent_slug, parent_run) in children {
            let mut child = mission(2);
            child.dispatch = Some(corpus_core::MissionDispatch {
                parent: corpus_core::MissionRunRef {
                    project: "p".into(),
                    mission: parent_slug.into(),
                    run_id: parent_run.into(),
                },
                child_run_id: Some(format!("{slug}-run")),
                live_seen: true,
                running_seen: true,
                completion: Some(corpus_core::MissionCompletion::Completed { at: 3 }),
                delivery_attempt: 0,
                delivery_message_id: None,
                delivered: slug == "child-a1",
            });
            store.write_mission("p", slug, &child, "work").unwrap();
        }

        let service = RecordingQueueService::default();
        deliver_completed_dispatches(
            &store,
            &service,
            &["run-a".into(), "run-b".into(), "old-run".into()],
        )
        .unwrap();
        // Admission is not delivery. A later pass observes the exact curator
        // turn's successful terminal state and acknowledges it.
        deliver_completed_dispatches(
            &store,
            &service,
            &["run-a".into(), "run-b".into(), "old-run".into()],
        )
        .unwrap();
        let mut calls = service.calls.lock().unwrap().clone();
        calls.sort_by(|left, right| left.run_id.cmp(&right.run_id));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].run_id, "run-a");
        assert_eq!(calls[0].session_id, "ses_a");
        assert!(calls[0].message_id.starts_with("msg_corpus"));
        assert!(calls[0].prompt.contains("p/child-a1"));
        assert!(calls[0].prompt.contains("p/child-a2"));
        assert!(!calls[0].prompt.contains("child-b1"));
        assert_eq!(calls[1].run_id, "run-b");
        assert_ne!(calls[0].password, calls[1].password);
        assert!(calls[1].prompt.contains("p/child-b1"));
        assert!(store
            .load_mission("p", "child-a1")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered);
        assert!(!store
            .load_mission("p", "child-stale")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_queue_admission_remains_retryable_with_the_same_message_id() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-delivery-retry-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut parent = mission(1);
        parent.session = Some("run-a".into());
        parent.control = Some(corpus_core::MissionControl {
            run_id: "run-a".into(),
            port: 41_001,
        });
        parent.opencode_session = Some("ses_a".into());
        store
            .write_mission("p", "curator", &parent, "curate")
            .unwrap();
        let mut child = mission(2);
        child.dispatch = Some(corpus_core::MissionDispatch {
            parent: corpus_core::MissionRunRef {
                project: "p".into(),
                mission: "curator".into(),
                run_id: "run-a".into(),
            },
            child_run_id: Some("child-run".into()),
            live_seen: true,
            running_seen: true,
            completion: Some(corpus_core::MissionCompletion::UnexpectedExit { at: 3 }),
            delivery_attempt: 0,
            delivery_message_id: None,
            delivered: false,
        });
        store.write_mission("p", "child", &child, "work").unwrap();

        let service = RecordingQueueService::default();
        service.fail.store(true, Ordering::Relaxed);
        assert!(deliver_completed_dispatches(&store, &service, &["run-a".into()]).is_err());
        assert!(!store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered);
        service.fail.store(false, Ordering::Relaxed);
        deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
        assert!(!store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered);
        deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
        let calls = service.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].prompt.contains("exited unexpectedly"));
        assert!(store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn admitted_prompt_is_not_delivered_when_the_curator_model_fails() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-delivery-model-failure-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let mut parent = mission(1);
        parent.session = Some("run-a".into());
        parent.control = Some(corpus_core::MissionControl {
            run_id: "run-a".into(),
            port: 41_001,
        });
        parent.opencode_session = Some("ses_a".into());
        store
            .write_mission("p", "curator", &parent, "curate")
            .unwrap();
        let mut child = mission(2);
        child.dispatch = Some(corpus_core::MissionDispatch {
            parent: corpus_core::MissionRunRef {
                project: "p".into(),
                mission: "curator".into(),
                run_id: "run-a".into(),
            },
            child_run_id: Some("child-run".into()),
            live_seen: true,
            running_seen: true,
            completion: Some(corpus_core::MissionCompletion::Completed { at: 3 }),
            delivery_attempt: 0,
            delivery_message_id: None,
            delivered: false,
        });
        store.write_mission("p", "child", &child, "work").unwrap();

        let service = RecordingQueueService::default();
        *service.prompt_state.lock().unwrap() =
            PromptDeliveryState::Failed {
                error: "Model unavailable".into(),
                retry_ready: false,
            };
        deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
        let admitted = store.load_mission("p", "child").unwrap().dispatch.unwrap();
        assert_eq!(admitted.delivery_attempt, 1);
        assert!(admitted.delivery_message_id.is_some());
        assert!(!admitted.delivered);

        let error = deliver_completed_dispatches(&store, &service, &["run-a".into()])
            .unwrap_err();
        assert!(error.contains("Model unavailable"));
        assert_eq!(service.calls.lock().unwrap().len(), 1);
        let failed = store.load_mission("p", "child").unwrap().dispatch.unwrap();
        assert!(!failed.delivered);
        assert_eq!(failed.delivery_message_id, admitted.delivery_message_id);

        // Reconciliation remains observational after the failure; it does
        // not mint fresh prompt ids and spin the paid model overnight.
        assert!(deliver_completed_dispatches(&store, &service, &["run-a".into()]).is_err());
        assert_eq!(service.calls.lock().unwrap().len(), 1);

        *service.prompt_state.lock().unwrap() = PromptDeliveryState::Failed {
            error: "Model unavailable".into(),
            retry_ready: true,
        };
        assert!(deliver_completed_dispatches(&store, &service, &["run-a".into()]).is_err());
        assert!(store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivery_message_id
            .is_none());
        *service.prompt_state.lock().unwrap() = PromptDeliveryState::Acknowledged;
        deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
        assert_eq!(service.calls.lock().unwrap().len(), 2);
        let retried = store.load_mission("p", "child").unwrap().dispatch.unwrap();
        assert_eq!(retried.delivery_attempt, 2);
        assert_ne!(retried.delivery_message_id, admitted.delivery_message_id);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn failed_environment_survives_restart_and_blocks_relaunch_and_delete() {
        let root = std::env::temp_dir().join(format!(
            "corpus-app-environment-recovery-{}-{}",
            std::process::id(),
            new_uuid_id()
        ));
        let store = Store::new(root.join("store"));
        store.create_project("p", "P", "cdk-regtest").unwrap();
        store
            .create_agent_with_role("p", "operator", corpus_core::AgentRole::Tester)
            .unwrap();
        let id = RunId {
            project: "p".into(),
            mission: "mission".into(),
            generation: 1,
        };
        let mut mission = mission(1);
        mission.environment_session = Some(id.storage_key());
        store
            .write_mission("p", "mission", &mission, "brief")
            .unwrap();
        let mut environment = corpus_core::EnvironmentSessionRecord {
            id,
            plugin_id: "cdk-regtest".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: "fixture".into(),
            state: corpus_core::EnvironmentSessionState::Failed,
            source_shas: Default::default(),
            environment_lock: None,
            image_digest: None,
            created: 1,
            updated: 2,
            error: Some("cleanup failed".into()),
        };
        store.save_environment_session(&environment).unwrap();

        let mut state = AppState::with_runtime(
            store.clone(),
            Arc::new(ManualClock::new(0)),
            Arc::new(FakeRunBackend::default()),
            Arc::new(FakeSessionCatalog),
        );
        assert!(state.mission_environment_needs_cleanup("p", "mission"));
        assert!(state
            .refuse_duplicate_mission_run("p", "mission")
            .unwrap_err()
            .to_string()
            .contains("requiring cleanup"));
        let cleanup_error = state.delete_mission("p", "mission").unwrap_err();
        assert!(
            cleanup_error.to_string().contains("cleanup_failed"),
            "{cleanup_error}"
        );
        assert!(store.load_mission("p", "mission").is_ok());

        environment.state = corpus_core::EnvironmentSessionState::Closed;
        store.save_environment_session(&environment).unwrap();
        assert!(!state.mission_environment_needs_cleanup("p", "mission"));
        state.delete_mission("p", "mission").unwrap();
        let _ = std::fs::remove_dir_all(root);
    }
}
