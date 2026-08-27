use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use corpus_app::state::AppState;
use corpus_core::{AgentRole, Mission, Store};
use corpus_integration::{ollama, ModelLease, TestHarness};

const PROJECT: &str = "serial-curator";
const PARENT: &str = "curator-campaign";

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match self.previous.as_ref() {
            Some(value) => std::env::set_var(self.key, value),
            None => std::env::remove_var(self.key),
        }
    }
}

struct TmuxCleanup(Store);

impl Drop for TmuxCleanup {
    fn drop(&mut self) {
        if let Ok(missions) = self.0.list_missions(PROJECT) {
            for (_, mission) in missions {
                if let Some(session) = mission.session {
                    let _ = corpus_core::kill_tmux_session_checked(&session);
                }
            }
        }
    }
}

fn mission(agent: &str, name: &str) -> Mission {
    Mission {
        agent: agent.into(),
        pins: Default::default(),
        budget: None,
        created: 1,
        name: Some(name.into()),
        session: None,
        control: None,
        opencode_session: None,
        opencode_workspace: None,
        environment_session: None,
        launch_requested: None,
        delete_requested: None,
        dispatch: None,
    }
}

fn copy_plugin(harness: &TestHarness) -> PathBuf {
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/noop-plugin");
    let catalog = harness.world().join("plugins");
    let destination = catalog.join("noop-integration");
    std::fs::create_dir_all(&destination).unwrap();
    for name in ["plugin.toml", "plugin.sh"] {
        std::fs::copy(source.join(name), destination.join(name)).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            destination.join("plugin.sh"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    catalog
}

fn wait_until(
    state: &mut AppState,
    timeout: Duration,
    label: &str,
    mut predicate: impl FnMut(&Store) -> bool,
) {
    let deadline = Instant::now() + timeout;
    loop {
        state
            .reconcile_headless_sessions()
            .unwrap_or_else(|error| panic!("{label}: reconciliation failed: {error}"));
        if predicate(state.store()) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {label}");
        std::thread::sleep(Duration::from_millis(750));
    }
}

fn wait_for_terminal_turn(state: &mut AppState, project: &str, mission: &str) {
    let deadline = Instant::now() + Duration::from_secs(240);
    loop {
        state.reconcile_headless_sessions().unwrap();
        if state
            .mission_turn_completed(project, mission)
            .unwrap_or(false)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {project}/{mission} terminal turn"
        );
        std::thread::sleep(Duration::from_millis(750));
    }
}

/// Full production curator campaign. It is ignored by default because it
/// launches real OpenCode/tmux processes and performs three MLX inference
/// turns. Every launch is gated on the prior turn's terminal proof, so the
/// one-model host never has two active inference turns.
#[test]
#[ignore = "model-qwen38: real serial OpenCode/tmux curator campaign"]
fn curator_launches_children_serially_and_receives_exact_completions() {
    let _lease = ModelLease::acquire("curator-system-orchestration").unwrap();
    let installed = ollama::require_qwen38().unwrap();
    let harness = TestHarness::new("curator-system-qwen38-mlx");
    let plugin_catalog = copy_plugin(&harness);
    let mcp = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/corpus-mcp");
    assert!(mcp.is_file(), "build corpus-mcp before the live scenario");

    let _store_env = EnvGuard::set("CORPUS_STORE", harness.store().root());
    let _home_env = EnvGuard::set("CORPUS_HOME", harness.world().join("home"));
    let _plugins_env = EnvGuard::set("CORPUS_PLUGINS_DIR", &plugin_catalog);
    let _mcp_env = EnvGuard::set("CORPUS_MCP", &mcp);
    let _model_env = EnvGuard::set("CORPUS_QWEN38_MODEL", &installed.name);

    let store = harness.store().clone();
    let _cleanup = TmuxCleanup(store.clone());
    store
        .create_project(PROJECT, "Serial curator campaign", "noop-integration")
        .unwrap();
    let launch_model = format!("ollama/{}", installed.name);
    store
        .create_agent(&corpus_core::CreateAgentRequest {
            project: PROJECT.into(),
            slug: "curator".into(),
            description: "Serial integration curator".into(),
            prompt: "You coordinate missions exactly as instructed. Use Corpus tools and never invent tool results.".into(),
            model: Some(launch_model.clone()),
            from: None,
            role: Some(AgentRole::Curator),
        })
        .unwrap();
    store
        .create_agent(&corpus_core::CreateAgentRequest {
            project: PROJECT.into(),
            slug: "worker".into(),
            description: "Deterministic integration worker".into(),
            prompt: "Complete the brief directly, without creating or launching other missions."
                .into(),
            model: Some(launch_model),
            from: None,
            role: Some(AgentRole::Researcher),
        })
        .unwrap();
    store
        .write_mission(
            PROJECT,
            PARENT,
            &mission("curator", "Curator campaign"),
            "Create mission child-one for agent worker with brief 'Reply CHILD_ONE_DONE'. Launch child-one. Do not create child-two yet. After Corpus notifies you that child-one completed, create mission child-two for agent worker with brief 'Reply CHILD_TWO_DONE', then launch it. After Corpus notifies you that child-two completed, reply CAMPAIGN_DONE. Never launch more than one child at a time.",
        )
        .unwrap();

    // Fail before spending a model turn if the test-only plugin catalog is
    // not available to the local MCP process spawned by OpenCode.
    let run_config = store
        .provision_run_dir(PROJECT)
        .unwrap()
        .join(".opencode/opencode.json");
    let run_config: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(run_config).unwrap()).unwrap();
    assert_eq!(
        run_config["mcp"]["corpus"]["environment"]["CORPUS_PLUGINS_DIR"].as_str(),
        Some(plugin_catalog.to_string_lossy().as_ref()),
        "run-local MCP must inherit the integration plugin catalog"
    );

    let mut state = AppState::from_store_headless(store.clone());
    state.launch_mission_detached(PROJECT, PARENT).unwrap();

    wait_until(
        &mut state,
        Duration::from_secs(240),
        "curator first terminal turn",
        |store| {
            let first_requested = store
                .load_mission(PROJECT, "child-one")
                .is_ok_and(|mission| mission.launch_requested.is_some());
            let no_second = store.load_mission(PROJECT, "child-two").is_err();
            first_requested && no_second
        },
    );
    wait_for_terminal_turn(&mut state, PROJECT, PARENT);
    state.honor_headless_launch_requests();

    wait_until(
        &mut state,
        Duration::from_secs(240),
        "first completion acknowledgement",
        |store| {
            store
                .load_mission(PROJECT, "child-one")
                .is_ok_and(|mission| mission.dispatch.is_some_and(|dispatch| dispatch.delivered))
        },
    );
    assert!(state.mission_turn_completed(PROJECT, PARENT).unwrap());
    // Restart the coordinator while the curator TUI and durable campaign
    // survive. The replacement must rediscover the exact conversation before
    // it may honor the follow-up launch or deliver the second completion.
    drop(state);
    let mut state = AppState::from_store_headless(store.clone());
    state.reconcile_headless_sessions().unwrap();
    assert!(state.mission_turn_completed(PROJECT, PARENT).unwrap());
    assert!(store
        .load_mission(PROJECT, "child-two")
        .is_ok_and(|mission| mission.launch_requested.is_some()));
    state.honor_headless_launch_requests();

    wait_until(
        &mut state,
        Duration::from_secs(240),
        "second completion acknowledgement",
        |store| {
            store
                .load_mission(PROJECT, "child-two")
                .is_ok_and(|mission| mission.dispatch.is_some_and(|dispatch| dispatch.delivered))
        },
    );
    assert!(state.mission_turn_completed(PROJECT, PARENT).unwrap());

    let snapshot = store.list_missions(PROJECT).unwrap();
    harness.record_json("missions.json", &snapshot);
    harness.record_json(
        "model.json",
        &serde_json::json!({"name": installed.name, "digest": installed.digest}),
    );
}
