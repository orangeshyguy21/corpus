//! Protocol tests: the corpus-core plugin client against a fake echo
//! plugin (canned UTF-8 JSONL replies, no docker, no host side effects).
//! This pins the wire contract — the exact method names and result shapes
//! the real cdk-regtest plugin must uphold.

use corpus_core::{FaucetCall, Plugin};

fn spawn_echo() -> Plugin {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/echo-plugin");
    Plugin::spawn(&dir).expect("spawn echo plugin")
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