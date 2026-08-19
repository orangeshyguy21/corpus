//! corpus-mcp: MCP server (stdio, newline-delimited JSON-RPC) exposing the
//! corpus harness — sandbox, oracles, faucet, gated findings — to OpenCode
//! agents. Hand-rolled minimal protocol: initialize, tools/list,
//! tools/call. No async, no framework: the attack surface stays auditable.

use std::io::{BufRead, Write};

use serde_json::{json, Value};

use corpus_mcp::{
    error::{Error, Result},
    tools,
    tools::Ctx,
};

fn main() {
    if let Err(error) = serve() {
        eprintln!("corpus-mcp: fatal: {error}");
        std::process::exit(1);
    }
}

fn serve() -> Result<()> {
    let mut ctx = Ctx::from_env()?;
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue, // undecidable id; nothing safe to reply to
        };
        let method = request.get("method").and_then(Value::as_str).unwrap_or("");
        let id = request.get("id").cloned();

        // Notifications (no id) get no response.
        let Some(id) = id else {
            continue;
        };

        let response = match method {
            "initialize" => {
                let client_version = request
                    .pointer("/params/protocolVersion")
                    .and_then(Value::as_str)
                    .unwrap_or("2024-11-05");
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": client_version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "corpus-mcp", "version": env!("CARGO_PKG_VERSION") }
                    }
                })
            }
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => {
                // The research server advertises only what this run's role
                // can actually call. Host-global administration is a
                // different artifact (`corpus-admin-mcp`).
                let tools = tools::catalog_for(&ctx.role);
                json!({"jsonrpc": "2.0", "id": id, "result": { "tools": tools }})
            }
            "tools/call" => handle_call(&mut ctx, id, &request),
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("unknown method: {other}") }
            }),
        };

        let mut out = serde_json::to_string(&response)?;
        out.push('\n');
        stdout.write_all(out.as_bytes())?;
        stdout.flush()?;
    }
    Ok(())
}

/// tools/call dispatch.
fn handle_call(ctx: &mut Ctx, id: Value, request: &Value) -> Value {
    let name = request
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let empty = json!({});
    let args = request.pointer("/params/arguments").unwrap_or(&empty);

    let result = tools::dispatch(ctx, &name, args);
    match result {
        Ok(text) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "content": [{ "type": "text", "text": text }] }
        }),
        // `Refused` renders exactly like the `Args` refusals it replaced —
        // its `gate` is for the refusal log and never reaches the wire.
        Err(Error::Args(message)) | Err(Error::Refused { message, .. }) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("error: {message}") }],
                "isError": true
            }
        }),
        Err(error) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": {
                "content": [{ "type": "text", "text": format!("error: {error}") }],
                "isError": true
            }
        }),
    }
}
