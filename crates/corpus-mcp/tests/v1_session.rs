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
            cleanup_verified_at: None,
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

    // A v1 target may still be starting when the MCP process takes its
    // startup snapshot. The closed gate must recover through the scoped
    // session probe, never the legacy unscoped `probe` method.
    ctx.probe_ready = false;
    ctx.probe_notes = "target is still starting".into();
    ctx.last_probe = std::time::Instant::now() - std::time::Duration::from_secs(6);

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
    let funded = tools::dispatch(
        &mut ctx,
        "wallet_fund",
        &json!({"amount_sat":10,"idempotency_key":"fund-1"}),
    )
    .unwrap();
    assert!(funded.contains("\"funded\": true"), "{funded}");
    assert_eq!(ctx.faucet_spent_sats, 10);

    let paid = tools::dispatch(
        &mut ctx,
        "faucet",
        &json!({"op":"pay","invoice":"lnbcrt-fixture","idempotency_key":"pay-first"}),
    )
    .unwrap();
    assert!(paid.contains("Payment succeeded (7 sat"), "{paid}");
    assert_eq!(ctx.faucet_spent_sats, 17);

    let oracle_catalog: serde_json::Value =
        serde_json::from_str(&tools::dispatch(&mut ctx, "oracle_list", &json!({})).unwrap())
            .unwrap();
    assert_eq!(
        oracle_catalog,
        json!({"oracles":[{"name":"fixture-invariant","description":"fixture"}]})
    );

    let replay = tools::dispatch(
        &mut ctx,
        "faucet",
        &json!({"op":"pay","invoice":"lnbcrt-fixture","idempotency_key":"pay-replay"}),
    )
    .unwrap();
    assert!(replay.contains("no additional session charge"), "{replay}");
    assert_eq!(ctx.faucet_spent_sats, 17);

    tools::dispatch(
        &mut ctx,
        "wallet_fund",
        &json!({"amount_sat":10,"idempotency_key":"fund-replay"}),
    )
    .unwrap();
    assert_eq!(ctx.faucet_spent_sats, 17);

    let runs = ctx.store.project_corpus_dir("p").join("runs");
    std::fs::create_dir_all(&runs).unwrap();
    std::fs::write(runs.join("1700000000-v1.raw"), "fixture transcript\n").unwrap();
    ctx.run_log = Some("1700000000-v1.raw".into());
    let finding = tools::dispatch(
        &mut ctx,
        "finding_write",
        &json!({"title":"v1 oracle provenance","severity":"low","detail":"fixture detail"}),
    )
    .unwrap();
    assert!(finding.contains("oracle_verified: false"), "{finding}");
    let findings = ctx.store.project_corpus_dir("p").join("findings");
    let path = std::fs::read_dir(findings)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let stored = std::fs::read_to_string(path).unwrap();
    assert!(stored.contains("fixture-invariant"), "{stored}");
    assert!(
        stored.contains(r#"{"oracle":"fixture-invariant","count":1}"#),
        "{stored}"
    );
    assert!(stored.contains("run_log: 1700000000-v1.raw"), "{stored}");
    assert!(
        stored.contains("target: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        "{stored}"
    );

    let invalid =
        tools::dispatch(&mut ctx, "oracle_run", &json!({"name":"invalid-verdict"})).unwrap_err();
    assert!(invalid.to_string().contains("invalid verdict"), "{invalid}");

    let bounded =
        tools::dispatch(&mut ctx, "oracle_run", &json!({"name":"large-evidence"})).unwrap();
    assert!(
        bounded.len() < 17_000,
        "bounded output was {} bytes",
        bounded.len()
    );
    assert!(
        bounded.contains("evidence truncated by corpus"),
        "{bounded}"
    );
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
        concat!("020-", "conservation"),
    ] {
        assert!(
            !source.contains(forbidden),
            "generic MCP tools must not contain target-specific marker {forbidden:?}"
        );
    }
}

#[test]
fn oracle_tools_require_dynamic_discovery() {
    let catalog = tools::catalog();
    let tools = catalog.as_array().unwrap();
    let list = tools
        .iter()
        .find(|tool| tool["name"] == "oracle_list")
        .expect("oracle_list is advertised");
    assert_eq!(list["inputSchema"]["required"], json!([]));
    assert!(list["description"]
        .as_str()
        .unwrap()
        .contains("decide which oracle"));
    let run = tools
        .iter()
        .find(|tool| tool["name"] == "oracle_run")
        .expect("oracle_run is advertised");
    assert!(run["description"].as_str().unwrap().contains("oracle_list"));

    let stale = concat!("020-", "conservation");
    for (name, text) in [
        (
            "tester prompt",
            include_str!("../../corpus-store/src/prompts/tester.md"),
        ),
        ("agent guidance", include_str!("../../../AGENTS.md")),
    ] {
        assert!(
            !text.contains(stale),
            "{name} contains stale oracle example"
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
            cleanup_verified_at: None,
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
