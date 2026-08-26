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
    status_hook: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl Default for RecordingQueueService {
    fn default() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: AtomicBool::new(false),
            active: AtomicBool::new(false),
            prompt_state: Mutex::new(PromptDeliveryState::Acknowledged),
            status_hook: Mutex::new(None),
        }
    }
}

impl SessionService for RecordingQueueService {
    fn health(&self) -> Result<crate::session_service::ServiceHealth, String> {
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
        if let Some(hook) = self.status_hook.lock().unwrap().take() {
            hook();
        }
        Ok(self.prompt_state.lock().unwrap().clone())
    }
}

struct BlockingExportService {
    block: AtomicBool,
    in_progress: AtomicBool,
}

impl SessionService for BlockingExportService {
    fn health(&self) -> Result<crate::session_service::ServiceHealth, String> {
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

    fn usage_snapshot(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        session: &SessionRef,
    ) -> Result<corpus_core::UsageSnapshot, String> {
        self.in_progress.store(true, Ordering::Release);
        while self.block.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.in_progress.store(false, Ordering::Release);
        Ok(corpus_core::UsageSnapshot {
            version: corpus_core::USAGE_SNAPSHOT_VERSION,
            session_id: session.id.clone(),
            captured_at: 1,
            source: "test".into(),
            rows: Vec::new(),
        })
    }

    fn queue_prompt(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
        _prompt: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    fn session_turn_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _launched_at_ms: u64,
    ) -> Result<SessionTurnState, String> {
        Ok(SessionTurnState::Completed)
    }

    fn prompt_delivery_state(
        &self,
        _control: &corpus_core::MissionControl,
        _password: &str,
        _session: &SessionRef,
        _message_id: &str,
    ) -> Result<PromptDeliveryState, String> {
        Ok(PromptDeliveryState::Acknowledged)
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

    fn export_session(&self, _project: &str, _opencode_session_id: &str) -> Result<PathBuf, Error> {
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

struct CountingSessionCatalog(Arc<AtomicUsize>);

impl SessionCatalog for CountingSessionCatalog {
    fn live_tui_sessions(&self) -> Vec<String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        vec!["fake-run".into()]
    }

    fn raw_log(&self, _store: &Store, _project: &str, _session: &str) -> Option<PathBuf> {
        None
    }
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

mod corpus;
mod delivery;
mod dispatch_requests;
mod lifecycle;
mod maintenance;
mod resources;
mod session;
