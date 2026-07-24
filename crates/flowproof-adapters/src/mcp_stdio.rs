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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_runner::argv;
// The matching/mocking core is shared with the HTTP transport UNCHANGED, not
// forked: an HTTP lane and a stdio lane are indistinguishable, so the same
// functions decide both.
use crate::mcp_core::{
    error_envelope, match_call, mock_tool_result, notification_envelope, result_envelope,
    server_request_named,
};
pub use crate::mcp_core::{McpCall, McpDivergence, McpServerEvent};

/// In-flight requests keyed by canonical JSON-RPC id string, each carrying
/// its request sequence, method, and params for Thread B to correlate a
/// response against.
type Pending = HashMap<String, (usize, String, Value)>;

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
    /// Server-initiated notifications to re-emit, in capture order (replay
    /// only). ADDITIVE and skipped when empty, so a v3.1/v3.2 plan with no
    /// `events` key deserializes (empty) and re-serializes byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<McpServerEvent>,
}

/// The per-server outcome the stand-in writes and the orchestrator reads.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpOut {
    /// Calls captured, in request order (record).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub calls: Vec<McpCall>,
    /// Server-initiated notifications captured, in arrival order (record).
    /// ADDITIVE and skipped when empty, so a v3.1/v3.2 out with no `events`
    /// key deserializes (empty) and re-serializes byte-identical.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<McpServerEvent>,
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
///
/// Recorded server notifications ride the SAME pipe back to the agent: after
/// answering client call `k` every not-yet-emitted event whose `after <= k`
/// is written out (anchor-0 events before the first read, since the channel
/// already exists). Anchors are emission cues, never assertions - the
/// verdict still judges `calls` only.
fn run_replay(plan: &McpPlan, out_path: &Path) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();

    let mut lane = 0usize;
    let mut served = 0usize;
    let mut divergence: Option<McpDivergence> = None;
    // Which events have already been emitted; a flag per event, since events
    // are not necessarily ordered by `after`.
    let mut emitted = vec![false; plan.events.len()];
    // Anchor-0 events emit as soon as the channel exists: before the first
    // client line is read.
    emit_due_events(&mut writer, &plan.events, &mut emitted, served)?;
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
                    // The notifications due after this call cross now, on the
                    // heels of its response line.
                    emit_due_events(&mut writer, &plan.events, &mut emitted, served)?;
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
    // The lane counter, incremented once per client request in Thread A (the
    // single writer). Thread B reads it to anchor a server notification at
    // the count of calls issued when it crossed.
    let seq = Arc::new(AtomicUsize::new(0));
    // Server notifications captured in arrival order (Thread B), to re-emit
    // at replay.
    let events: Arc<Mutex<Vec<McpServerEvent>>> = Arc::new(Mutex::new(Vec::new()));
    // A server-initiated REQUEST mid-record is the one thing v3.3 does not
    // handle (it is v3.4): the first one seen names the record failure,
    // matching the HTTP transport, rather than corrupt the lane silently.
    let record_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let thread_b = {
        let write_lock = Arc::clone(&write_lock);
        let pending = Arc::clone(&pending);
        let calls = Arc::clone(&calls);
        let seq = Arc::clone(&seq);
        let events = Arc::clone(&events);
        let record_error = Arc::clone(&record_error);
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
                // Classify the server line by the JSON-RPC shape. It was
                // already forwarded verbatim above; here we decide what, if
                // anything, to CAPTURE from it.
                if let Ok(msg) = serde_json::from_str::<Value>(line.trim()) {
                    let has_method = msg.get("method").and_then(Value::as_str);
                    let has_id = msg.get("id");
                    match (has_method, has_id) {
                        // A server-initiated REQUEST (both a method to invoke
                        // and an id to answer): recording it is v3.4. Name the
                        // failure - the same message the HTTP transport uses -
                        // rather than mis-park the client's later answer and
                        // corrupt the lane. Keep forwarding so nothing
                        // deadlocks; the run is failed from the out file.
                        (Some(method), Some(_)) => {
                            let mut slot = record_error.lock().unwrap_or_else(|e| e.into_inner());
                            if slot.is_none() {
                                *slot = Some(server_request_named(method));
                            }
                        }
                        // A server NOTIFICATION (a method, no id): forwarded to
                        // the agent already, and captured as an event anchored
                        // at the count of calls issued when it crossed.
                        (Some(method), None) => {
                            let after = seq.load(Ordering::SeqCst);
                            let params = msg.get("params").cloned().unwrap_or(Value::Null);
                            events.lock().unwrap_or_else(|e| e.into_inner()).push(
                                McpServerEvent::notification(after, method.to_string(), params),
                            );
                        }
                        // A response (an id, no method): correlate it to its
                        // pending request by id and capture the triple.
                        (None, Some(id)) => {
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
                        (None, None) => {}
                    }
                }
            }
        })
    };

    // Thread A: the agent's stdin.
    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
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
            let this_seq = seq.fetch_add(1, Ordering::SeqCst);
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
            let this_seq = seq.fetch_add(1, Ordering::SeqCst);
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

    // A server-initiated request seen mid-record fails the record loudly,
    // with the named reason and no captured lane - the same posture the HTTP
    // transport takes.
    if let Some(err) = record_error
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
    {
        write_out_atomic(
            out_path,
            &McpOut {
                error: Some(err.clone()),
                ..Default::default()
            },
        )?;
        return Err(err);
    }

    let captured: Vec<McpCall> = calls
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect();
    let events: Vec<McpServerEvent> = events.lock().unwrap_or_else(|e| e.into_inner()).clone();
    write_out_atomic(
        out_path,
        &McpOut {
            calls: captured,
            events,
            ..Default::default()
        },
    )
}

/// Emit every not-yet-emitted event whose anchor has been reached
/// (`after <= served`), writing each as a JSON-RPC notification line. Called
/// after each answered call (and once at anchor 0), so a notification lands
/// right after the response of the call it followed.
fn emit_due_events<W: Write>(
    writer: &mut W,
    events: &[McpServerEvent],
    emitted: &mut [bool],
    served: usize,
) -> Result<(), String> {
    for (i, event) in events.iter().enumerate() {
        if !emitted[i] && event.after <= served {
            let line = notification_envelope(&event.method, &event.params).to_string();
            writeln!(writer, "{line}").map_err(|e| e.to_string())?;
            writer.flush().map_err(|e| e.to_string())?;
            emitted[i] = true;
        }
    }
    Ok(())
}

/// Write a JSON-RPC success response line and flush.
fn write_result<W: Write>(writer: &mut W, id: &Value, result: &Value) -> Result<(), String> {
    let line = result_envelope(id, result).to_string();
    writeln!(writer, "{line}").map_err(|e| e.to_string())?;
    writer.flush().map_err(|e| e.to_string())
}

/// Write a JSON-RPC error response line and flush, so a diverged or
/// past-the-end request does not leave the agent waiting forever.
fn write_error<W: Write>(writer: &mut W, id: &Value, detail: &str) -> Result<(), String> {
    let line = error_envelope(id, detail).to_string();
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

    /// Additivity: a `McpPlan` written before v3.3 (no `events` key)
    /// deserializes with an empty events vec and re-serializes byte-identical,
    /// so an old plan is unchanged by the new field.
    #[test]
    fn an_event_free_plan_round_trips_byte_identical() {
        let plan = McpPlan {
            mode: "replay".into(),
            command: String::new(),
            mocks: BTreeMap::new(),
            calls: vec![McpCall {
                method: "tools/call".into(),
                params: serde_json::json!({ "name": "get_weather" }),
                result: serde_json::json!({ "ok": true }),
            }],
            events: Vec::new(),
        };
        let json = serde_json::to_string(&plan).expect("serialize");
        assert!(!json.contains("events"), "no events key: {json}");
        let back: McpPlan = serde_json::from_str(&json).expect("deserialize");
        assert!(back.events.is_empty(), "events default empty");
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));

        // A hand-built pre-v3.3 plan with no events key at all deserializes.
        let old = r#"{"mode":"replay","calls":[]}"#;
        let back: McpPlan = serde_json::from_str(old).expect("old plan deserializes");
        assert!(back.events.is_empty());
    }

    /// Additivity: a `McpOut` with captured calls but no events serializes
    /// with no `events` key and round-trips byte-identical.
    #[test]
    fn an_event_free_out_round_trips_byte_identical() {
        let out = McpOut {
            calls: vec![McpCall {
                method: "initialize".into(),
                params: serde_json::json!({}),
                result: serde_json::json!({}),
            }],
            ..Default::default()
        };
        let json = serde_json::to_string(&out).expect("serialize");
        assert!(!json.contains("events"), "no events key: {json}");
        let back: McpOut = serde_json::from_str(&json).expect("deserialize");
        assert!(back.events.is_empty(), "events default empty");
        assert_eq!(json, serde_json::to_string(&back).expect("re-serialize"));
    }

    /// A `McpOut` carrying events serializes them and round-trips.
    #[test]
    fn an_out_with_events_carries_them() {
        let out = McpOut {
            calls: Vec::new(),
            events: vec![McpServerEvent::notification(
                2,
                "notifications/tools/list_changed".into(),
                serde_json::json!({}),
            )],
            ..Default::default()
        };
        let json = serde_json::to_string(&out).expect("serialize");
        assert!(json.contains("events"), "events present: {json}");
        let back: McpOut = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.events.len(), 1);
        assert_eq!(back.events[0].after, 2);
        assert_eq!(back.events[0].method, "notifications/tools/list_changed");
    }

    /// Due events emit once their anchor is reached, in order, each exactly
    /// once - the shared emission rule replay leans on.
    #[test]
    fn emit_due_events_fires_each_once_when_its_anchor_is_reached() {
        let events = vec![
            McpServerEvent::notification(0, "notifications/a".into(), serde_json::json!({})),
            McpServerEvent::notification(2, "notifications/b".into(), serde_json::json!({})),
        ];
        let mut emitted = vec![false; events.len()];
        let mut buf: Vec<u8> = Vec::new();

        // Anchor 0 is due at served 0; anchor 2 is not.
        emit_due_events(&mut buf, &events, &mut emitted, 0).expect("emit");
        let text = String::from_utf8(buf.clone()).expect("utf8");
        assert!(text.contains("notifications/a"), "{text}");
        assert!(!text.contains("notifications/b"), "{text}");

        // At served 1 still nothing new is due.
        emit_due_events(&mut buf, &events, &mut emitted, 1).expect("emit");
        // At served 2 the second is due, and the first does not repeat.
        buf.clear();
        emit_due_events(&mut buf, &events, &mut emitted, 2).expect("emit");
        let text = String::from_utf8(buf).expect("utf8");
        assert!(text.contains("notifications/b"), "{text}");
        assert_eq!(
            text.matches("notifications/a").count(),
            0,
            "no repeat: {text}"
        );
    }
}
