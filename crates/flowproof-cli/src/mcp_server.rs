//! `flowproof mcp`: flowproof as a coding agent's own MCP tool server
//! (plans/017-flowproof-mcp-server.md). Not the hidden `mcp-stdio`, which
//! stands in for *someone else's* agent's tool server inside a flow.
//!
//! Newline-delimited JSON-RPC 2.0 over stdin/stdout, synchronous like the
//! rest of the engine. Each `tools/call` is answered from its own thread, so
//! a long replay does not hold up the read loop; the tools themselves are in
//! `mcp_tools`.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use crate::mcp_tools;
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

type Out = Arc<Mutex<std::io::Stdout>>;

/// Serve until the client closes stdin, then wait for calls still running so
/// no child is left behind.
///
/// `embedded` is true under the Python package, whose `flowproof` command
/// runs this engine inside the interpreter: `current_exe()` is then Python,
/// not something that takes `run --json`, so the server needs
/// `FLOWPROOF_MCP_EXE` to know what to spawn.
pub fn serve(embedded: bool) -> Result<(), String> {
    if embedded && !exe_override_set() {
        return Err(
            "flowproof mcp needs the flowproof executable to run its tools, and \
            inside the Python package it cannot find one. Set FLOWPROOF_MCP_EXE to a \
            flowproof binary, or install flowproof from npm or cargo."
                .into(),
        );
    }
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    let mut running = Vec::new();
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| format!("reading stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(&line) {
            Ok(message) => running.extend(handle(&out, message)),
            Err(e) => send(
                &out,
                &coded_error_envelope(&Value::Null, -32700, &format!("parse error: {e}")),
            ),
        }
        running.retain(|call: &std::thread::JoinHandle<()>| !call.is_finished());
    }
    for call in running {
        let _ = call.join();
    }
    Ok(())
}

fn send(out: &Out, message: &Value) {
    let mut out = out.lock().unwrap_or_else(|p| p.into_inner());
    // A closed stdout means the client has gone; there is no one to tell.
    let _ = writeln!(out, "{message}");
    let _ = out.flush();
}

/// Answer one message. A `tools/call` is answered from its own thread, whose
/// handle is returned so `serve` can wait for it.
fn handle(out: &Out, message: Value) -> Option<std::thread::JoinHandle<()>> {
    if !message.is_object() {
        let reply = coded_error_envelope(&Value::Null, -32600, "invalid request: not an object");
        send(out, &reply);
        return None;
    }
    // No id: a notification (`notifications/initialized`) or a response to
    // nothing this server asked. Neither is answered.
    let id = message.get("id").cloned()?;
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        send(
            out,
            &coded_error_envelope(&id, -32600, "invalid request: no method"),
        );
        return None;
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => send(out, &result_envelope(&id, &initialize(&params))),
        "ping" => send(out, &result_envelope(&id, &json!({}))),
        "tools/list" => send(
            out,
            &result_envelope(&id, &json!({ "tools": mcp_tools::list() })),
        ),
        "tools/call" => {
            let out = Arc::clone(out);
            // A replay can take minutes; the read loop keeps serving meanwhile.
            return Some(std::thread::spawn(move || {
                let reply = match mcp_tools::call(&params) {
                    Ok(result) => result_envelope(&id, &result),
                    Err(message) => coded_error_envelope(&id, -32602, &message),
                };
                send(&out, &reply);
            }));
        }
        other => {
            let reply = coded_error_envelope(&id, -32601, &format!("method not found: {other}"));
            send(out, &reply);
        }
    }
    None
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

fn exe_override_set() -> bool {
    std::env::var("FLOWPROOF_MCP_EXE").is_ok_and(|path| !path.trim().is_empty())
}
