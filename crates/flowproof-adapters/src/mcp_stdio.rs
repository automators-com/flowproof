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
// The matching/mocking core is shared with the HTTP transport UNCHANGED, not
// forked: an HTTP lane and a stdio lane are indistinguishable, so the same
// functions decide both.
use crate::mcp_core::{error_envelope, match_call, mock_tool_result, result_envelope};
pub use crate::mcp_core::{McpCall, McpDivergence};

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
