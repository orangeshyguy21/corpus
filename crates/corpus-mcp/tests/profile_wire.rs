use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn legacy_admin_flag_cannot_enable_host_admin_tools() {
    let store =
        std::env::temp_dir().join(format!("corpus-research-profile-{}", std::process::id()));
    let mut child = Command::new(env!("CARGO_BIN_EXE_corpus-mcp"))
        .arg("--admin")
        .env("CORPUS_STORE", &store)
        .env("CORPUS_PLUGIN_DIR", store.join("missing-plugin"))
        .env_remove("CORPUS_PROJECT")
        .env_remove("CORPUS_OPENCODE_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn research MCP");
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        serde_json::to_writer(
            &mut *stdin,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}),
        )
        .expect("write request");
        stdin.write_all(b"\n").expect("write newline");
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for research MCP");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Value = serde_json::from_slice(&output.stdout).expect("JSON response");
    let tools = response["result"]["tools"].as_array().expect("tools array");
    assert!(
        tools.is_empty(),
        "unresolved research identity must advertise nothing"
    );
    assert!(tools.iter().all(|tool| tool["name"] != "project_list"));
}
