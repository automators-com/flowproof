//! `flowproof mcp`: flowproof as a coding agent's own MCP tool server
//! (plans/017-flowproof-mcp-server.md). Not the hidden `mcp-stdio`, which
//! stands in for *someone else's* agent's tool server inside a flow.
//!
//! Newline-delimited JSON-RPC 2.0 over stdin/stdout, synchronous like the
//! rest of the engine. This is the protocol alone: the handshake, `ping`,
//! `tools/list` and the JSON-RPC errors. The tools follow in their own PRs,
//! and the command stays hidden until the first one lands.

use std::io::{BufRead, Write};

use flowproof_adapters::mcp_core::{coded_error_envelope, result_envelope};
use serde_json::{json, Value};

/// The revision advertised when the client asks for one this server does not
/// know. The older two are accepted as asked: the tools-only surface here is
/// the same in all three. Bumping this is its own PR (plan 17, decision 2).
const PROTOCOL_VERSION: &str = "2025-11-25";
const ACCEPTED_VERSIONS: [&str; 3] = [PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

const INSTRUCTIONS: &str = "flowproof records, replays and repairs end-to-end flows. \
A failing flow is a result to read, not an error. Credentials and the authoring model are \
set by a human with `flowproof config`; no tool here changes configuration, so stop and \
ask rather than work around a missing value.";

type Out = std::io::Stdout;

/// Serve until the client closes stdin.
pub fn serve() -> Result<(), String> {
    let mut out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| format!("reading stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(message) => handle(&mut out, message),
            Err(e) => send(
                &mut out,
                &coded_error_envelope(&Value::Null, -32700, &format!("parse error: {e}")),
            ),
        }
    }
    Ok(())
}

fn send(out: &mut Out, message: &Value) {
    // A closed stdout means the client has gone; there is no one to tell.
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// Answer one message.
fn handle(out: &mut Out, message: Value) {
    if !message.is_object() {
        let reply = coded_error_envelope(&Value::Null, -32600, "invalid request: not an object");
        send(out, &reply);
        return;
    }
    // No id: a notification (`notifications/initialized`) or a response to
    // nothing this server asked. Neither is answered.
    let Some(id) = message.get("id").cloned() else {
        return;
    };
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        send(
            out,
            &coded_error_envelope(&id, -32600, "invalid request: no method"),
        );
        return;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => send(out, &result_envelope(&id, &initialize(&params))),
        "ping" => send(out, &result_envelope(&id, &json!({}))),
        "tools/list" => send(out, &result_envelope(&id, &json!({ "tools": [] }))),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let reply = coded_error_envelope(&id, -32602, &format!("unknown tool: {name}"));
            send(out, &reply);
        }
        other => {
            let reply = coded_error_envelope(&id, -32601, &format!("method not found: {other}"));
            send(out, &reply);
        }
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| ACCEPTED_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "flowproof", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}
