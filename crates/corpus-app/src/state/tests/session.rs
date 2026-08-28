use super::*;

#[test]
fn ansi_is_removed_once_at_ingest_and_tail_bound_is_finite() {
    assert_eq!(strip_ansi("plain \u{1b}[31mred\u{1b}[0m"), "plain red");
    assert_eq!(MAX_RUN_LINES, 4_000);
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
fn mission_display_state_has_stable_precedence() {
    assert_eq!(
        mission_display_state_from(MissionActivity::Idle, &RunPhase::Idle, false, false,),
        MissionDisplayState::Idle
    );
    assert_eq!(
        mission_display_state_from(MissionActivity::Idle, &RunPhase::Idle, true, false,),
        MissionDisplayState::Queued
    );
    assert_eq!(
        mission_display_state_from(MissionActivity::Waiting, &RunPhase::Idle, true, false,),
        MissionDisplayState::Waiting,
        "a stale queued flag must not hide a live session"
    );
    assert_eq!(
        mission_display_state_from(MissionActivity::Working, &RunPhase::Exporting, false, false,),
        MissionDisplayState::Exporting
    );
    let failed = RunPhase::Failed {
        at: RunPhaseKind::Starting,
        message: "boom".into(),
        recoverable: true,
        cleanup_pending: false,
    };
    assert_eq!(
        mission_display_state_from(MissionActivity::Idle, &failed, false, false),
        MissionDisplayState::Failed
    );
    assert_eq!(
        mission_display_state_from(MissionActivity::Working, &failed, false, true),
        MissionDisplayState::Deleting,
        "durable deletion owns the visible state"
    );
}

#[test]
fn controlled_status_stays_working_through_quiet_tools_and_exposes_retry_failure() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-controlled-status-{}-{}",
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
    record.session = Some("controlled-run".into());
    record.control = Some(corpus_core::MissionControl {
        run_id: "controlled-run".into(),
        port: 4096,
    });
    record.opencode_session = Some("ses_controlled".into());
    record.opencode_workspace = Some("workspace".into());
    state.trees.insert(
        "p".into(),
        ProjectTree {
            agents: Vec::new(),
            missions: vec![("mission".into(), record)],
        },
    );
    state.live_sessions.push("controlled-run".into());
    state.apply_session_status_updates(
        "p",
        vec![SessionStatusUpdate {
            mission: "mission".into(),
            run_id: "stale-run".into(),
            result: Ok(OpenCodeSessionStatus::Busy),
        }],
    );
    assert!(state.session_statuses.is_empty(), "late run was rejected");
    state.apply_session_status_updates(
        "p",
        vec![SessionStatusUpdate {
            mission: "mission".into(),
            run_id: "controlled-run".into(),
            result: Ok(OpenCodeSessionStatus::Busy),
        }],
    );

    clock.advance(Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1));
    assert_eq!(
        state.mission_activity("p", "mission"),
        MissionActivity::Working
    );
    assert_eq!(
        state.mission_display_state("p", "mission"),
        MissionDisplayState::Working,
        "terminal silence must not dim an OpenCode-busy mission"
    );

    state.apply_session_status_updates(
        "p",
        vec![SessionStatusUpdate {
            mission: "mission".into(),
            run_id: "controlled-run".into(),
            result: Ok(OpenCodeSessionStatus::Retrying {
                attempt: 2,
                message: "rate limited".into(),
                next_at: 1_700_000_130,
            }),
        }],
    );
    assert_eq!(
        state.mission_display_state("p", "mission"),
        MissionDisplayState::Retrying
    );

    state.apply_session_status_updates(
        "p",
        vec![SessionStatusUpdate {
            mission: "mission".into(),
            run_id: "controlled-run".into(),
            result: Ok(OpenCodeSessionStatus::Idle),
        }],
    );
    assert_eq!(
        state.mission_display_state("p", "mission"),
        MissionDisplayState::Waiting
    );

    state.apply_session_status_updates(
        "p",
        vec![SessionStatusUpdate {
            mission: "mission".into(),
            run_id: "controlled-run".into(),
            result: Err("injected timeout".into()),
        }],
    );
    assert_eq!(
        state.mission_display_state("p", "mission"),
        MissionDisplayState::Waiting,
        "a transient failure retains the last observation during the grace period"
    );
    clock.advance(SESSION_STATUS_GRACE + Duration::from_millis(1));
    assert_eq!(
        state.mission_display_state("p", "mission"),
        MissionDisplayState::Unavailable
    );
    assert_eq!(
        state.mission_activity("p", "mission"),
        MissionActivity::Working,
        "unknown status must not trigger turn-completion work"
    );
    let _ = std::fs::remove_dir_all(root);
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
    assert_eq!(state.live_repaint_after(), Some(OWNED_RUN_REPAINT_BACKSTOP));

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

    // Once this process owns the same working run, PTY output remains
    // the prompt repaint source. The clock is only the quiet exit audit.
    state.owned_run_id = Some(RunId {
        project: "p".into(),
        mission: "mission".into(),
        generation: 1,
    });
    state.run = Some(Box::new(FakeRun {
        lines: VecDeque::new(),
        exit: None,
        stop_export_error: false,
        stop_cleanup_error: false,
        stops: Arc::new(AtomicUsize::new(0)),
    }));
    assert_eq!(state.live_repaint_after(), Some(OWNED_RUN_REPAINT_BACKSTOP));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn unchanged_liveness_refresh_does_not_repeat_expensive_reconciliation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-session-supervisor-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let mut state = AppState::with_runtime(
        Store::new(root.clone()),
        Arc::new(ManualClock::new(1_700_000_123)),
        Arc::new(FakeRunBackend::default()),
        Arc::new(FakeSessionCatalog),
    );

    state.apply_live_sessions(vec!["fake-run".into()]);
    assert_eq!(
        state.session_lifecycle_stats,
        SessionLifecycleStats {
            live_refreshes: 1,
            reconciliation_passes: 1,
        }
    );
    state.apply_live_sessions(vec!["fake-run".into()]);
    assert_eq!(
        state.session_lifecycle_stats,
        SessionLifecycleStats {
            live_refreshes: 2,
            reconciliation_passes: 1,
        }
    );
    state.apply_live_sessions(Vec::new());
    assert_eq!(
        state.session_lifecycle_stats,
        SessionLifecycleStats {
            live_refreshes: 3,
            reconciliation_passes: 2,
        }
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn liveness_listing_is_event_driven_with_a_slow_backstop() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-liveness-cadence-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let clock = Arc::new(ManualClock::new(1_700_000_123));
    let calls = Arc::new(AtomicUsize::new(0));
    let mut state = AppState::with_runtime(
        Store::new(root.clone()),
        clock.clone(),
        Arc::new(FakeRunBackend::default()),
        Arc::new(CountingSessionCatalog(calls.clone())),
    );
    let mut record = mission(1);
    record.session = Some("fake-run".into());
    state.trees.insert(
        "p".into(),
        ProjectTree {
            agents: Vec::new(),
            missions: vec![("mission".into(), record)],
        },
    );

    state.poll_live_sessions();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    clock.advance(LIVE_SESSION_BACKSTOP - Duration::from_millis(1));
    state.poll_live_sessions();
    assert_eq!(calls.load(Ordering::Relaxed), 1);

    state.live_sessions_dirty = true;
    state.poll_live_sessions();
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    clock.advance(LIVE_SESSION_BACKSTOP);
    state.poll_live_sessions();
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn raw_output_events_debounce_to_one_settled_reconciliation() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-session-debounce-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let clock = Arc::new(ManualClock::new(1_700_000_123));
    let store = Store::new(root.clone());
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let mut state = AppState::with_runtime(
        store,
        clock.clone(),
        Arc::new(FakeRunBackend::default()),
        Arc::new(FakeSessionCatalog),
    );
    state.refresh();
    let mut record = mission(1);
    record.session = Some("fake-run".into());
    state.trees.insert(
        "p".into(),
        ProjectTree {
            agents: Vec::new(),
            missions: vec![("mission".into(), record)],
        },
    );
    state.live_sessions = vec!["fake-run".into()];
    state.live_sessions_dirty = false;
    state.live_sessions_polled_at = Some(clock.monotonic_now());
    state.session_activity_polled_at = Some(clock.monotonic_now());
    state.session_reconciled_at = Some(clock.monotonic_now());
    state.install_job_runtime(eframe::egui::Context::default());

    let activity = || crate::file_watch::FileInvalidations {
        activity: BTreeSet::from(["p".into()]),
        ..crate::file_watch::FileInvalidations::default()
    };
    state.file_invalidations = Some(Box::new(
        crate::file_watch::FakeFileInvalidationSource::new(activity()),
    ));
    state.poll_file_invalidations();
    clock.advance(Duration::from_secs(corpus_core::WORKING_WINDOW_SECS - 1));
    state.poll_live_sessions();
    assert_eq!(state.session_lifecycle_stats.reconciliation_passes, 0);

    state.file_invalidations = Some(Box::new(
        crate::file_watch::FakeFileInvalidationSource::new(activity()),
    ));
    state.poll_file_invalidations();
    clock.advance(Duration::from_secs(corpus_core::WORKING_WINDOW_SECS - 1));
    state.poll_live_sessions();
    assert_eq!(state.session_lifecycle_stats.reconciliation_passes, 0);
    clock.advance(Duration::from_secs(1));
    state.poll_live_sessions();
    assert_eq!(state.session_lifecycle_stats.reconciliation_passes, 1);
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
    clock.advance(UNWATCHED_STORE_BACKSTOP - Duration::from_millis(1));
    state.poll_launch_requests();
    assert_eq!(state.launch_requests_polled_at, Some(first_poll));
    clock.advance(Duration::from_millis(1));
    state.poll_launch_requests();
    assert!(state.launch_requests_polled_at.unwrap() > first_poll);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn store_audit_backstop_slows_only_while_notifications_are_healthy() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-adaptive-store-backstop-{}-{}",
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

    state.poll_launch_requests();
    let initial = state.launch_requests_polled_at.unwrap();
    clock.advance(UNWATCHED_STORE_BACKSTOP);
    state.poll_launch_requests();
    let fallback_audit = state.launch_requests_polled_at.unwrap();
    assert!(fallback_audit > initial);

    state.file_invalidations = Some(Box::new(
        crate::file_watch::FakeFileInvalidationSource::new(
            crate::file_watch::FileInvalidations::default(),
        ),
    ));
    clock.advance(WATCHED_STORE_BACKSTOP - Duration::from_millis(1));
    state.poll_launch_requests();
    assert_eq!(state.launch_requests_polled_at, Some(fallback_audit));
    clock.advance(Duration::from_millis(1));
    state.poll_launch_requests();
    assert!(state.launch_requests_polled_at.unwrap() > fallback_audit);

    let _ = std::fs::remove_dir_all(root);
}
