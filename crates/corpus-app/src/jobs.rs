//! App-owned background-job contract.
//!
//! Each submitted job gets an independent coordinator thread. The coordinator
//! races worker completion against cancellation and a deadline, publishes
//! exactly one terminal result, then wakes egui. A worker that cannot be
//! interrupted may finish later, but its private channel has no path back into
//! app state after the coordinator has published cancellation/timeout.

use std::collections::BTreeMap;
use std::io;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::state::RunId;

pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

pub(crate) trait JobSpawner: Send + Sync {
    fn spawn(&self, name: &str, job: Job) -> io::Result<()>;
}

pub(crate) struct ThreadJobSpawner;

impl JobSpawner for ThreadJobSpawner {
    fn spawn(&self, name: &str, job: Job) -> io::Result<()> {
        std::thread::Builder::new()
            .name(name.to_string())
            .spawn(job)
            .map(|_| ())
    }
}

pub(crate) trait RepaintWake: Send + Sync {
    fn request_repaint(&self);
}

impl RepaintWake for eframe::egui::Context {
    fn request_repaint(&self) {
        eframe::egui::Context::request_repaint(self);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct JobId(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum JobKind {
    PluginProbe,
    PluginInstall,
    PluginSetup,
    PluginDoctor,
    PluginStop,
    SourceRevisions,
    ModelDiscovery,
    LaunchPreparation,
    LaunchRequests,
    SessionDiscovery,
    SessionExport,
    DispatchDelivery,
    SessionTeardown,
    OrphanCleanup,
    ProjectScope,
    ProjectAgents,
    ProjectMissions,
    CorpusSummary,
    CorpusCost,
}

impl JobKind {
    fn thread_name(self) -> &'static str {
        match self {
            Self::PluginProbe => "corpus-plugin-probe",
            Self::PluginInstall => "corpus-plugin-install",
            Self::PluginSetup => "corpus-plugin-setup",
            Self::PluginDoctor => "corpus-plugin-doctor",
            Self::PluginStop => "corpus-plugin-stop",
            Self::SourceRevisions => "corpus-source-revisions",
            Self::ModelDiscovery => "corpus-model-discovery",
            Self::LaunchPreparation => "corpus-launch-prepare",
            Self::LaunchRequests => "corpus-launch-requests",
            Self::SessionDiscovery => "corpus-session-discovery",
            Self::SessionExport => "corpus-session-export",
            Self::DispatchDelivery => "corpus-dispatch-delivery",
            Self::SessionTeardown => "corpus-session-teardown",
            Self::OrphanCleanup => "corpus-orphan-cleanup",
            Self::ProjectScope => "corpus-project-scope",
            Self::ProjectAgents => "corpus-project-agents",
            Self::ProjectMissions => "corpus-project-missions",
            Self::CorpusSummary => "corpus-summary",
            Self::CorpusCost => "corpus-cost",
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::PluginProbe => "plugin probe",
            Self::PluginInstall => "plugin install",
            Self::PluginSetup => "plugin setup",
            Self::PluginDoctor => "plugin doctor",
            Self::PluginStop => "plugin stop",
            Self::SourceRevisions => "source revision refresh",
            Self::ModelDiscovery => "model discovery",
            Self::LaunchPreparation => "launch preparation",
            Self::LaunchRequests => "launch request scan",
            Self::SessionDiscovery => "session discovery",
            Self::SessionExport => "session export",
            Self::DispatchDelivery => "mission completion delivery",
            Self::SessionTeardown => "session teardown",
            Self::OrphanCleanup => "orphan environment cleanup",
            Self::ProjectScope => "project scope refresh",
            Self::ProjectAgents => "agent list refresh",
            Self::ProjectMissions => "mission list refresh",
            Self::CorpusSummary => "corpus summary refresh",
            Self::CorpusCost => "corpus cost refresh",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct JobScope {
    pub project: String,
    pub project_generation: u64,
    /// Corpus invalidation revision captured when a corpus-reading job starts.
    /// It is deliberately absent from `JobKey`: dirty-during-flight work is
    /// coalesced behind the active walk, then rescheduled when that key clears.
    pub corpus_revision: Option<u64>,
    pub run_id: Option<RunId>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct JobKey {
    kind: JobKind,
    project: String,
    project_generation: u64,
    run_id: Option<RunId>,
}

#[derive(Clone, Default)]
pub(crate) struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub(crate) fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Debug)]
pub(crate) enum JobTerminal<T> {
    Success(T),
    Failure(String),
    Cancelled,
    TimedOut,
}

#[derive(Debug)]
pub(crate) struct JobResult<T> {
    pub id: JobId,
    pub kind: JobKind,
    pub scope: JobScope,
    pub terminal: JobTerminal<T>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartOutcome {
    Started(JobId),
    Duplicate(JobId),
}

struct ActiveJob {
    id: JobId,
    cancellation: CancellationToken,
}

/// Main-thread-owned job registry and result inbox.
pub(crate) struct JobSet<T> {
    spawner: Arc<dyn JobSpawner>,
    wake: Arc<dyn RepaintWake>,
    sent: mpsc::Sender<JobResult<T>>,
    received: mpsc::Receiver<JobResult<T>>,
    active: BTreeMap<JobKey, ActiveJob>,
    next_id: u64,
}

impl<T: Send + 'static> JobSet<T> {
    pub(crate) fn new(wake: Arc<dyn RepaintWake>) -> Self {
        Self::with_spawner(Arc::new(ThreadJobSpawner), wake)
    }

    fn with_spawner(spawner: Arc<dyn JobSpawner>, wake: Arc<dyn RepaintWake>) -> Self {
        let (sent, received) = mpsc::channel();
        Self {
            spawner,
            wake,
            sent,
            received,
            active: BTreeMap::new(),
            next_id: 1,
        }
    }

    pub(crate) fn start<F>(
        &mut self,
        kind: JobKind,
        scope: JobScope,
        timeout: Duration,
        work: F,
    ) -> StartOutcome
    where
        F: FnOnce(CancellationToken) -> Result<T, String> + Send + 'static,
    {
        let key = JobKey {
            kind,
            project: scope.project.clone(),
            project_generation: scope.project_generation,
            run_id: scope.run_id.clone(),
        };
        if let Some(active) = self.active.get(&key) {
            return StartOutcome::Duplicate(active.id);
        }

        let id = JobId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        let cancellation = CancellationToken::default();
        self.active.insert(
            key,
            ActiveJob {
                id,
                cancellation: cancellation.clone(),
            },
        );

        let result_sender = self.sent.clone();
        let result_scope = scope.clone();
        let wake = self.wake.clone();
        let coordinator_cancel = cancellation.clone();
        let coordinator = Box::new(move || {
            let (worker_sent, worker_received) = mpsc::sync_channel(1);
            let worker_cancel = coordinator_cancel.clone();
            let worker_spawn = std::thread::Builder::new()
                .name(format!("{}-worker", kind.thread_name()))
                .spawn(move || {
                    let result = panic::catch_unwind(AssertUnwindSafe(|| work(worker_cancel)))
                        .map_err(panic_message)
                        .and_then(|result| result);
                    let _ = worker_sent.send(result);
                });

            let terminal = match worker_spawn {
                Err(error) => JobTerminal::Failure(format!("worker spawn failed: {error}")),
                Ok(_) => await_terminal(worker_received, &coordinator_cancel, timeout),
            };
            publish(
                &result_sender,
                &*wake,
                JobResult {
                    id,
                    kind,
                    scope: result_scope,
                    terminal,
                },
            );
        });

        if let Err(error) = self.spawner.spawn(kind.thread_name(), coordinator) {
            publish(
                &self.sent,
                &*self.wake,
                JobResult {
                    id,
                    kind,
                    scope,
                    terminal: JobTerminal::Failure(format!("job spawn failed: {error}")),
                },
            );
        }
        StartOutcome::Started(id)
    }

    pub(crate) fn cancel_scope(&self, kind: JobKind, run_id: &RunId) -> bool {
        self.active
            .iter()
            .find(|(key, _)| key.kind == kind && key.run_id.as_ref() == Some(run_id))
            .is_some_and(|(_, active)| {
                active.cancellation.cancel();
                true
            })
    }

    /// Cancel every active job of one operator operation kind. Plugin
    /// lifecycle work is installation-scoped rather than run-scoped, so it
    /// cannot use `cancel_scope`'s `RunId` key.
    pub(crate) fn cancel_kind(&self, kind: JobKind) -> usize {
        self.active
            .iter()
            .filter(|(key, _)| key.kind == kind)
            .map(|(_, active)| {
                active.cancellation.cancel();
                1
            })
            .sum()
    }

    pub(crate) fn is_kind_active(&self, kind: JobKind) -> bool {
        self.active.keys().any(|key| key.kind == kind)
    }

    /// Drain terminal results, removing their in-flight guards. Results for
    /// stale scopes are consumed but never returned to the state mutator.
    pub(crate) fn drain_applicable(
        &mut self,
        mut scope_is_current: impl FnMut(&JobScope) -> bool,
    ) -> Vec<JobResult<T>> {
        let mut applicable = Vec::new();
        while let Ok(result) = self.received.try_recv() {
            let key = JobKey {
                kind: result.kind,
                project: result.scope.project.clone(),
                project_generation: result.scope.project_generation,
                run_id: result.scope.run_id.clone(),
            };
            if self
                .active
                .get(&key)
                .is_some_and(|active| active.id == result.id)
            {
                self.active.remove(&key);
                if scope_is_current(&result.scope) {
                    applicable.push(result);
                }
            }
        }
        applicable
    }
}

fn await_terminal<T>(
    received: mpsc::Receiver<Result<T, String>>,
    cancellation: &CancellationToken,
    timeout: Duration,
) -> JobTerminal<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if cancellation.is_cancelled() {
            return JobTerminal::Cancelled;
        }
        let now = Instant::now();
        if now >= deadline {
            // Cooperative workers see the deadline too. In particular,
            // launch preparation checks this token before process spawn, so
            // a timed-out network fetch cannot later create an orphan run.
            cancellation.cancel();
            return JobTerminal::TimedOut;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(10));
        match received.recv_timeout(wait) {
            Ok(Ok(value)) => return JobTerminal::Success(value),
            Ok(Err(error)) => return JobTerminal::Failure(error),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return JobTerminal::Failure("worker ended without a result".into());
            }
        }
    }
}

fn publish<T>(sent: &mpsc::Sender<JobResult<T>>, wake: &dyn RepaintWake, result: JobResult<T>) {
    if sent.send(result).is_ok() {
        // Ordering is deliberate: a repaint must never beat the terminal
        // result into the UI inbox and leave it waiting for an ambient tick.
        wake.request_repaint();
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    let detail = payload
        .downcast_ref::<&str>()
        .map(|message| (*message).to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown panic payload".into());
    format!("job panicked: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[derive(Default)]
    struct CountingWake(AtomicUsize);

    impl RepaintWake for CountingWake {
        fn request_repaint(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    struct RefusingSpawner;

    impl JobSpawner for RefusingSpawner {
        fn spawn(&self, _name: &str, _job: Job) -> io::Result<()> {
            Err(io::Error::other("injected spawn failure"))
        }
    }

    fn scope(generation: u64) -> JobScope {
        JobScope {
            project: "p".into(),
            project_generation: generation,
            corpus_revision: None,
            run_id: None,
        }
    }

    fn wait_for_results<T: Send + 'static>(jobs: &mut JobSet<T>) -> Vec<JobResult<T>> {
        for _ in 0..100 {
            let results = jobs.drain_applicable(|_| true);
            if !results.is_empty() {
                return results;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("job did not publish a terminal result");
    }

    #[test]
    fn success_wakes_only_after_the_result_is_sent() {
        let wake = Arc::new(CountingWake::default());
        let mut jobs = JobSet::new(wake.clone());
        jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| Ok(7),
        );
        for _ in 0..1_000 {
            if wake.0.load(Ordering::SeqCst) > 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(wake.0.load(Ordering::SeqCst), 1, "job must wake the UI");
        let results = jobs.drain_applicable(|_| true);
        assert_eq!(results.len(), 1, "wake observed only after inbox delivery");
        assert!(matches!(results[0].terminal, JobTerminal::Success(7)));
    }

    #[test]
    fn duplicate_is_suppressed_until_the_first_terminal_is_drained() {
        let mut jobs = JobSet::new(Arc::new(CountingWake::default()));
        let first = jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| Ok(()),
        );
        let duplicate = jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| Ok(()),
        );
        let StartOutcome::Started(id) = first else {
            panic!("first must start")
        };
        assert_eq!(duplicate, StartOutcome::Duplicate(id));
        wait_for_results(&mut jobs);
        assert!(matches!(
            jobs.start(
                JobKind::PluginProbe,
                scope(1),
                Duration::from_secs(1),
                |_| Ok(())
            ),
            StartOutcome::Started(_)
        ));
    }

    #[test]
    fn stale_generation_does_not_block_replacement_work() {
        let mut jobs = JobSet::new(Arc::new(CountingWake::default()));
        jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| {
                std::thread::sleep(Duration::from_millis(30));
                Ok(())
            },
        );
        assert!(matches!(
            jobs.start(
                JobKind::PluginProbe,
                scope(2),
                Duration::from_secs(1),
                |_| Ok(())
            ),
            StartOutcome::Started(_)
        ));
    }

    #[test]
    fn stale_generation_is_consumed_without_becoming_applicable() {
        let mut jobs = JobSet::new(Arc::new(CountingWake::default()));
        jobs.start(
            JobKind::SourceRevisions,
            scope(1),
            Duration::from_secs(1),
            |_| Ok(9),
        );
        for _ in 0..100 {
            let results = jobs.drain_applicable(|scope| scope.project_generation == 2);
            assert!(results.is_empty());
            if jobs.active.is_empty() {
                return;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        panic!("stale result was not consumed");
    }

    #[test]
    fn panic_is_a_visible_failure() {
        let mut jobs = JobSet::<()>::new(Arc::new(CountingWake::default()));
        jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| panic!("injected panic"),
        );
        let results = wait_for_results(&mut jobs);
        assert!(
            matches!(&results[0].terminal, JobTerminal::Failure(error) if error.contains("injected panic"))
        );
    }

    #[test]
    fn deadline_publishes_timeout_without_waiting_for_worker() {
        let mut jobs = JobSet::new(Arc::new(CountingWake::default()));
        jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_millis(5),
            |_| {
                std::thread::sleep(Duration::from_secs(1));
                Ok(())
            },
        );
        let results = wait_for_results(&mut jobs);
        assert!(matches!(results[0].terminal, JobTerminal::TimedOut));
    }

    #[test]
    fn cancellation_publishes_once_and_late_success_is_inert() {
        let wake = Arc::new(CountingWake::default());
        let mut jobs = JobSet::new(wake.clone());
        let run_id = RunId {
            project: "p".into(),
            mission: "m".into(),
            generation: 1,
        };
        let mut run_scope = scope(1);
        run_scope.run_id = Some(run_id.clone());
        let StartOutcome::Started(_) = jobs.start(
            JobKind::PluginProbe,
            run_scope,
            Duration::from_secs(1),
            |_| {
                std::thread::sleep(Duration::from_millis(40));
                Ok(())
            },
        ) else {
            panic!("job must start")
        };
        assert!(jobs.cancel_scope(JobKind::PluginProbe, &run_id));
        let results = wait_for_results(&mut jobs);
        assert!(matches!(results[0].terminal, JobTerminal::Cancelled));
        std::thread::sleep(Duration::from_millis(60));
        assert!(jobs.drain_applicable(|_| true).is_empty());
        assert_eq!(wake.0.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn installation_scoped_jobs_cancel_by_kind() {
        let mut jobs = JobSet::new(Arc::new(CountingWake::default()));
        jobs.start(
            JobKind::PluginSetup,
            scope(1),
            Duration::from_secs(1),
            |_| {
                std::thread::sleep(Duration::from_millis(40));
                Ok(())
            },
        );
        assert_eq!(jobs.cancel_kind(JobKind::PluginSetup), 1);
        let results = wait_for_results(&mut jobs);
        assert!(matches!(results[0].terminal, JobTerminal::Cancelled));
    }

    #[test]
    fn coordinator_spawn_failure_is_terminal_and_wakes() {
        let wake = Arc::new(CountingWake::default());
        let mut jobs = JobSet::<()>::with_spawner(Arc::new(RefusingSpawner), wake.clone());
        jobs.start(
            JobKind::PluginProbe,
            scope(1),
            Duration::from_secs(1),
            |_| Ok(()),
        );
        assert_eq!(wake.0.load(Ordering::SeqCst), 1);
        let results = jobs.drain_applicable(|_| true);
        assert!(
            matches!(&results[0].terminal, JobTerminal::Failure(error) if error.contains("injected spawn failure"))
        );
    }
}
