use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

#[test]
fn dedicated_binary_advertises_the_admin_catalog() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_corpus-admin-mcp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn corpus-admin-mcp");
    let input = [
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2024-11-05"}
        }),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    ];
    {
        let stdin = child.stdin.as_mut().expect("child stdin");
        for request in input {
            serde_json::to_writer(&mut *stdin, &request).expect("write request");
            stdin.write_all(b"\n").expect("write newline");
        }
    }
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for corpus-admin-mcp");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let responses = String::from_utf8(output.stdout)
        .expect("utf8 responses")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("json response"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 2);
    assert_eq!(
        responses[0]["result"]["serverInfo"]["name"],
        "corpus-admin-mcp"
    );
    assert_eq!(responses[1]["result"]["tools"], corpus_admin::catalog());
}
