use corpus_core::{
    AgentRole, EnvironmentSessionId, EnvironmentSessionRecord, EnvironmentSessionState, Plugin,
    Scope, Store,
};
use corpus_mcp::tools::{self, Ctx};
use serde_json::json;

#[test]
fn generic_tools_forward_the_durable_v1_session_and_typed_description() {
    let root = std::env::temp_dir().join(format!("corpus-mcp-v1-session-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.join("store"));
    store
        .create_project("p", "P", "v1-session-fixture")
        .unwrap();
    store
        .create_agent_with_role("p", "tester", AgentRole::Tester)
        .unwrap();
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v1-session-plugin");
    let environment_id = EnvironmentSessionId {
        project: "p".into(),
        mission: "m".into(),
        generation: 1,
    };
    store
        .save_environment_session(&EnvironmentSessionRecord {
            id: environment_id.clone(),
            plugin_id: "v1-session-fixture".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: corpus_core::plugin_bundle_digest(&fixture).unwrap(),
            state: EnvironmentSessionState::Ready,
            source_shas: std::collections::BTreeMap::from([(
                "target".into(),
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            )]),
            environment_lock: None,
            image_digest: None,
            created: 1,
            updated: 1,
            error: None,
        })
        .unwrap();
    let plugin = Plugin::spawn(&fixture).unwrap();
    let mut ctx = Ctx::for_test(
        plugin,
        store,
        Scope {
            project: "p".into(),
        },
        AgentRole::Tester,
    );
    ctx.environment_session = Some(environment_id.storage_key());
    ctx.source_pins = Some(serde_json::Map::from_iter([(
        "target".into(),
        json!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
    )]));

    let info: serde_json::Value =
        serde_json::from_str(&tools::dispatch(&mut ctx, "target_info", &json!({})).unwrap())
            .unwrap();
    assert_eq!(info["environment_session"], "p1-p-m1-m-g1");
    assert_eq!(info["targets"][0]["id"], "target");
    assert_eq!(
        info["sources"][0]["path"],
        "sources/target/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    );
    assert!(
        tools::dispatch(&mut ctx, "sandbox_exec", &json!({"command":"id"}))
            .unwrap()
            .contains("fixture sandbox")
    );
    let funded = tools::dispatch(&mut ctx, "wallet_fund", &json!({"amount_sat":10})).unwrap();
    assert!(funded.contains("\"funded\": true"), "{funded}");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn generic_mcp_layer_contains_no_target_specific_wallet_contract() {
    let source = include_str!("../src/tools.rs");
    for forbidden in [
        concat!("c", "dk-cli"),
        concat!("c", "dk-regtest"),
        concat!("nuts", "hell"),
        "targets[0]",
        r#""target":{"type":"integer""#,
    ] {
        assert!(
            !source.contains(forbidden),
            "generic MCP tools must not contain target-specific marker {forbidden:?}"
        );
    }
}

#[test]
fn a_second_plugin_projects_its_own_targets_tools_and_capabilities() {
    let root =
        std::env::temp_dir().join(format!("corpus-mcp-v1-session-alt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.join("store"));
    store
        .create_project("other", "Other", "v1-session-fixture-alt")
        .unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v1-session-plugin-alt");
    let plugin = Plugin::spawn(&fixture).unwrap();
    let mut ctx = Ctx::for_test(
        plugin,
        store,
        Scope {
            project: "other".into(),
        },
        AgentRole::Tester,
    );
    ctx.environment_session = Some("p5-other-m3-alt-g4".into());
    ctx.source_pins = Some(serde_json::Map::from_iter([(
        "service".into(),
        json!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
    )]));

    let info: serde_json::Value =
        serde_json::from_str(&tools::dispatch(&mut ctx, "target_info", &json!({})).unwrap())
            .unwrap();
    assert_eq!(info["targets"][0]["id"], "service");
    assert_eq!(info["tools_in_sandbox"][0]["id"], "service-tool");
    assert_eq!(
        info["sources"][0]["path_inside_sandbox_exec"],
        "/srv/source"
    );
    assert_eq!(info["provenance"]["image_digest"], "sha256:alternate");
    assert!(
        tools::dispatch(&mut ctx, "sandbox_exec", &json!({"command":"id"}))
            .unwrap()
            .contains("alternate sandbox")
    );
    assert!(tools::dispatch(&mut ctx, "wallet_fund", &json!({"amount_sat":1})).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn startup_refuses_an_environment_session_from_another_project() {
    let root =
        std::env::temp_dir().join(format!("corpus-mcp-v1-cross-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let store = Store::new(root.join("store"));
    store
        .create_project("p", "P", "v1-session-fixture-alt")
        .unwrap();
    store
        .create_agent_with_role("p", "tester", AgentRole::Tester)
        .unwrap();
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v1-session-plugin-alt");
    let id = EnvironmentSessionId {
        project: "other".into(),
        mission: "alt".into(),
        generation: 1,
    };
    store
        .save_environment_session(&EnvironmentSessionRecord {
            id: id.clone(),
            plugin_id: "v1-session-fixture-alt".into(),
            plugin_version: "1.0.0".into(),
            plugin_digest: corpus_core::plugin_bundle_digest(&fixture).unwrap(),
            state: EnvironmentSessionState::Ready,
            source_shas: Default::default(),
            environment_lock: None,
            image_digest: None,
            created: 1,
            updated: 1,
            error: None,
        })
        .unwrap();

    let previous: Vec<(&str, Option<std::ffi::OsString>)> = [
        (
            corpus_core::STORE_ENV,
            Some(store.root().as_os_str().to_owned()),
        ),
        (corpus_core::PROJECT_ENV, Some("p".into())),
        (corpus_core::AGENT_ENV, Some("tester".into())),
        (
            corpus_core::ENVIRONMENT_SESSION_ENV,
            Some(id.storage_key().into()),
        ),
        (
            "CORPUS_PLUGINS_DIR",
            Some(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests")
                    .into_os_string(),
            ),
        ),
    ]
    .into_iter()
    .map(|(key, value)| {
        let old = std::env::var_os(key);
        std::env::set_var(key, value.unwrap());
        (key, old)
    })
    .collect();
    let mut ctx = Ctx::from_env().unwrap();
    for (key, value) in previous.into_iter().rev() {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }
    assert!(!ctx.probe_ready);
    assert!(
        ctx.probe_notes.contains("does not match project"),
        "{}",
        ctx.probe_notes
    );
    assert!(tools::dispatch(&mut ctx, "target_info", &json!({})).is_err());
    let _ = std::fs::remove_dir_all(root);
}
