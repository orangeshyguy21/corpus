//! Protocol tests: the corpus-core plugin client against a fake echo
//! plugin (canned UTF-8 JSONL replies, no docker, no host side effects).
//! This pins the wire contract — the exact method names and result shapes
//! the real cdk-regtest plugin must uphold.

use corpus_core::{FaucetCall, Plugin};

fn spawn_echo() -> Plugin {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/echo-plugin");
    Plugin::spawn(&dir).expect("spawn echo plugin")
}

fn spawn_v1_echo() -> Plugin {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v1-echo-plugin");
    Plugin::spawn(&dir).expect("spawn v1 echo plugin")
}

#[test]
fn probe_reports_ready() {
    let mut plugin = spawn_echo();
    let probe = plugin.probe().expect("probe");
    assert!(probe.ready);
    assert_eq!(probe.notes, "echo up");
}

#[test]
fn targets_and_tools() {
    let mut plugin = spawn_echo();
    let targets = plugin.targets().expect("targets");
    assert_eq!(
        targets,
        vec![
            "http://echo-gw:8085".to_string(),
            "http://echo-gw:8087".to_string()
        ]
    );
    let tools = plugin.tools().expect("tools");
    assert_eq!(tools, vec!["/opt/tools/cdk-cli".to_string()]);
}

#[test]
fn sandbox_exec_returns_output_and_exit_code() {
    let mut plugin = spawn_echo();
    let result = plugin.sandbox_exec("ls -la /tmp").expect("sandbox_exec");
    assert_eq!(result.output, "echo-container:ls -la /tmp");
    assert_eq!(result.exit_code, 0);
}

#[test]
fn sandbox_exec_forwards_source_pins() {
    let mut plugin = spawn_echo();
    let pins: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
        r#"{"cdk":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
    )
    .unwrap();
    let result = plugin
        .sandbox_exec_with_sources("ls", Some(&pins))
        .expect("sandbox_exec_with_sources");
    assert_eq!(
        result.output,
        r#"echo-container:ls:{"cdk":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#
    );
    // None sends no sources key at all (the echo shows nothing extra).
    let plain = plugin
        .sandbox_exec_with_sources("ls", None)
        .expect("sandbox_exec plain");
    assert_eq!(plain.output, "echo-container:ls");
}

#[test]
fn sources_report_default_and_override_pins() {
    let mut plugin = spawn_echo();
    let default = plugin.sources().expect("sources");
    assert_eq!(default.len(), 1);
    assert_eq!(default[0].name, "cdk");
    assert_eq!(
        default[0].sha,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        "no pins -> the plugin's default pin"
    );
    // Forwarding the mission's resolved pins must change the reported
    // sha: target_info must not contradict the launched sandbox mounts.
    let mut pins = serde_json::Map::new();
    pins.insert(
        "cdk".to_string(),
        serde_json::Value::String("cccccccccccccccccccccccccccccccccccccccc".to_string()),
    );
    let pinned = plugin
        .sources_with_sources(Some(&pins))
        .expect("sources with pins");
    assert_eq!(pinned[0].sha, "cccccccccccccccccccccccccccccccccccccccc");
    assert_eq!(pinned[0].mount, "/opt/src/cdk");
}

#[test]
fn oracles_and_call_oracle() {
    let mut plugin = spawn_echo();
    let oracles = plugin.oracles().expect("oracles");
    assert_eq!(oracles.len(), 1);
    assert_eq!(oracles[0].name, "001-echo");

    let result = plugin.call_oracle("001-echo").expect("call_oracle");
    assert_eq!(result.verdict, "violated");
    assert_eq!(result.log, "echo oracle log");
}

#[test]
fn faucet_pay_reports_paid_sats() {
    let mut plugin = spawn_echo();
    let call = FaucetCall {
        invoice: Some("lnbcrt1echo".to_string()),
        ..Default::default()
    };
    let result = plugin.faucet("pay", &call).expect("faucet pay");
    assert_eq!(result.paid_sats, Some(42));
    assert!(result.text.contains("42 sat"));
}

#[test]
fn faucet_invoice_returns_invoice_text() {
    let mut plugin = spawn_echo();
    let call = FaucetCall {
        amount_sat: Some(1000),
        memo: Some("test".to_string()),
        ..Default::default()
    };
    let result = plugin.faucet("invoice", &call).expect("faucet invoice");
    assert_eq!(result.paid_sats, None);
    assert!(result.text.starts_with("lnbcrt1"));
}

#[test]
fn protocol_error_round_trips_as_plugin_error() {
    let mut plugin = spawn_echo();
    let result = plugin.call("definitely-not-a-method", None);
    let Err(corpus_core::Error::Plugin { plugin: name, message }) = result else {
        panic!("expected Plugin error, got {result:?}");
    };
    assert_eq!(name, "echo-plugin");
    assert!(message.contains("unknown method"));
}

#[test]
fn v1_hello_negotiates_the_declared_protocol() {
    let mut plugin = spawn_v1_echo();
    let hello = plugin.hello().expect("hello");
    assert_eq!(hello.protocol, corpus_core::ENVIRONMENT_PROTOCOL_V1);
    assert_eq!(hello.capabilities, vec!["lifecycle.setup"]);
}

#[test]
fn v1_hello_refuses_manifest_executable_capability_drift() {
    let source = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/v1-echo-plugin");
    let dir = std::env::temp_dir().join(format!(
        "corpus-v1-hello-drift-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source_exec = source.join("plugin.sh");
    let exec = dir.join("plugin.sh");
    std::fs::copy(&source_exec, &exec).unwrap();
    std::fs::set_permissions(&exec, std::fs::metadata(&source_exec).unwrap().permissions()).unwrap();
    std::fs::write(
        dir.join("plugin.toml"),
        r#"
manifest_version = 1
id = "v1-drift"
protocol = "corpus.environment/1"
exec = "plugin.sh"
capabilities = ["sessions"]
"#,
    )
    .unwrap();

    let mut plugin = Plugin::spawn(&dir).unwrap();
    let error = plugin.hello().unwrap_err().to_string();
    assert!(error.contains("do not match manifest"), "{error}");
    drop(plugin);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn v1_setup_streams_progress_then_one_terminal_result() {
    let mut plugin = spawn_v1_echo();
    let mut phases = Vec::new();
    let result = plugin
        .lifecycle_call(
            "setup",
            None,
            std::time::Duration::from_secs(2),
            |progress| phases.push(progress.phase.clone()),
        )
        .expect("setup");
    assert_eq!(phases, vec!["dependency_fetch", "verification"]);
    assert_eq!(result, serde_json::json!({"ready": true}));
}

#[test]
fn v1_lifecycle_errors_keep_stable_code_and_retryability() {
    let mut plugin = spawn_v1_echo();
    let error = plugin
        .lifecycle_call(
            "doctor",
            None,
            std::time::Duration::from_secs(2),
            |_| {},
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("docker_unavailable"), "{error}");
    assert!(error.contains("retryable: true"), "{error}");
}

#[test]
fn v1_operation_status_makes_retry_decisions_explicit() {
    let mut plugin = spawn_v1_echo();
    let status = plugin.operation_status("setup:project:mission:1").unwrap();
    assert_eq!(status.idempotency_key, "setup:project:mission:1");
    assert_eq!(status.state, corpus_core::OperationState::Succeeded);
    assert_eq!(status.result, Some(serde_json::json!({"ready": true})));
}

#[test]
fn v1_lifecycle_rejects_a_mismatched_reply_id() {
    let mut plugin = spawn_v1_echo();
    let error = plugin
        .lifecycle_call(
            "status",
            None,
            std::time::Duration::from_secs(2),
            |_| {},
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("does not match request id"), "{error}");
}

#[test]
fn v1_lifecycle_cancellation_kills_a_silent_child_promptly() {
    let mut plugin = spawn_v1_echo();
    let started = std::time::Instant::now();
    let error = plugin
        .lifecycle_call_cancellable(
            "stop",
            None,
            std::time::Duration::from_secs(2),
            || started.elapsed() >= std::time::Duration::from_millis(50),
            |_| {},
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

#[test]
fn v1_lifecycle_outer_deadline_kills_a_silent_child() {
    let mut plugin = spawn_v1_echo();
    let started = std::time::Instant::now();
    let error = plugin
        .lifecycle_call(
            "stop",
            None,
            std::time::Duration::from_millis(50),
            |_| {},
        )
        .unwrap_err()
        .to_string();
    assert!(error.contains("timed out"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}
