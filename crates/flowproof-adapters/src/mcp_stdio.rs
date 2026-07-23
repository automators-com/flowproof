//! The MCP boundary: a stdio JSON-RPC stand-in the agent spawns AS its MCP
//! server command. v1/v2 make the MODEL a record/replay boundary; this
//! makes an external MCP server one too.
//!
//! An agent whose tools are stdio MCP servers speaks JSON-RPC 2.0 over a
//! subprocess's stdin/stdout, newline-delimited, one message per line. The
//! SUT's MCP config points its server command at
//! `FLOWPROOF_MCP_SERVER_<NAME>` (`<flowproof-exe> mcp-stdio --server
//! <name>`), so flowproof stands in as the command the agent spawns:
//!
//! - RECORD forwards each JSON-RPC request to the REAL server, captures the
//!   `{method, params, result}` triple, and hands the server's own answer
//!   back to the agent. A tool the spec MOCKS is answered locally and never
//!   forwarded - the real server is never asked for it, which is how a
//!   dangerous tool is intercepted.
//! - REPLAY serves the recorded lane positionally, with ZERO external
//!   processes, mirroring the cassette's strict-positional, envelope-first
//!   divergence doctrine.
//!
//! The stand-in and the orchestrator (agent_flow) share a small file
//! contract in a per-run temp dir named by `FLOWPROOF_MCP_DIR`:
//!
//! - `<DIR>/<server>.plan.json` - written by the orchestrator BEFORE the
//!   agent is spawned: `{mode, command, mocks, calls}` (`calls` is the lane
//!   to serve, present for replay).
//! - `<DIR>/<server>.out.json` - written by the stand-in ATOMICALLY at stdin
//!   EOF: record -> `{calls: [...]}`; replay -> `{served, divergence}`.
//!
//! No async runtime, matching the model proxy's posture: std threads, plain
//! pipes, one blocking read loop per direction.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_runner::argv;

/// In-flight requests keyed by canonical JSON-RPC id string, each carrying
/// its request sequence, method, and params for Thread B to correlate a
/// response against.
type Pending = HashMap<String, (usize, String, Value)>;

/// One recorded MCP exchange: the request's method and params, and the
/// result served back. `arguments`/`params` live as JSON (not re-encoded
/// strings) because MCP carries them as JSON objects on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCall {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub result: Value,
}

/// The per-server plan the orchestrator writes and the stand-in reads.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpPlan {
    /// `"record"` or `"replay"`.
    pub mode: String,
    /// The command that starts the REAL server (record only; `${VAR}`s
    /// already resolved by the orchestrator).
    #[serde(default)]
    pub command: String,
    /// Tools mocked at the MCP boundary: name to result value.
    #[serde(default)]
    pub mocks: BTreeMap<String, Value>,
    /// The lane to serve, in order (replay only).
    #[serde(default)]
    pub calls: Vec<McpCall>,
}

/// Where a replay lane diverged: the 0-based lane index and the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDivergence {
    pub index: usize,
    pub detail: String,
}

/// The per-server outcome the stand-in writes and the orchestrator reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpOut {
    /// Calls captured, in request order (record).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<McpCall>,
    /// A setup failure the record-progress guard surfaces (record).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Lane entries served (replay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served: Option<usize>,
    /// The first (and only) lane divergence (replay).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub divergence: Option<McpDivergence>,
}

/// Run the stand-in for `server`, resolving its context from the
/// environment (`FLOWPROOF_MCP_DIR`, `FLOWPROOF_MCP_MODE`). Reads the plan,
/// bridges JSON-RPC in the chosen mode, and writes the out file. Returns an
/// error only for a setup failure the CLI turns into a non-zero exit; the
/// out file is written in every case so the orchestrator can see what
/// happened.
pub fn run_stand_in(server: &str) -> Result<(), String> {
    let dir = std::env::var("FLOWPROOF_MCP_DIR").map_err(|_| {
        "FLOWPROOF_MCP_DIR is not set: mcp-stdio is spawned by flowproof".to_string()
    })?;
    let mode = std::env::var("FLOWPROOF_MCP_MODE").map_err(|_| {
        "FLOWPROOF_MCP_MODE is not set: mcp-stdio is spawned by flowproof".to_string()
    })?;
    let dir = PathBuf::from(dir);
    let plan_path = dir.join(format!("{server}.plan.json"));
    let out_path = dir.join(format!("{server}.out.json"));

    let raw = std::fs::read_to_string(&plan_path)
        .map_err(|e| format!("reading MCP plan {}: {e}", plan_path.display()))?;
    let plan: McpPlan =
        serde_json::from_str(&raw).map_err(|e| format!("parsing MCP plan for `{server}`: {e}"))?;

    match mode.as_str() {
        "record" => run_record(&plan, &out_path),
        "replay" => run_replay(&plan, &out_path),
        other => Err(format!("unknown FLOWPROOF_MCP_MODE `{other}`")),
    }
}

/// REPLAY: one blocking read loop. Serve the plan's `calls` lane
/// positionally; on divergence or past-the-end write a JSON-RPC error (so
/// the agent does not hang), record it, and stop matching.
fn run_replay(plan: &McpPlan, out_path: &Path) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut lane = 0usize;
    let mut served = 0usize;
    let mut divergence: Option<McpDivergence> = None;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        // A client notification (no id) carries no response: consume it
        // silently, exactly as a real server would.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        if divergence.is_some() {
            // Past the first divergence the recording no longer describes
            // this run; keep the agent unblocked with an error, but do not
            // match or record further.
            write_error(&mut writer, &id, "flowproof: MCP replay already diverged")?;
            continue;
        }
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        match plan.calls.get(lane) {
            None => {
                let detail = format!(
                    "the agent made more MCP calls than the recording has ({} recorded)",
                    plan.calls.len()
                );
                write_error(&mut writer, &id, &detail)?;
                divergence = Some(McpDivergence {
                    index: lane,
                    detail,
                });
            }
            Some(recorded) => match match_call(recorded, method, msg.get("params")) {
                Ok(()) => {
                    write_result(&mut writer, &id, &recorded.result)?;
                    served += 1;
                    lane += 1;
                }
                Err(detail) => {
                    write_error(&mut writer, &id, &detail)?;
                    divergence = Some(McpDivergence {
                        index: lane,
                        detail,
                    });
                }
            },
        }
    }

    write_out_atomic(
        out_path,
        &McpOut {
            served: Some(served),
            divergence,
            ..Default::default()
        },
    )
}

/// RECORD: two threads bridge the agent and the REAL server.
///
/// Thread A (this thread) reads the agent's stdin: a mocked `tools/call` is
/// answered locally and recorded WITHOUT forwarding; every other request is
/// forwarded to the real server and parked in a pending map keyed by its
/// JSON-RPC id. Thread B reads the real server's stdout, forwards each line
/// to the agent, and correlates responses to their pending request by id,
/// recording the captured triple.
///
/// The two threads stay race-free without ordering the network:
/// - Request order is fixed by a `seq` counter incremented only in Thread A
///   (a single writer), so both mock answers and correlated real answers
///   land at their request position in a `BTreeMap<seq, McpCall>` regardless
///   of when the real response arrives.
/// - The pending insert in Thread A happens-before the forward to the
///   server, which happens-before the server's response, which
///   happens-before Thread B reads it - so the pending entry is always
///   visible when Thread B looks it up.
/// - A single stdout write mutex is shared between Thread A (mock answers)
///   and Thread B (real answers), so lines never interleave.
fn run_record(plan: &McpPlan, out_path: &Path) -> Result<(), String> {
    let parts = argv(&plan.command);
    let Some((program, args)) = parts.split_first() else {
        let msg = "the MCP server command is empty".to_string();
        write_out_atomic(
            out_path,
            &McpOut {
                error: Some(msg.clone()),
                ..Default::default()
            },
        )?;
        return Err(msg);
    };

    let child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // The child's stderr passes through to ours (which the agent
        // captured), so a server that explains itself stays audible.
        .stderr(Stdio::inherit())
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(e) => {
            let msg = format!("the real MCP server `{}` did not start: {e}", plan.command);
            write_out_atomic(
                out_path,
                &McpOut {
                    error: Some(msg.clone()),
                    ..Default::default()
                },
            )?;
            return Err(msg);
        }
    };

    // Only Thread A writes the server's stdin, so it stays owned here (no
    // lock); dropping it at EOF closes the pipe and lets the server exit.
    let mut server_stdin = child.stdin.take().ok_or("no child stdin")?;
    let server_stdout = child.stdout.take().ok_or("no child stdout")?;

    let write_lock = Arc::new(Mutex::new(std::io::stdout()));
    // id (canonical string) -> (request seq, method, params) for the
    // in-flight requests Thread B correlates responses against.
    let pending: Arc<Mutex<Pending>> = Arc::new(Mutex::new(HashMap::new()));
    // seq -> captured call, so a collect at the end yields request order.
    let calls: Arc<Mutex<BTreeMap<usize, McpCall>>> = Arc::new(Mutex::new(BTreeMap::new()));

    let thread_b = {
        let write_lock = Arc::clone(&write_lock);
        let pending = Arc::clone(&pending);
        let calls = Arc::clone(&calls);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(server_stdout);
            let mut line = String::new();
            loop {
                line.clear();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                if line.trim().is_empty() {
                    continue;
                }
                // Forward the server's line to the agent verbatim, so record
                // is transparent.
                {
                    let mut out = write_lock.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = out.write_all(line.as_bytes());
                    let _ = out.flush();
                }
                // Correlate a response to its pending request by id. A line
                // with no matching id (a server notification, say) is
                // forwarded but not recorded.
                if let Ok(msg) = serde_json::from_str::<Value>(line.trim()) {
                    if let Some(id) = msg.get("id") {
                        let key = id.to_string();
                        let entry = pending
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .remove(&key);
                        if let Some((seq, method, params)) = entry {
                            let result = msg.get("result").cloned().unwrap_or(Value::Null);
                            calls.lock().unwrap_or_else(|e| e.into_inner()).insert(
                                seq,
                                McpCall {
                                    method,
                                    params,
                                    result,
                                },
                            );
                        }
                    }
                }
            }
        })
    };

    // Thread A: the agent's stdin.
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let mut seq = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(line.trim()) else {
            // Not JSON we understand: pass it through to the real server.
            let _ = server_stdin.write_all(line.as_bytes());
            let _ = server_stdin.flush();
            continue;
        };
        // A notification (no id) is forwarded but never recorded: it has no
        // response, and replay consumes it silently.
        let Some(id) = msg.get("id").cloned() else {
            let _ = server_stdin.write_all(line.as_bytes());
            let _ = server_stdin.flush();
            continue;
        };
        let method = msg
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        let mocked_tool = (method == "tools/call")
            .then(|| params.get("name").and_then(Value::as_str))
            .flatten()
            .filter(|name| plan.mocks.contains_key(*name));

        if let Some(name) = mocked_tool {
            // Answer the dangerous tool locally and record it WITHOUT ever
            // asking the real server.
            let mock = plan.mocks.get(name).cloned().unwrap_or(Value::Null);
            let result = mock_tool_result(&mock);
            let this_seq = seq;
            seq += 1;
            calls.lock().unwrap_or_else(|e| e.into_inner()).insert(
                this_seq,
                McpCall {
                    method,
                    params,
                    result: result.clone(),
                },
            );
            let mut out = write_lock.lock().unwrap_or_else(|e| e.into_inner());
            write_result(&mut *out, &id, &result)?;
        } else {
            // Park it, THEN forward: the pending entry is in place before
            // the server can answer, so Thread B never misses the id.
            let this_seq = seq;
            seq += 1;
            pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(id.to_string(), (this_seq, method, params));
            let _ = server_stdin.write_all(line.as_bytes());
            let _ = server_stdin.flush();
        }
    }

    // The agent closed its side: close the server's stdin so it drains any
    // in-flight requests and exits, then let Thread B finish reading.
    drop(server_stdin);
    let _ = thread_b.join();
    let _ = child.wait();

    let captured: Vec<McpCall> = calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    write_out_atomic(
        out_path,
        &McpOut {
            calls: captured,
            ..Default::default()
        },
    )
}

/// Match an incoming request against the recorded lane entry, mirroring the
/// cassette's method-first, envelope-then-body doctrine. `initialize`
/// matches `protocolVersion` but NOT `clientInfo`/`capabilities` (the
/// "ignored knobs" precedent: they are recorded and reported, not matched).
fn match_call(recorded: &McpCall, method: &str, params: Option<&Value>) -> Result<(), String> {
    if method != recorded.method {
        return Err(format!(
            "method changed: recorded `{}`, replayed `{method}`",
            recorded.method
        ));
    }
    let null = Value::Null;
    let params = params.unwrap_or(&null);
    match method {
        "initialize" => {
            let want = recorded.params.get("protocolVersion");
            let got = params.get("protocolVersion");
            if want != got {
                return Err(format!(
                    "initialize protocolVersion changed: recorded {}, replayed {}",
                    show(want),
                    show(got)
                ));
            }
            Ok(())
        }
        "tools/call" => {
            let want_name = recorded.params.get("name").and_then(Value::as_str);
            let got_name = params.get("name").and_then(Value::as_str);
            if want_name != got_name {
                return Err(format!(
                    "tools/call name changed: recorded {}, replayed {}",
                    want_name
                        .map(|n| format!("`{n}`"))
                        .unwrap_or("absent".into()),
                    got_name
                        .map(|n| format!("`{n}`"))
                        .unwrap_or("absent".into()),
                ));
            }
            let want_args = recorded.params.get("arguments").unwrap_or(&null);
            let got_args = params.get("arguments").unwrap_or(&null);
            match json_diff_path(want_args, got_args, "arguments") {
                Some(detail) => Err(detail),
                None => Ok(()),
            }
        }
        _ => match json_diff_path(&recorded.params, params, "params") {
            Some(detail) => Err(detail),
            None => Ok(()),
        },
    }
}

/// The first path at which two JSON values diverge, named
/// (`arguments.city`, `params.items[2].id`). `None` when they are equal.
/// Deterministic: object keys are compared in sorted order.
fn json_diff_path(recorded: &Value, incoming: &Value, path: &str) -> Option<String> {
    if recorded == incoming {
        return None;
    }
    match (recorded, incoming) {
        (Value::Object(want), Value::Object(got)) => {
            let mut keys: Vec<&String> = want.keys().chain(got.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                match (want.get(key), got.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(detail) = json_diff_path(a, b, &format!("{path}.{key}")) {
                            return Some(detail);
                        }
                    }
                    (Some(_), None) => {
                        return Some(format!(
                            "{path}.{key} missing: recorded present, replayed absent"
                        ))
                    }
                    (None, Some(_)) => {
                        return Some(format!(
                            "{path}.{key} added: recorded absent, replayed present"
                        ))
                    }
                    (None, None) => {}
                }
            }
            None
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                return Some(format!(
                    "{path} length changed: recorded {}, replayed {}",
                    want.len(),
                    got.len()
                ));
            }
            for (i, (a, b)) in want.iter().zip(got).enumerate() {
                if let Some(detail) = json_diff_path(a, b, &format!("{path}[{i}]")) {
                    return Some(detail);
                }
            }
            None
        }
        _ => Some(format!(
            "{path} changed: recorded {}, replayed {}",
            abbreviate(&recorded.to_string()),
            abbreviate(&incoming.to_string())
        )),
    }
}

/// The MCP `tools/call` result shape for a mocked tool: a single text
/// content block carrying the mock. A string mock is used verbatim;
/// anything else is JSON-encoded. Built identically at record (the local
/// answer) and served verbatim at replay, so the two never disagree.
fn mock_tool_result(mock: &Value) -> Value {
    let text = match mock {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    serde_json::json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false,
    })
}

/// Render an optional JSON value for a divergence line.
fn show(value: Option<&Value>) -> String {
    value
        .map(Value::to_string)
        .unwrap_or_else(|| "absent".into())
}

/// Truncate a value's JSON for a diff line: the first divergent stretch is
/// what identifies it, same as the cassette's `abbreviate`.
fn abbreviate(text: &str) -> String {
    const LIMIT: usize = 160;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}...")
}

/// Write a JSON-RPC success response line and flush.
fn write_result<W: Write>(writer: &mut W, id: &Value, result: &Value) -> Result<(), String> {
    let line = serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
    writeln!(writer, "{line}").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

/// Write a JSON-RPC error response line and flush, so a diverged or
/// past-the-end request does not leave the agent waiting forever.
fn write_error<W: Write>(writer: &mut W, id: &Value, detail: &str) -> Result<(), String> {
    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": detail },
    })
    .to_string();
    writeln!(writer, "{line}").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

/// Write the out file atomically: a full write to a sibling temp path then a
/// rename, so the orchestrator that polls for it never reads a half-written
/// file.
fn write_out_atomic(path: &Path, out: &McpOut) -> Result<(), String> {
    let tmp = PathBuf::from(format!("{}.tmp", path.display()));
    let json = serde_json::to_string(out).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, json).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming to {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value, result: Value) -> McpCall {
        McpCall {
            method: method.into(),
            params,
            result,
        }
    }

    #[test]
    fn identical_tools_call_matches() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } }),
            Value::Null,
        );
        let incoming =
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } });
        assert!(match_call(&recorded, "tools/call", Some(&incoming)).is_ok());
    }

    #[test]
    fn a_changed_argument_names_the_first_divergent_path() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } }),
            Value::Null,
        );
        let incoming =
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("arguments.city"), "{err}");
        assert!(err.contains("Nairobi") && err.contains("Paris"), "{err}");
    }

    #[test]
    fn a_changed_method_is_named() {
        let recorded = call("tools/list", serde_json::json!({}), Value::Null);
        let err = match_call(&recorded, "resources/list", None).expect_err("must diverge");
        assert!(err.contains("method changed"), "{err}");
        assert!(
            err.contains("tools/list") && err.contains("resources/list"),
            "{err}"
        );
    }

    #[test]
    fn a_changed_tool_name_is_named_before_arguments() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "X" } }),
            Value::Null,
        );
        let incoming = serde_json::json!({ "name": "send_alert", "arguments": { "city": "Y" } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("name changed"), "{err}");
        assert!(!err.contains("arguments"), "name wins over args: {err}");
    }

    /// `initialize` matches `protocolVersion` but ignores `clientInfo` and
    /// `capabilities` - they are recorded and reported, never matched.
    #[test]
    fn initialize_matches_protocol_version_and_ignores_client_knobs() {
        let recorded = call(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "clientInfo": { "name": "recorder", "version": "1.0" },
                "capabilities": {},
            }),
            Value::Null,
        );
        // Different clientInfo and capabilities, same protocolVersion: match.
        let incoming = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "replayer", "version": "9.9" },
            "capabilities": { "roots": {} },
        });
        assert!(match_call(&recorded, "initialize", Some(&incoming)).is_ok());
        // A changed protocolVersion IS a divergence.
        let bumped = serde_json::json!({ "protocolVersion": "2025-06-18" });
        let err = match_call(&recorded, "initialize", Some(&bumped)).expect_err("must diverge");
        assert!(err.contains("protocolVersion"), "{err}");
    }

    #[test]
    fn a_string_mock_becomes_a_text_content_block_verbatim() {
        let result = mock_tool_result(&Value::String("sunny".into()));
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "sunny");
        assert_eq!(result["isError"], false);
    }

    #[test]
    fn a_structured_mock_is_json_encoded_in_the_text_block() {
        let result = mock_tool_result(&serde_json::json!({ "temp": 25, "sky": "clear" }));
        // serde_json sorts keys, so the encoding is canonical/deterministic.
        assert_eq!(result["content"][0]["text"], r#"{"sky":"clear","temp":25}"#);
    }

    #[test]
    fn a_missing_argument_is_named() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "t", "arguments": { "a": 1, "b": 2 } }),
            Value::Null,
        );
        let incoming = serde_json::json!({ "name": "t", "arguments": { "a": 1 } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("arguments.b"), "{err}");
        assert!(err.contains("missing"), "{err}");
    }
}
