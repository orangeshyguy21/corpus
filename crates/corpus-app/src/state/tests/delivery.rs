use super::*;

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
        parent.opencode_workspace = Some(fake_workspace_id());
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
            completion: Some(corpus_core::MissionCompletion::Completed {
                at: 3,
                artifacts: if slug == "child-a2" {
                    vec!["findings/assembled.md".into()]
                } else {
                    Vec::new()
                },
            }),
            delivery_attempt: 0,
            delivery_message_id: None,
            delivered: slug == "child-a1",
            delivery_abandoned: None,
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
    let (result, events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(
            &store,
            &service,
            &["run-a".into(), "run-b".into(), "old-run".into()],
        )
    });
    result.unwrap();
    let mut calls = service.calls.lock().unwrap().clone();
    calls.sort_by(|left, right| left.run_id.cmp(&right.run_id));
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].run_id, "run-a");
    assert_eq!(calls[0].session_id, "ses_a");
    assert_eq!(
        calls[0].directory,
        store
            .project_run_dir("p")
            .join("views")
            .join(fake_workspace_id())
    );
    assert!(calls[0].message_id.starts_with("msg_corpus"));
    assert!(calls[0].prompt.contains("p/child-a1"));
    assert!(calls[0].prompt.contains("p/child-a2"));
    assert!(calls[0].prompt.contains("findings/assembled.md"));
    assert!(!calls[0].prompt.contains("child-b1"));
    assert_eq!(calls[1].run_id, "run-b");
    assert_ne!(calls[0].password, calls[1].password);
    assert!(calls[1].prompt.contains("p/child-b1"));
    assert_eq!(events.len(), 2);
    let event = events
        .iter()
        .find(|event| event.fields.get("mission").map(String::as_str) == Some("curator-a"))
        .unwrap();
    assert_eq!(event.target, "corpus.delivery");
    assert_eq!(
        event.fields.get("event").map(String::as_str),
        Some("delivery.operation")
    );
    assert_eq!(
        event.fields.get("run_session").map(String::as_str),
        Some("run-a")
    );
    assert_eq!(
        event.fields.get("message_id").map(String::as_str),
        Some(calls[0].message_id.as_str())
    );
    assert_eq!(event.fields.get("attempt").map(String::as_str), Some("1"));
    assert_eq!(
        event.fields.get("child_count").map(String::as_str),
        Some("2")
    );
    assert_eq!(
        event.fields.get("outcome").map(String::as_str),
        Some("succeeded")
    );
    assert_eq!(
        event.fields.get("terminal_state").map(String::as_str),
        Some("acknowledged")
    );
    assert_eq!(
        event.fields.get("retryable").map(String::as_str),
        Some("false")
    );
    assert!(
        store
            .load_mission("p", "child-a1")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
    assert!(
        !store
            .load_mission("p", "child-stale")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
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
    parent.opencode_workspace = Some(fake_workspace_id());
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
        delivery_abandoned: None,
    });
    store.write_mission("p", "child", &child, "work").unwrap();

    let service = RecordingQueueService::default();
    service.fail.store(true, Ordering::Relaxed);
    assert!(deliver_completed_dispatches(&store, &service, &["run-a".into()]).is_err());
    assert!(
        !store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
    service.fail.store(false, Ordering::Relaxed);
    deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
    assert!(
        !store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
    deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
    let calls = service.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].prompt.contains("exited unexpectedly"));
    assert!(
        store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn acknowledged_delivery_with_stale_persistence_is_reported_retryable() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-delivery-persistence-race-{}-{}",
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
    parent.opencode_workspace = Some(fake_workspace_id());
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
        completion: Some(corpus_core::MissionCompletion::Completed {
            at: 3,
            artifacts: Vec::new(),
        }),
        delivery_attempt: 0,
        delivery_message_id: None,
        delivered: false,
        delivery_abandoned: None,
    });
    store.write_mission("p", "child", &child, "work").unwrap();

    let service = RecordingQueueService::default();
    deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
    let store_for_hook = store.clone();
    *service.status_hook.lock().unwrap() = Some(Box::new(move || {
        let mut child = store_for_hook.load_mission("p", "child").unwrap();
        child.dispatch.as_mut().unwrap().delivery_message_id = Some("stale-message".into());
        store_for_hook.update_mission("p", "child", &child).unwrap();
    }));

    let (result, events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(&store, &service, &["run-a".into()])
    });
    let error = result.unwrap_err();
    assert!(error.contains("durable delivery state could not be persisted"));
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].fields.get("terminal_state").map(String::as_str),
        Some("persistence_failed")
    );
    assert_eq!(
        events[0].fields.get("retryable").map(String::as_str),
        Some("true")
    );
    assert!(
        !store
            .load_mission("p", "child")
            .unwrap()
            .dispatch
            .unwrap()
            .delivered
    );
    std::fs::remove_dir_all(root).unwrap();
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
    parent.opencode_workspace = Some(fake_workspace_id());
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
        completion: Some(corpus_core::MissionCompletion::Completed {
            at: 3,
            artifacts: Vec::new(),
        }),
        delivery_attempt: 0,
        delivery_message_id: None,
        delivered: false,
        delivery_abandoned: None,
    });
    store.write_mission("p", "child", &child, "work").unwrap();

    let service = RecordingQueueService::default();
    *service.prompt_state.lock().unwrap() = PromptDeliveryState::Failed {
        error: "Model unavailable".into(),
        retry_ready: false,
        interrupted: false,
    };
    deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
    let admitted = store.load_mission("p", "child").unwrap().dispatch.unwrap();
    assert_eq!(admitted.delivery_attempt, 1);
    assert!(admitted.delivery_message_id.is_some());
    assert!(!admitted.delivered);

    let (result, failed_events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(&store, &service, &["run-a".into()])
    });
    let error = result.unwrap_err();
    assert!(error.contains("Model unavailable"));
    assert_eq!(service.calls.lock().unwrap().len(), 1);
    let failed = store.load_mission("p", "child").unwrap().dispatch.unwrap();
    assert!(!failed.delivered);
    assert_eq!(failed.delivery_message_id, admitted.delivery_message_id);
    assert_eq!(failed_events.len(), 1);
    let failed_event = &failed_events[0];
    assert_eq!(failed_event.target, "corpus.delivery");
    assert_eq!(
        failed_event.fields.get("message_id").map(String::as_str),
        admitted.delivery_message_id.as_deref()
    );
    assert_eq!(
        failed_event
            .fields
            .get("terminal_state")
            .map(String::as_str),
        Some("failed")
    );
    assert_eq!(
        failed_event.fields.get("retryable").map(String::as_str),
        Some("false")
    );

    // Reconciliation remains observational after the failure; it does
    // not mint fresh prompt ids and spin the paid model overnight.
    assert!(deliver_completed_dispatches(&store, &service, &["run-a".into()]).is_err());
    assert_eq!(service.calls.lock().unwrap().len(), 1);

    *service.prompt_state.lock().unwrap() = PromptDeliveryState::Failed {
        error: "Model unavailable".into(),
        retry_ready: true,
        interrupted: false,
    };
    let (result, retry_events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(&store, &service, &["run-a".into()])
    });
    assert!(result.is_err());
    assert_eq!(retry_events.len(), 1);
    assert_eq!(
        retry_events[0]
            .fields
            .get("terminal_state")
            .map(String::as_str),
        Some("retry_ready")
    );
    assert_eq!(
        retry_events[0].fields.get("retryable").map(String::as_str),
        Some("true")
    );
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
fn interrupted_completion_delivery_is_durably_abandoned_across_restart() {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-delivery-interrupted-{}-{}",
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
    parent.opencode_workspace = Some(fake_workspace_id());
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
        completion: Some(corpus_core::MissionCompletion::Completed {
            at: 3,
            artifacts: Vec::new(),
        }),
        delivery_attempt: 0,
        delivery_message_id: None,
        delivered: false,
        delivery_abandoned: None,
    });
    store.write_mission("p", "child", &child, "work").unwrap();

    let service = RecordingQueueService::default();
    deliver_completed_dispatches(&store, &service, &["run-a".into()]).unwrap();
    *service.prompt_state.lock().unwrap() = PromptDeliveryState::Failed {
        error: "Aborted".into(),
        retry_ready: false,
        interrupted: true,
    };
    let (result, events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(&store, &service, &["run-a".into()])
    });
    result.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].fields.get("terminal_state").map(String::as_str),
        Some("abandoned")
    );
    assert_eq!(
        events[0].fields.get("outcome").map(String::as_str),
        Some("abandoned")
    );
    let persisted = store.load_mission("p", "child").unwrap().dispatch.unwrap();
    let abandonment = persisted.delivery_abandoned.unwrap();
    assert_eq!(
        abandonment.message_id,
        persisted.delivery_message_id.unwrap()
    );
    assert_eq!(abandonment.reason, "interrupted");
    assert!(!persisted.delivered);

    // A fresh reconciler sees the durable terminal disposition and neither
    // polls nor reports the interrupted delivery again.
    let restarted_service = RecordingQueueService::default();
    let (result, restart_events) = crate::observability::testing::capture_events(|| {
        deliver_completed_dispatches(&store, &restarted_service, &["run-a".into()])
    });
    result.unwrap();
    assert!(restart_events.is_empty());
    assert!(restarted_service.calls.lock().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn failed_environment_survives_restart_and_blocks_relaunch_and_delete() {
    const MISSING_PLUGIN: &str = "missing-session-plugin";
    let root = std::env::temp_dir().join(format!(
        "corpus-app-environment-recovery-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let store = Store::new(root.join("store"));
    store.create_project("p", "P", MISSING_PLUGIN).unwrap();
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
        plugin_id: MISSING_PLUGIN.into(),
        plugin_version: "1.0.0".into(),
        plugin_digest: "fixture".into(),
        state: corpus_core::EnvironmentSessionState::Failed,
        source_shas: Default::default(),
        environment_lock: None,
        image_digest: None,
        created: 1,
        updated: 2,
        error: Some("cleanup failed".into()),
        cleanup_verified_at: None,
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
        cleanup_error
            .to_string()
            .contains("plugin not found: missing-session-plugin"),
        "{cleanup_error}"
    );
    assert!(store.load_mission("p", "mission").is_ok());

    environment.state = corpus_core::EnvironmentSessionState::Closed;
    store.save_environment_session(&environment).unwrap();
    assert!(!state.mission_environment_needs_cleanup("p", "mission"));
    state.delete_mission("p", "mission").unwrap();
    let _ = std::fs::remove_dir_all(root);
}
