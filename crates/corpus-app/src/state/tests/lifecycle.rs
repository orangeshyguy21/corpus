use super::*;

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
        store.create_project(project, "P", "cdk-regtest").unwrap();
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

    let agent_error = state.launch_mission("agent-parent", "mission").unwrap_err();
    assert!(agent_error.to_string().contains("agent"), "{agent_error}");
    assert!(agent_error.to_string().contains("pending deletion"));
    let project_error = state
        .launch_mission("project-parent", "mission")
        .unwrap_err();
    assert!(
        project_error.to_string().contains("project"),
        "{project_error}"
    );
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

    let (result, events) =
        crate::observability::testing::capture_events(|| state.apply_launch_ready(&run_id, ready));
    let error = result.unwrap_err();
    assert!(error.to_string().contains("project p is pending deletion"));
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.target, "corpus.lifecycle");
    assert_eq!(
        event.fields.get("event").map(String::as_str),
        Some("lifecycle.operation")
    );
    assert_eq!(event.fields.get("project").map(String::as_str), Some("p"));
    assert_eq!(
        event.fields.get("mission").map(String::as_str),
        Some("mission")
    );
    assert_eq!(
        event.fields.get("run_session").map(String::as_str),
        Some("p1-p-m7-mission-g1")
    );
    assert_eq!(
        event.fields.get("operation").map(String::as_str),
        Some("launch_adoption")
    );
    assert_eq!(
        event.fields.get("generation").map(String::as_str),
        Some("1")
    );
    assert!(event.fields.contains_key("elapsed_ms"));
    assert_eq!(
        event.fields.get("outcome").map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        event.fields.get("retryable").map(String::as_str),
        Some("false")
    );
    assert!(event
        .fields
        .get("error")
        .is_some_and(|error| error.contains("project p is pending deletion")));
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
