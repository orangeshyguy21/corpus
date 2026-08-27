use super::*;

fn project_state(name: &str, projects: &[&str]) -> (PathBuf, AppState) {
    let root = std::env::temp_dir().join(format!(
        "corpus-app-{name}-{}-{}",
        std::process::id(),
        new_uuid_id()
    ));
    let store = Store::new(root.clone());
    for project in projects {
        store
            .create_project(project, project, "cdk-regtest")
            .unwrap();
    }
    (root, AppState::from_store_headless(store))
}

#[test]
fn completed_project_delete_prunes_render_state_immediately() {
    let (root, mut state) = project_state("project-delete-prune", &["keep", "remove"]);
    state.select_project("remove");

    assert_eq!(
        state.delete_project("remove").unwrap(),
        DeleteProjectResult::Completed
    );
    assert!(!state.projects.iter().any(|(slug, _)| slug == "remove"));
    assert!(!state.trees.contains_key("remove"));
    assert_eq!(state.effective_project().as_deref(), Some("keep"));
    assert!(!state.store.project_dir("remove").exists());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_the_last_project_produces_an_empty_selection() {
    let (root, mut state) = project_state("last-project-delete", &["only"]);
    state.select_project("only");

    assert_eq!(
        state.delete_project("only").unwrap(),
        DeleteProjectResult::Completed
    );
    assert!(state.projects.is_empty());
    assert_eq!(state.effective_project(), None);
    assert_eq!(state.selected_project, None);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn scheduled_project_delete_is_retained_but_not_selectable() {
    let (root, mut state) = project_state("scheduled-project-delete", &["keep", "remove"]);
    state
        .store
        .create_agent_with_role("remove", "runner", corpus_core::AgentRole::Tester)
        .unwrap();
    let mut record = mission(1);
    record.agent = "runner".into();
    record.session = Some("live-session".into());
    state
        .store
        .write_mission("remove", "mission", &record, "brief")
        .unwrap();
    state.refresh();

    assert_eq!(
        state.delete_project("remove").unwrap(),
        DeleteProjectResult::Scheduled
    );
    assert!(state
        .projects
        .iter()
        .find(|(slug, _)| slug == "remove")
        .is_some_and(|(_, project)| project.delete_requested.is_some()));
    state.select_project("remove");
    assert_ne!(state.effective_project().as_deref(), Some("remove"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn ghost_project_selection_prunes_the_stale_cache() {
    let (root, mut state) = project_state("ghost-project-click", &["ghost", "keep"]);
    state.store.delete_project("ghost").unwrap();

    state.select_project("ghost");

    assert!(!state.projects.iter().any(|(slug, _)| slug == "ghost"));
    assert_ne!(state.selected_project.as_deref(), Some("ghost"));
    assert_eq!(state.effective_project().as_deref(), Some("keep"));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stale_project_index_snapshot_cannot_restore_removed_project() {
    let (root, mut state) = project_state("stale-project-index", &["keep"]);
    let stale_projects = state.projects.clone();
    let stale_trees = state.trees.clone();
    state.project_index_revision = 2;
    state.projects.clear();
    state.trees.clear();

    assert!(!state.apply_project_index(1, stale_projects, stale_trees));
    assert!(state.projects.is_empty());
    assert!(state.trees.is_empty());

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn refresh_requested_during_active_index_gets_one_current_follow_up() {
    let (root, mut state) = project_state("coalesced-project-index", &["first"]);
    state.install_job_runtime(eframe::egui::Context::default());
    state.refresh();
    state
        .store
        .create_project("second", "second", "cdk-regtest")
        .unwrap();
    state.refresh();
    let requested = state.project_index_revision;

    for _ in 0..300 {
        state.poll_background_jobs();
        if state.projects.iter().any(|(slug, _)| slug == "second")
            && state
                .jobs
                .as_ref()
                .is_some_and(|jobs| !jobs.is_kind_active(JobKind::ProjectIndex))
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    assert_eq!(state.project_index_revision, requested);
    assert!(state.projects.iter().any(|(slug, _)| slug == "second"));
    assert!(state
        .jobs
        .as_ref()
        .is_some_and(|jobs| !jobs.is_kind_active(JobKind::ProjectIndex)));

    let _ = std::fs::remove_dir_all(root);
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
fn historical_agent_label_distinguishes_deleted_agents_without_showing_uuids() {
    let uuid = new_uuid_id();
    assert_eq!(historical_agent_label(None, &uuid), "deleted agent");
    assert_eq!(historical_agent_label(None, "recon-mapper"), "recon-mapper");
    assert_eq!(historical_agent_label(Some("hunter"), &uuid), "hunter");
    assert_eq!(
        historical_agent_label(Some("unnamed agent"), &uuid),
        "unnamed agent"
    );
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
fn created_agent_is_cached_and_can_be_opened_immediately() {
    let (root, mut state) = project_state("create-agent-navigation", &["p"]);
    state.select_project("p");

    let id = state
        .create_agent_with_role("p", corpus_core::AgentRole::Researcher)
        .unwrap();
    state.select_agent("p", &id);

    assert_eq!(state.current_screen, Screen::Agents);
    assert_eq!(state.selected_agent.as_deref(), Some(id.as_str()));
    assert!(state.agents.iter().any(|(slug, _)| slug == &id));
    assert!(state
        .trees
        .get("p")
        .is_some_and(|tree| tree.agents.iter().any(|(slug, _)| slug == &id)));

    let _ = std::fs::remove_dir_all(root);
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
        opencode_workspace: None,
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
        prepared_plugin_leases(&store, Some("p"), Some("fixture-regtest"), &statuses,).is_empty()
    );

    record.id.mission = "deleted-mission".into();
    record.state = corpus_core::EnvironmentSessionState::Ready;
    store.save_environment_session(&record).unwrap();
    let orphan = prepared_plugin_leases(&store, Some("p"), Some("fixture-regtest"), &statuses);
    assert_eq!(orphan.len(), 1);
    assert_eq!(orphan[0].mission, "deleted-mission");
    assert!(orphan[0]
        .drift
        .iter()
        .any(|drift| drift.contains("automatic orphan cleanup pending")));
    assert_eq!(
        orphan_environment_sessions(&store),
        vec![("fixture-regtest".to_string(), record.id.storage_key())]
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
