use super::*;

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
        crate::file_watch::FakeFileInvalidationSource::new(crate::file_watch::FileInvalidations {
            metadata: BTreeSet::from(["p".into()]),
            corpus: BTreeSet::from(["p".into()]),
            activity: BTreeSet::from(["p".into()]),
            ..crate::file_watch::FileInvalidations::default()
        }),
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
        crate::file_watch::FakeFileInvalidationSource::new(crate::file_watch::FileInvalidations {
            corpus: BTreeSet::from(["p".into()]),
            ..crate::file_watch::FileInvalidations::default()
        }),
    ));
    state.poll_file_invalidations();
    state.poll_project_scope();
    wait_for_finding_titles(&mut state, &["One", "Two"]);

    write_finding_fixture(&store, "p", "one.md", "One edited");
    std::fs::remove_file(store.project_corpus_dir("p").join("findings/nested/two.md")).unwrap();
    state.file_invalidations = Some(Box::new(
        crate::file_watch::FakeFileInvalidationSource::new(crate::file_watch::FileInvalidations {
            corpus: BTreeSet::from(["p".into()]),
            ..crate::file_watch::FileInvalidations::default()
        }),
    ));
    state.poll_file_invalidations();
    state.poll_project_scope();
    wait_for_finding_titles(&mut state, &["One edited"]);

    // No event: the slow watched-store audit still discovers the change,
    // without a findings-specific timer.
    write_finding_fixture(&store, "p", "three.md", "Three");
    clock.advance(WATCHED_STORE_BACKSTOP);
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
