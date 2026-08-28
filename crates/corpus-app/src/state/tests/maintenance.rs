use super::*;

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
fn legacy_live_mission_recovers_and_persists_its_exact_workspace() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-legacy-workspace-recovery-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let store = Store::new(root.join("store"));
    store.create_project("p", "P", "cdk-regtest").unwrap();
    store
        .create_agent_with_role("p", "runner", corpus_core::AgentRole::Tester)
        .unwrap();
    let first = format!("sources-{}", "a".repeat(64));
    let second = format!("sources-{}", "b".repeat(64));
    let first_path = store.project_run_dir("p").join("views").join(&first);
    let second_path = store.project_run_dir("p").join("views").join(&second);
    std::fs::create_dir_all(&first_path).unwrap();
    std::fs::create_dir_all(&second_path).unwrap();
    let mut record = mission(1);
    record.agent = "runner".into();
    record.session = Some("corpus-runner-1700000000".into());
    store
        .write_mission("p", "mission", &record, "brief")
        .unwrap();
    let service = Arc::new(WorkspaceListingService {
        sessions: vec![
            crate::session_service::SessionSummary {
                id: "later-wrong-view".into(),
                directory: first_path,
                created_ms: 1_700_000_003_000,
                title: None,
            },
            crate::session_service::SessionSummary {
                id: "ses_exact".into(),
                directory: second_path,
                created_ms: 1_700_000_001_000,
                title: None,
            },
        ],
    });
    let mut state = AppState::with_runtime(
        store.clone(),
        Arc::new(ManualClock::new(1_700_000_010)),
        Arc::new(FakeRunBackend::default()),
        Arc::new(FakeSessionCatalog),
    );
    state.session_service = service;
    state.select_project("p");
    state.live_sessions = vec!["corpus-runner-1700000000".into()];

    state.sweep_conversations("p");

    let repaired = store.load_mission("p", "mission").unwrap();
    assert_eq!(repaired.opencode_session.as_deref(), Some("ses_exact"));
    assert_eq!(
        repaired.opencode_workspace.as_deref(),
        Some(second.as_str())
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn background_recovery_can_fill_a_missing_workspace_without_rebinding_a_session() {
    let mut legacy = mission(1);
    legacy.session = Some("corpus-runner-1700000000".into());
    legacy.opencode_session = Some("ses_exact".into());

    assert!(discovered_identity_is_current(
        &legacy,
        "corpus-runner-1700000000",
        "ses_exact"
    ));
    assert!(!discovered_identity_is_current(
        &legacy,
        "corpus-runner-1700000000",
        "ses_other"
    ));
    legacy.opencode_workspace = Some(fake_workspace_id());
    assert!(!discovered_identity_is_current(
        &legacy,
        "corpus-runner-1700000000",
        "ses_exact"
    ));
}

#[test]
fn usage_snapshot_makes_cost_independent_of_large_transcript() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-large-cost-checkpoint-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let store = Store::new(root.clone());
    store.create_project("p", "P", "cdk-regtest").unwrap();
    let document = serde_json::json!({
        "info": {"id": "ses_large"},
        "messages": [{
            "info": {
                "role": "assistant",
                "providerID": "openrouter",
                "modelID": "moonshotai/kimi-k3",
                "cost": 4.4480775,
                "tokens": {
                    "input": 970080,
                    "output": 28512,
                    "reasoning": 3948,
                    "cache": {"read": 3503125, "write": 0}
                }
            },
            "parts": [{"type": "text", "text": "x".repeat(140 * 1024)}]
        }]
    });

    let path = store.project_corpus_dir("p").join("runs/ses_large.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    assert!(std::fs::metadata(path).unwrap().len() > 128 * 1024);
    store.backfill_usage_snapshots("p").unwrap();
    std::fs::remove_file(store.project_corpus_dir("p").join("runs/ses_large.json")).unwrap();
    let report = corpus_core::corpus_cost(&store, "p").unwrap();
    assert_eq!(report.rows.len(), 1);
    assert!((report.cost - 4.4480775).abs() < f64::EPSILON);
    assert_eq!(report.tokens, 1_002_540);
    assert_eq!(report.accounted_sessions, 1);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn settled_source_view_turn_updates_live_cost_from_the_exact_workspace() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-live-workspace-cost-{}-{}",
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
    record.control = Some(corpus_core::MissionControl {
        run_id: "fake-run".into(),
        port: 43_111,
    });
    record.opencode_session = Some("ses_source_view".into());
    record.opencode_workspace = Some(fake_workspace_id());
    store
        .write_mission("p", "mission", &record, "brief")
        .unwrap();
    let clock = Arc::new(ManualClock::new(2));
    let export = Arc::new(BlockingExportService {
        block: AtomicBool::new(false),
        in_progress: AtomicBool::new(false),
        directory: Mutex::new(None),
    });
    let mut state = AppState::with_runtime(
        store.clone(),
        clock.clone(),
        Arc::new(FakeRunBackend::default()),
        Arc::new(FakeSessionCatalog),
    );
    state.session_service = export.clone();
    state.refresh();
    state.live_sessions = vec!["fake-run".into()];
    state.session_activity.insert(
        "fake-run".into(),
        clock.monotonic_now() - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1),
    );
    state.install_job_runtime(eframe::egui::Context::default());
    state.schedule_session_maintenance("p");

    for _ in 0..400 {
        state.poll_background_jobs();
        if state
            .corpus_cost()
            .is_some_and(|report| report.tokens == 15)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(
        export.directory.lock().unwrap().as_deref(),
        Some(
            store
                .project_run_dir("p")
                .join("views")
                .join(fake_workspace_id())
                .as_path()
        )
    );
    let report = state.corpus_cost().expect("cost refresh applied");
    assert_eq!(report.tokens, 15);
    assert_eq!(report.cost, 0.5);
    assert!(store
        .project_usage_dir("p")
        .join("ses_source_view.json")
        .is_file());
    let _ = std::fs::remove_dir_all(root);
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
fn structured_lifecycle_failure_is_not_reported_as_resolved_first() {
    let failed_export =
        JobTerminal::Success(AppJobOutput::SessionMaintenance(SessionMaintenance {
            conversations: Vec::new(),
            discovery_failure: None,
            exported_tmux: Vec::new(),
            export_failure: Some(("tmux".into(), "failed".into())),
            warning: None,
        }));
    assert!(!successful_job_resolves_notice(&failed_export));

    let failed_discovery =
        JobTerminal::Success(AppJobOutput::SessionMaintenance(SessionMaintenance {
            conversations: Vec::new(),
            discovery_failure: Some(("tmux".into(), "identity unavailable".into())),
            exported_tmux: Vec::new(),
            export_failure: None,
            warning: None,
        }));
    assert!(!successful_job_resolves_notice(&failed_discovery));

    let failed_teardown = JobTerminal::Success(AppJobOutput::TeardownReady(TeardownReady {
        transcript: None,
        error: Some("failed".into()),
        cleanup_complete: false,
        retained: None,
    }));
    assert!(!successful_job_resolves_notice(&failed_teardown));

    let clean = JobTerminal::Success(AppJobOutput::SessionMaintenance(SessionMaintenance {
        conversations: Vec::new(),
        discovery_failure: None,
        exported_tmux: Vec::new(),
        export_failure: None,
        warning: None,
    }));
    assert!(successful_job_resolves_notice(&clean));
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
    record.control = Some(corpus_core::MissionControl {
        run_id: "fake-run".into(),
        port: 43_111,
    });
    record.opencode_session = Some("fake-conversation".into());
    record.opencode_workspace = Some(fake_workspace_id());
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
        clock.monotonic_now() - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1),
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
    assert!(Project::load(&store, "p")
        .unwrap()
        .delete_requested
        .is_some());
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
    record.control = Some(corpus_core::MissionControl {
        run_id: "fake-run".into(),
        port: 43_111,
    });
    record.opencode_session = Some("fake-conversation".into());
    record.opencode_workspace = Some(fake_workspace_id());
    store
        .write_mission("p", "mission", &record, "brief")
        .unwrap();
    let clock = Arc::new(ManualClock::new(2));
    let backend = Arc::new(FakeRunBackend::default());
    let export = Arc::new(BlockingExportService {
        block: AtomicBool::new(true),
        in_progress: AtomicBool::new(false),
        directory: Mutex::new(None),
    });
    let mut state = AppState::with_runtime(
        store.clone(),
        clock.clone(),
        backend.clone(),
        Arc::new(FakeSessionCatalog),
    );
    state.session_service = export.clone();
    state.refresh();
    state.live_sessions = vec!["fake-run".into()];
    state.session_activity.insert(
        "fake-run".into(),
        clock.monotonic_now() - Duration::from_secs(corpus_core::WORKING_WINDOW_SECS + 1),
    );
    state.install_job_runtime(eframe::egui::Context::default());
    state.schedule_session_maintenance("p");
    for _ in 0..200 {
        if export.in_progress.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(export.in_progress.load(Ordering::Acquire));

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

    export.block.store(false, Ordering::Release);
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

    clock.advance(WATCHED_STORE_BACKSTOP);
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
