//! Host-side corpus administration MCP server.
//!
//! This first Epic 3 cut gives management chat a distinct executable and
//! avoids environment-plugin startup. P3.2 moves the admin implementation
//! onto narrow store/path dependencies so the dependency-policy gate turns
//! green.

use std::io::{BufRead, Write};

use corpus_admin::{error::Error, State};
use serde_json::{json, Value};

fn main() {
    if let Err(error) = serve() {
        eprintln!("corpus-admin-mcp: fatal: {error}");
        std::process::exit(1);
    }
}

fn serve() -> Result<(), Error> {
    let mut state = State::from_env();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let response = match request.get("method").and_then(Value::as_str).unwrap_or("") {
            "initialize" => initialize(id, &request),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => {
                json!({"jsonrpc": "2.0", "id": id, "result": {"tools": corpus_admin::catalog()}})
            }
            "tools/call" => call(&mut state, id, &request),
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("unknown method: {other}")}
            }),
        };
        serde_json::to_writer(&mut stdout, &response)?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
    }
    Ok(())
}

fn initialize(id: Value, request: &Value) -> Value {
    let protocol = request
        .pointer("/params/protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2024-11-05");
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "protocolVersion": protocol,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "corpus-admin-mcp", "version": env!("CARGO_PKG_VERSION")}
        }
    })
}

fn call(state: &mut State, id: Value, request: &Value) -> Value {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let empty = json!({});
    let args = request.pointer("/params/arguments").unwrap_or(&empty);
    match corpus_admin::dispatch(&mut state.context(), name, args) {
        Ok(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {"content": [{"type": "text", "text": text}]}
        }),
        Err(Error::Args(message)) | Err(Error::Refused { message, .. }) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": format!("error: {message}")}],
                "isError": true
            }
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{"type": "text", "text": format!("error: {error}")}],
                "isError": true
            }
        }),
    }
}
