//! The app's thin state layer.
//!
//! House rule: widgets never touch the filesystem
//! or the corpus-core store API directly — every corpus-core call goes
//! through `AppState`, and widgets only render state and request actions.
//! Business logic (validation, store plumbing) lives here or in corpus-core,
//! never in a view.

use std::collections::{hash_map::RandomState, BTreeMap, BTreeSet};
use std::hash::{BuildHasher, Hasher};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use corpus_core::{
    AgentConfig, CorpusStats, CostReport, FindingCard, FindingIndexCache, Mission, PluginStatus,
    Project, RunLine, SourceRevs, Store,
};
#[cfg(test)]
use corpus_core::{Error, MissionDeleteRequest, StopOutcome};

use crate::file_watch::FileInvalidationSource;
#[cfg(test)]
use crate::jobs::{JobKind, JobTerminal};
use crate::jobs::{JobScope, JobSet};
use crate::nav::Screen;
use crate::session_service::{
    launch_stamp_ms, mission_session_ref, mission_workspace_candidates, mission_workspace_dir,
    ConfiguredSessionService, OpenCodeSessionStatus, PromptDeliveryState, SessionRef,
    SessionService, SessionTurnState,
};

mod models;
pub use models::*;
mod background;
mod corpus;
mod dispatch;
mod plugin;
mod resources;
mod run;
mod session;
#[cfg(test)]
use background::{
    discovered_identity_is_current, merge_plugin_statuses, successful_job_resolves_notice,
};
#[cfg(test)]
use dispatch::{deliver_completed_dispatches, reconcile_dispatch_activity};
use dispatch::{AgentDeletionRequest, DeletionRequest, LaunchRequest};
#[cfg(test)]
use plugin::prepared_plugin_leases;
use plugin::{orphan_environment_sessions, plugin_recovery_hint};
pub use resources::{agent_label, mission_label};
#[cfg(test)]
use resources::{historical_agent_label, is_uuid_like, sort_missions};
#[cfg(test)]
use run::LaunchMode;
#[cfg(test)]
use run::NoopEnvironmentRuntime;
use run::{
    load_launchable_mission, ActiveRun, CoreEnvironmentRuntime, CoreRunBackend, EnvironmentRuntime,
    LaunchReady, RunBackend, RunCancellation, TeardownReady,
};
#[cfg(test)]
use session::{activity_for, checkpoint_export_due, mission_display_state_from, should_reexport};
use session::{CoreSessionCatalog, SessionCatalog, SessionMaintenance};

/// How often the raw captures are re-stat'd. Cheap next to the tmux
/// listing (no subprocess), so it runs on the faster beat.
const ACTIVITY_EVENT_MIN: Duration = Duration::from_millis(100);
/// Notifications are hints. These slower timers reconcile startup, dropped
/// events, watcher failure, and changes made on filesystems without native
/// notification support.
const ACTIVITY_BACKSTOP: Duration = Duration::from_secs(10);
const SESSION_STATUS_BUSY_POLL: Duration = Duration::from_millis(500);
const SESSION_STATUS_IDLE_POLL: Duration = Duration::from_secs(2);
const SESSION_STATUS_GRACE: Duration = Duration::from_secs(5);
/// Native mission/run events make liveness refresh immediately. This slow
/// subprocess backstop covers startup, watcher failure, and external tmux
/// exits that do not touch the project tree.
const LIVE_SESSION_BACKSTOP: Duration = Duration::from_secs(10);
/// Expensive session reconciliation (dispatch HTTP/store walks plus optional
/// transcript maintenance) is event-driven with this missed-event backstop.
/// It must not inherit the two-second tmux liveness-list cadence.
const SESSION_RECONCILE_BACKSTOP: Duration = Duration::from_secs(60);
/// Native filesystem notifications are the primary invalidation path. When
/// the watcher is healthy, this timer is only a dropped-event audit and can
/// stay deliberately slow. If watcher installation failed, retain the old
/// cadence so curator launch/delete requests remain responsive.
const WATCHED_STORE_BACKSTOP: Duration = Duration::from_secs(60);
const UNWATCHED_STORE_BACKSTOP: Duration = Duration::from_secs(10);
/// PTY output wakes egui directly. This clock exists only to notice a quiet
/// owned process exiting and to age its activity projection; it is not a
/// terminal animation cadence.
const OWNED_RUN_REPAINT_BACKSTOP: Duration = Duration::from_secs(1);
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
    /// Monotonic identity for full project-index requests. Requests made
    /// during an active scan coalesce behind it; only the latest revision may
    /// replace the render cache.
    project_index_revision: u64,
    project_index_active_revision: Option<u64>,
    pending_background_notices: Vec<BackgroundNotice>,
    /// The screen the sidebar selection points at (Projects / Agents /
    /// Missions). Live runs remain inside the Missions screen.
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
    /// Live Corpus tmux sessions seen at the last `refresh_live_sessions`;
    /// a relaunched app offers these for reattachment.
    pub live_sessions: Vec<String>,
    /// When `live_sessions` was last polled (polled on a throttle, never
    /// per frame — the poll spawns `tmux list-sessions`).
    live_sessions_polled_at: Option<std::time::Instant>,
    live_sessions_dirty: bool,
    /// Expensive dispatch/export reconciliation has its own event-driven
    /// cadence and must not run after every cheap liveness listing.
    session_reconciled_at: Option<std::time::Instant>,
    /// Raw output events move this deadline forward. Reconciliation happens
    /// once after output settles instead of once per filesystem write.
    session_reconcile_due_at: Option<std::time::Instant>,
    pub session_lifecycle_stats: SessionLifecycleStats,
    /// Per tmux session, the moment its TUI last painted anything —
    /// derived from the run's raw capture mtime and aged forward between
    /// polls, so it stays honest without re-statting every frame. This is
    /// what separates a WORKING agent from one parked at its prompt.
    session_activity: BTreeMap<String, std::time::Instant>,
    /// When `session_activity` was last refreshed (a `stat` per live
    /// session — cheap, so polled faster than the tmux listing).
    session_activity_polled_at: Option<std::time::Instant>,
    session_activity_dirty: bool,
    /// Authoritative process-local OpenCode execution state for controlled
    /// missions. Raw capture age remains only as a legacy fallback.
    session_statuses: BTreeMap<(String, String), MissionStatusObservation>,
    session_status_polled_at: Option<std::time::Instant>,
    /// Per tmux session, the moment we last re-exported its usage transcript.
    /// The turn-completion sweep exports only when the session last painted
    /// (its `session_activity` instant) is NEWER than this — so a finished
    /// turn records exactly once, and a session parked quiet at its prompt
    /// is not re-exported every beat.
    last_exported_at: BTreeMap<String, std::time::Instant>,
    /// Failed usage checkpoints are retried on a bounded cadence rather than
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
    SessionStatuses(Vec<SessionStatusUpdate>),
    SessionMaintenance(SessionMaintenance),
    DispatchDeliveries,
    TeardownReady(TeardownReady),
    OrphanCleanup {
        project: String,
        plugin: String,
    },
    ProjectScope(ProjectScopeSnapshot),
    LaunchRequests {
        launches: Vec<LaunchRequest>,
        deletions: Vec<DeletionRequest>,
        agent_deletions: Vec<AgentDeletionRequest>,
        project_deletions: Vec<String>,
    },
    ProjectIndex {
        revision: u64,
        projects: Vec<(String, Project)>,
        trees: BTreeMap<String, ProjectTree>,
    },
    Agents(Vec<(String, AgentConfig)>),
    Missions(Vec<(String, Mission)>),
}

#[derive(Debug)]
struct SessionStatusUpdate {
    mission: String,
    run_id: String,
    result: Result<OpenCodeSessionStatus, String>,
}

#[derive(Debug, Clone)]
struct MissionStatusObservation {
    run_id: String,
    status: Option<OpenCodeSessionStatus>,
    observed_at: std::time::Instant,
    failed_at: Option<std::time::Instant>,
}

struct PluginLifecycleResult {
    plugin: String,
    operation: &'static str,
    phases: Vec<String>,
    result: serde_json::Value,
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

struct ProjectScopeSnapshot {
    agents: Vec<(String, AgentConfig)>,
    missions: Vec<(String, Mission)>,
    stats: CorpusStats,
    logs: Vec<corpus_core::MissionLog>,
    findings: FindingSnapshot,
}

/// Shared activity vocabulary. Controlled app missions refine it with their
/// owning OpenCode server's status; corpus-core's capture-age rule remains
/// the fallback for headless/legacy observers without that control endpoint.
pub use corpus_core::MissionActivity;

/// Reload the complete durable launch ancestry. Parent deletion requests do
/// not immediately stamp every child mission, so checking only the mission
/// leaves a reconciliation window in which a run could still be spawned or
/// adopted into a deleting agent/project.
/// One mission may have discovery, checkpointing, Stop, and Delete requests
/// arrive on different background-job keys. The lease is the authoritative
/// writer boundary: checkpoint export and teardown for the same mission can
/// never overlap, while unrelated missions retain full concurrency.
type SessionLeaseKey = (String, String);
type SessionLeaseMap = BTreeMap<SessionLeaseKey, Weak<Mutex<()>>>;

#[derive(Clone, Default)]
struct SessionOperationLeases(Arc<Mutex<SessionLeaseMap>>);

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

impl AppState {
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

    /// Production coordinator without an egui job runtime. System tests use
    /// this to drive launch, liveness, completion, and delivery synchronously
    /// against an isolated store.
    pub fn from_store_headless(store: Store) -> Self {
        Self::with_runtime_inner(
            store,
            Arc::new(SystemClock),
            Arc::new(CoreRunBackend),
            Arc::new(CoreSessionCatalog),
            Arc::new(CoreEnvironmentRuntime),
            Arc::new(ConfiguredSessionService::from_env()),
            true,
        )
    }

    pub fn store(&self) -> &Store {
        &self.store
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
            project_index_revision: 0,
            project_index_active_revision: None,
            pending_background_notices: Vec::new(),
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
            live_sessions_dirty: true,
            session_reconciled_at: None,
            session_reconcile_due_at: None,
            session_lifecycle_stats: SessionLifecycleStats::default(),
            session_activity: BTreeMap::new(),
            session_activity_polled_at: None,
            session_activity_dirty: false,
            session_statuses: BTreeMap::new(),
            session_status_polled_at: None,
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
}

fn global_job_scope() -> JobScope {
    JobScope {
        project: String::new(),
        project_generation: 0,
        corpus_revision: None,
        run_id: None,
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
mod tests;
