use super::*;

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
    store
        .write_mission("p", "delete-me", &record, "brief")
        .unwrap();
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
    store
        .create_project("agents", "Agents", "cdk-regtest")
        .unwrap();
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
    assert!(
        !store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .running_seen
    );
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
    assert!(
        store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .running_seen
    );
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
        Some(corpus_core::MissionCompletion::UnexpectedExit { at: 1_700_000_100 })
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
