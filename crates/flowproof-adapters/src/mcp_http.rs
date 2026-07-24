//! The streamable-HTTP MCP boundary: an in-process loopback HTTP/1.1
//! listener the agent dials as its MCP server, hosted by the orchestrator
//! (agent_flow's `McpContext`) for the life of one run.
//!
//! This is the HTTP sibling of [`crate::mcp_stdio`], and it shares that
//! module's transport-independent core UNCHANGED via [`crate::mcp_core`]:
//! the same `match_call`, the same `mock_tool_result`, the same `McpCall`.
//! An HTTP lane and a stdio lane are therefore indistinguishable in the
//! trace, so a lane recorded through one transport replays through the
//! other.
//!
//! The posture is exactly [`crate::agent_proxy`]'s: a plain [`TcpListener`]
//! on `127.0.0.1`, a std-thread accept loop, `Connection: close` per
//! request, `ureq` for the one non-hermetic step (record forwarding to the
//! real server over TLS). There is NO separate process and NO plan/out
//! files: the listener reads served/divergence/captured from shared memory
//! (`Arc<Mutex<...>>`, the `ProxyLog` pattern) after the agent exits.
//!
//! The agent is served a single `application/json` JSON-RPC response in
//! BOTH phases - never SSE toward the agent. At record the REAL server may
//! answer over `text/event-stream`; those `data:` frames are parsed into
//! JSON-RPC messages and the one matching the request id is normalized into
//! that single JSON body. At replay there is zero network.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::agent_proxy::ProxyError;
use crate::mcp_core::McpDivergence;
use crate::mcp_core::{error_envelope, match_call, mock_tool_result, result_envelope, McpCall};

/// The largest request body the listener will read. MCP messages are small
/// (a tool call's arguments); this is a guard against a malformed
/// `content-length`, not a limit anyone should reach.
const MAX_BODY: usize = 8 * 1024 * 1024;

/// How long a record-mode forward to the real server may take, and how long
/// an SSE read may stall before it is abandoned. Bounds the one non-hermetic
/// step so a server that holds its stream open after answering cannot hang
/// the run; the read stops the instant the matching frame arrives, well
/// under this.
const FORWARD_TIMEOUT: Duration = Duration::from_secs(30);

/// The constant session id a replay hands back on `initialize`, so a client
/// that requires one has it. Never stored, never matched: the session id is
/// an ignored knob at both boundaries, like `clientInfo`.
const REPLAY_SESSION_ID: &str = "flowproof-replay";

/// What the listener observed while serving. Read after the run to decide
/// the verdict - the `ProxyLog` analogue for the MCP boundary.
#[derive(Debug, Default)]
pub struct McpHttpLog {
    /// Requests answered from the recorded lane (replay).
    pub served: usize,
    /// The first (and only) lane divergence (replay).
    pub divergence: Option<McpDivergence>,
    /// A named record failure: the one v3.2 does not handle (a
    /// server-initiated request mid-response). A recording is not minted
    /// when this is set.
    pub record_error: Option<String>,
}

/// A running in-process HTTP MCP listener. Dropping it stops the accept
/// loop.
pub struct McpHttpServer {
    addr: SocketAddr,
    log: Arc<Mutex<McpHttpLog>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Calls captured in record mode, in request order. Empty in replay.
    captured: Arc<Mutex<Vec<McpCall>>>,
}

/// Replay serves a recorded lane; record forwards to the real server and
/// captures. Both carry the per-server mocks, answered locally in either
/// phase and never forwarded.
enum Mode {
    Replay { calls: Vec<McpCall> },
    Record { upstream: String },
}

impl McpHttpServer {
    /// Start in REPLAY mode on `port` (`0` picks an ephemeral one), serving
    /// `calls` positionally with ZERO network. Mocks are answered locally.
    pub fn replay(
        calls: Vec<McpCall>,
        mocks: BTreeMap<String, Value>,
        port: u16,
    ) -> Result<Self, ProxyError> {
        Self::spawn(Mode::Replay { calls }, mocks, port)
    }

    /// Start in RECORD mode on `port` (`0` picks an ephemeral one),
    /// forwarding each request to the real server at `upstream` and
    /// capturing the `{method, params, result}` triple. A mocked
    /// `tools/call` is answered locally and NEVER forwarded.
    pub fn record(
        upstream: &str,
        mocks: BTreeMap<String, Value>,
        port: u16,
    ) -> Result<Self, ProxyError> {
        Self::spawn(
            Mode::Record {
                upstream: upstream.trim_end_matches('/').to_string(),
            },
            mocks,
            port,
        )
    }

    fn spawn(mode: Mode, mocks: BTreeMap<String, Value>, port: u16) -> Result<Self, ProxyError> {
        // Loopback always: an unauthenticated MCP endpoint must not be
        // reachable off the machine, whichever port it lands on.
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).map_err(|e| {
            // A taken FIXED port is the routine `port:` failure; name it. An
            // ephemeral (0) bind cannot hit AddrInUse, so it stays a bare io
            // error.
            if port != 0 && e.kind() == std::io::ErrorKind::AddrInUse {
                ProxyError::PortTaken(port)
            } else {
                ProxyError::Io(e)
            }
        })?;
        let addr = listener.local_addr()?;
        // Non-blocking accept so the loop notices `stop` even with no client.
        listener.set_nonblocking(true)?;

        let log = Arc::new(Mutex::new(McpHttpLog::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let (log, stop, captured) =
                (Arc::clone(&log), Arc::clone(&stop), Arc::clone(&captured));
            std::thread::spawn(move || {
                // Record capture-seq / replay lane index: the accept loop is
                // single-threaded and each POST is one request/response on
                // its own `Connection: close` socket, so the counter alone
                // fixes order - no pending map, unlike the async stdio path.
                let mut counter = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).ok();
                            serve_one(stream, &mode, &mocks, &mut counter, &log, &captured);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Ok(Self {
            addr,
            log,
            stop,
            thread: Some(thread),
            captured,
        })
    }

    /// The MCP endpoint URL to inject into the agent's environment.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// The calls captured in record mode, in request order. Empty in replay.
    pub fn captured(&self) -> Vec<McpCall> {
        self.captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, McpHttpLog> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The parts of a request the listener acts on.
struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    session_id: Option<String>,
    body: Vec<u8>,
}

/// Read one request, answer it, close. `Connection: close` every time, as
/// in the model proxy: keep-alive would buy nothing and multiplexing state
/// machines are where hand-rolled HTTP goes wrong.
fn serve_one(
    stream: TcpStream,
    mode: &Mode,
    mocks: &BTreeMap<String, Value>,
    counter: &mut usize,
    log: &Mutex<McpHttpLog>,
    captured: &Mutex<Vec<McpCall>>,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;

    let Some(request) = read_request(&mut reader) else {
        respond(
            &mut writer,
            &mut reader,
            &response(400, r#"{"error":"malformed request"}"#),
        );
        return;
    };

    // Substring-tolerant path match: the agent posts to the `/mcp` URL we
    // handed it, but a client that appends a segment still routes.
    if !request.path.contains("/mcp") {
        respond(
            &mut writer,
            &mut reader,
            &response(404, r#"{"error":"only the /mcp endpoint is served"}"#),
        );
        return;
    }

    match request.method.as_str() {
        // A standalone GET is the server-push SSE channel of the older
        // transport; v3.2 never opens one toward the agent and does not
        // serve one. (D5)
        "GET" => respond(
            &mut writer,
            &mut reader,
            &response(
                405,
                r#"{"error":"flowproof serves no server-push SSE stream (v3.2)"}"#,
            ),
        ),
        // Session teardown: answered locally in both phases. There is
        // nothing to tear down at replay, and at record the real session
        // drops when the run ends.
        "DELETE" => respond(&mut writer, &mut reader, &response(200, "")),
        "POST" => serve_post(
            &request,
            mode,
            mocks,
            counter,
            log,
            captured,
            &mut writer,
            &mut reader,
        ),
        _ => respond(
            &mut writer,
            &mut reader,
            &response(405, r#"{"error":"method not allowed"}"#),
        ),
    }
}

/// The POST path: the MCP JSON-RPC exchange. Split out so the method
/// dispatch above stays readable.
#[allow(clippy::too_many_arguments)]
fn serve_post(
    request: &Request,
    mode: &Mode,
    mocks: &BTreeMap<String, Value>,
    counter: &mut usize,
    log: &Mutex<McpHttpLog>,
    captured: &Mutex<Vec<McpCall>>,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    let json: Value = match serde_json::from_slice(&request.body) {
        Ok(json) => json,
        Err(e) => {
            respond(
                writer,
                reader,
                &response(400, &error_body(&format!("request is not JSON: {e}"))),
            );
            return;
        }
    };

    // A JSON-RPC batch (a top-level array) is a v3.3 punt, not silently
    // half-handled: name it. (D5)
    if json.is_array() {
        respond(
            writer,
            reader,
            &response(
                400,
                &error_body(
                    "JSON-RPC batch requests (a top-level array) are not recorded in v3.2; \
                     send one request per POST",
                ),
            ),
        );
        return;
    }

    let method = json.get("method").and_then(Value::as_str).unwrap_or("");
    let params = json.get("params").cloned().unwrap_or(Value::Null);

    // A notification (no id) carries no response. At record it is forwarded
    // to the real server (fire and forget) and answered 202; at replay it is
    // answered 202 with no network. Never recorded.
    let Some(id) = json.get("id").cloned() else {
        if let Mode::Record { upstream } = mode {
            let _ = forward(
                upstream,
                request.authorization.as_deref(),
                request.session_id.as_deref(),
                &request.body,
            );
        }
        respond(writer, reader, &response(202, ""));
        return;
    };

    match mode {
        Mode::Replay { calls } => {
            serve_replay(calls, method, &params, &id, counter, log, writer, reader)
        }
        Mode::Record { upstream } => serve_record(
            upstream, request, method, &params, &id, mocks, captured, log, writer, reader,
        ),
    }
}

/// REPLAY: match this request positionally against the recorded lane; answer
/// the recorded result, or a JSON-RPC error (in-band, HTTP 200) on divergence
/// or past-the-end, recording the divergence.
#[allow(clippy::too_many_arguments)]
fn serve_replay(
    calls: &[McpCall],
    method: &str,
    params: &Value,
    id: &Value,
    lane: &mut usize,
    log: &Mutex<McpHttpLog>,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    // Past the first divergence the recording no longer describes this run;
    // keep the agent unblocked with an error, but do not match or record on.
    if log
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .divergence
        .is_some()
    {
        let body = error_envelope(id, "flowproof: MCP replay already diverged").to_string();
        respond(
            writer,
            reader,
            &json_response(200, &body, session_for(method)),
        );
        return;
    }

    // The bookkeeping happens BEFORE the response: `respond` shuts the write
    // half, and the client's read returns the instant it does, so a caller
    // that reads the log right after must see served/divergence already
    // updated - not a beat later.
    match calls.get(*lane) {
        None => {
            let detail = format!(
                "the agent made more MCP calls than the recording has ({} recorded)",
                calls.len()
            );
            set_divergence(log, *lane, detail.clone());
            let body = error_envelope(id, &detail).to_string();
            respond(
                writer,
                reader,
                &json_response(200, &body, session_for(method)),
            );
        }
        Some(recorded) => match match_call(recorded, method, Some(params)) {
            Ok(()) => {
                log.lock().unwrap_or_else(|e| e.into_inner()).served += 1;
                *lane += 1;
                let body = result_envelope(id, &recorded.result).to_string();
                respond(
                    writer,
                    reader,
                    &json_response(200, &body, session_for(method)),
                );
            }
            Err(detail) => {
                set_divergence(log, *lane, detail.clone());
                let body = error_envelope(id, &detail).to_string();
                respond(
                    writer,
                    reader,
                    &json_response(200, &body, session_for(method)),
                );
            }
        },
    }
}

/// RECORD: a mocked `tools/call` is answered locally and captured WITHOUT
/// forwarding; every other request is forwarded to the real server, the
/// `{method, params, result}` triple captured, and the server's own answer
/// (normalized to a single JSON body) handed back to the agent.
#[allow(clippy::too_many_arguments)]
fn serve_record(
    upstream: &str,
    request: &Request,
    method: &str,
    params: &Value,
    id: &Value,
    mocks: &BTreeMap<String, Value>,
    captured: &Mutex<Vec<McpCall>>,
    log: &Mutex<McpHttpLog>,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    let mocked_tool = (method == "tools/call")
        .then(|| params.get("name").and_then(Value::as_str))
        .flatten()
        .filter(|name| mocks.contains_key(*name));

    if let Some(name) = mocked_tool {
        // Answer the dangerous tool locally and record it WITHOUT ever asking
        // the real server.
        let mock = mocks.get(name).cloned().unwrap_or(Value::Null);
        let result = mock_tool_result(&mock);
        captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(McpCall {
                method: method.to_string(),
                params: params.clone(),
                result: result.clone(),
            });
        let body = result_envelope(id, &result).to_string();
        respond(
            writer,
            reader,
            &json_response(200, &body, session_for(method)),
        );
        return;
    }

    match forward(
        upstream,
        request.authorization.as_deref(),
        request.session_id.as_deref(),
        &request.body,
    ) {
        ForwardResult::Ok {
            message,
            session_id,
        } => {
            let result = message.get("result").cloned().unwrap_or(Value::Null);
            captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(McpCall {
                    method: method.to_string(),
                    params: params.clone(),
                    result,
                });
            // Hand the server's own normalized answer back to the agent, and
            // pass its session id through so a client that needs one has it.
            let body = message.to_string();
            respond(
                writer,
                reader,
                &json_response(200, &body, session_id.as_deref()),
            );
        }
        ForwardResult::ServerRequest(named) => {
            let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
            if log.record_error.is_none() {
                log.record_error = Some(named.clone());
            }
            drop(log);
            respond(
                writer,
                reader,
                &json_response(200, &error_envelope(id, &named).to_string(), None),
            );
        }
        ForwardResult::Failed(why) => {
            let detail = format!("the real MCP server call failed: {why}");
            respond(
                writer,
                reader,
                &json_response(200, &error_envelope(id, &detail).to_string(), None),
            );
        }
    }
}

/// The constant replay session id, but only on `initialize` (where a client
/// first learns it). Other responses carry none.
fn session_for(method: &str) -> Option<&'static str> {
    (method == "initialize").then_some(REPLAY_SESSION_ID)
}

/// Record the first divergence (idempotent: the first wins).
fn set_divergence(log: &Mutex<McpHttpLog>, index: usize, detail: String) {
    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
    if log.divergence.is_none() {
        log.divergence = Some(McpDivergence { index, detail });
    }
}

/// The outcome of forwarding one request to the real server.
enum ForwardResult {
    /// The matched JSON-RPC response message, plus the server's session id
    /// header if it set one.
    Ok {
        message: Value,
        session_id: Option<String>,
    },
    /// The real server sent a server-initiated request (sampling /
    /// elicitation) mid-response: the one thing v3.2 cannot record.
    ServerRequest(String),
    /// A transport or parse failure reaching the real server.
    Failed(String),
}

/// Forward the agent's exact POST body to the real server, passing through
/// its `Authorization` and `Mcp-Session-Id` headers (headers are never
/// stored). The real server may answer `application/json` (one message) or
/// `text/event-stream` (SSE `data:` frames); either way the message whose
/// id matches the request is returned as a single JSON value.
fn forward(
    upstream: &str,
    auth: Option<&str>,
    session: Option<&str>,
    body: &[u8],
) -> ForwardResult {
    // Parse the request id up front: an SSE stream carries the response
    // interleaved with notifications, and the id is how the response frame is
    // told from the rest.
    let request_id = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|v| v.get("id").cloned());

    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_global(Some(FORWARD_TIMEOUT))
        .build();
    let agent = config.new_agent();
    let mut req = agent
        .post(upstream)
        .header("content-type", "application/json")
        // Streamable HTTP servers negotiate the response shape on Accept.
        .header("accept", "application/json, text/event-stream");
    if let Some(auth) = auth {
        req = req.header("authorization", auth);
    }
    if let Some(session) = session {
        req = req.header("mcp-session-id", session);
    }

    let mut response = match req.send(body) {
        Ok(response) => response,
        Err(e) => return ForwardResult::Failed(e.to_string()),
    };

    // The session id (when the server assigns one) and the content type,
    // read before the body borrow.
    let session_id = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.contains("text/event-stream"))
        .unwrap_or(false);

    if is_sse {
        let mut reader = BufReader::new(response.body_mut().as_reader());
        read_sse_message(&mut reader, request_id.as_ref(), session_id)
    } else {
        let raw = match response.body_mut().read_to_string() {
            Ok(raw) => raw,
            Err(e) => return ForwardResult::Failed(e.to_string()),
        };
        match serde_json::from_str::<Value>(&raw) {
            Ok(message) => ForwardResult::Ok {
                message,
                session_id,
            },
            Err(e) => ForwardResult::Failed(format!("real server returned non-JSON: {e}")),
        }
    }
}

/// Parse an SSE body into JSON-RPC messages and return the one whose id
/// matches the request. Stops at that frame under the read timeout - it does
/// NOT wait for the stream to close. A notification (no id) is dropped; a
/// server-initiated request (a message carrying both `method` and `id`) is
/// the named v3.2 record failure.
fn read_sse_message(
    reader: &mut impl BufRead,
    request_id: Option<&Value>,
    session_id: Option<String>,
) -> ForwardResult {
    let mut data = String::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => return ForwardResult::Failed(format!("reading the SSE stream: {e}")),
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            // Frame boundary: parse what we have accumulated.
            if !data.is_empty() {
                match classify_sse_frame(&data, request_id) {
                    SseFrame::Response(message) => {
                        return ForwardResult::Ok {
                            message,
                            session_id,
                        }
                    }
                    SseFrame::ServerRequest(named) => return ForwardResult::ServerRequest(named),
                    SseFrame::Skip => {}
                }
                data.clear();
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            // A leading space after `data:` is part of the SSE framing, not
            // the payload.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
        // `event:`, `id:`, `retry:` and comments carry no JSON-RPC payload.
    }
    // A trailing frame with no closing blank line.
    if !data.is_empty() {
        match classify_sse_frame(&data, request_id) {
            SseFrame::Response(message) => {
                return ForwardResult::Ok {
                    message,
                    session_id,
                }
            }
            SseFrame::ServerRequest(named) => return ForwardResult::ServerRequest(named),
            SseFrame::Skip => {}
        }
    }
    ForwardResult::Failed(
        "the real MCP server's SSE stream ended before answering the request".to_string(),
    )
}

/// One classified SSE frame.
enum SseFrame {
    Response(Value),
    ServerRequest(String),
    Skip,
}

/// Classify one SSE `data:` payload against the request id.
fn classify_sse_frame(data: &str, request_id: Option<&Value>) -> SseFrame {
    let Ok(message) = serde_json::from_str::<Value>(data) else {
        return SseFrame::Skip;
    };
    let has_method = message.get("method").and_then(Value::as_str);
    let has_id = message.get("id");
    match (has_method, has_id) {
        // A server-initiated request: it has both a method to invoke and an
        // id to answer. Recording it is v3.3.
        (Some(method), Some(_)) => SseFrame::ServerRequest(format!(
            "the real MCP server sent a server-initiated request (`{method}`) mid-response; \
             recording server-initiated traffic is v3.3"
        )),
        // A server notification: a method, no id. Dropped.
        (Some(_), None) => SseFrame::Skip,
        // A response: an id, no method. Ours iff the id matches.
        (None, Some(mid)) => {
            if request_id == Some(mid) {
                SseFrame::Response(message)
            } else {
                SseFrame::Skip
            }
        }
        (None, None) => SseFrame::Skip,
    }
}

/// Read the request line, headers, and exactly `content-length` bytes.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut length = 0usize;
    let mut authorization = None;
    let mut session_id = None;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok()?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_string());
            } else if name.eq_ignore_ascii_case("mcp-session-id") {
                session_id = Some(value.trim().to_string());
            }
        }
    }
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        authorization,
        session_id,
        body,
    })
}

/// Send a response and close the write half cleanly - the same orderly-FIN
/// shutdown the model proxy uses, so a client on Windows is not handed an
/// RST in place of its answer.
fn respond(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, bytes: &[u8]) {
    let _ = writer.write_all(bytes);
    let _ = writer.flush();
    let _ = writer.shutdown(std::net::Shutdown::Write);
    let mut sink = [0u8; 4096];
    for _ in 0..16 {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// A JSON response with an optional `Mcp-Session-Id` header.
fn json_response(status: u16, body: &str, session_id: Option<&str>) -> Vec<u8> {
    let session = session_id
        .map(|s| format!("mcp-session-id: {s}\r\n"))
        .unwrap_or_default();
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n{session}\
         content-length: {len}\r\nconnection: close\r\n\r\n{body}",
        reason = reason(status),
        len = body.len(),
    )
    .into_bytes()
}

/// A JSON response with no extra headers.
fn response(status: u16, body: &str) -> Vec<u8> {
    json_response(status, body, None)
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "OK",
    }
}

fn error_body(message: &str) -> String {
    serde_json::json!({ "error": { "type": "flowproof_mcp", "message": message } }).to_string()
}

#[cfg(test)]
mod tests {
    // `super::*` already brings `Read`/`Write`/`BufRead` and `TcpStream`
    // into scope for the socket client below.
    use super::*;

    /// A minimal MCP client: POST a JSON-RPC body to the `/mcp` endpoint,
    /// return `(status, parsed body, mcp-session-id header)`.
    fn post(url: &str, payload: Value) -> (u16, Value, Option<String>) {
        post_raw(url, "POST", &payload.to_string())
    }

    /// POST/GET/DELETE a raw body to a `http://addr/mcp` url over a socket.
    fn post_raw(url: &str, method: &str, body: &str) -> (u16, Value, Option<String>) {
        let addr = url.trim_start_matches("http://");
        let (addr, path) = addr
            .split_once('/')
            .map(|(a, p)| (a, format!("/{p}")))
            .expect("url has a path");
        let mut stream = TcpStream::connect(addr).expect("connect");
        let request = format!(
            "{method} {path} HTTP/1.1\r\nhost: {addr}\r\ncontent-type: application/json\r\n\
             content-length: {}\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(request.as_bytes()).expect("write");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read");
        let status = raw
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .expect("status");
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or(("", ""));
        let session = head.lines().find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.eq_ignore_ascii_case("mcp-session-id")
                .then(|| v.trim().to_string())
        });
        (
            status,
            serde_json::from_str(body).unwrap_or(Value::Null),
            session,
        )
    }

    fn call(method: &str, params: Value, result: Value) -> McpCall {
        McpCall {
            method: method.into(),
            params,
            result,
        }
    }

    /// A recorded lane replays over HTTP as single JSON bodies, positionally,
    /// with zero network - and `initialize` carries the constant session id.
    #[test]
    fn a_recorded_lane_replays_over_http() {
        let lane = vec![
            call(
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05" }),
                serde_json::json!({ "protocolVersion": "2024-11-05", "serverInfo": {} }),
            ),
            call(
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
                serde_json::json!({ "content": [{ "type": "text", "text": "sunny" }] }),
            ),
        ];
        let server = McpHttpServer::replay(lane, BTreeMap::new(), 0).expect("starts");
        let url = server.url();

        let (status, body, session) = post(
            &url,
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2024-11-05", "clientInfo": { "name": "x" } } }),
        );
        assert_eq!(status, 200);
        assert_eq!(body["result"]["serverInfo"], serde_json::json!({}));
        assert_eq!(
            session.as_deref(),
            Some(REPLAY_SESSION_ID),
            "initialize session id"
        );

        let (status, body, _) = post(
            &url,
            serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "get_weather", "arguments": { "city": "Paris" } } }),
        );
        assert_eq!(status, 200);
        assert_eq!(body["result"]["content"][0]["text"], "sunny");
        assert_eq!(server.log().served, 2);
        assert!(server.log().divergence.is_none());
    }

    /// A replay whose tool argument changed gets a JSON-RPC error in a 200
    /// body (in-band, no 409) and records the divergence naming the path.
    #[test]
    fn a_divergent_replay_answers_in_band_and_records_it() {
        let lane = vec![call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
            serde_json::json!({ "content": [] }),
        )];
        let server = McpHttpServer::replay(lane, BTreeMap::new(), 0).expect("starts");
        let (status, body, _) = post(
            &server.url(),
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "get_weather", "arguments": { "city": "Berlin" } } }),
        );
        assert_eq!(status, 200, "in-band, not a 409");
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("arguments.city"));
        let log = server.log();
        assert!(log.divergence.is_some());
        assert_eq!(log.served, 0);
    }

    /// A mocked tool is served locally at replay from the recorded lane (the
    /// mock was captured at record).
    #[test]
    fn a_notification_gets_202_and_a_get_gets_405() {
        let server = McpHttpServer::replay(Vec::new(), BTreeMap::new(), 0).expect("starts");
        let (status, _, _) = post(
            &server.url(),
            serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        assert_eq!(status, 202, "notification");

        let (status, _, _) = post_raw(&server.url(), "GET", "");
        assert_eq!(status, 405, "standalone GET SSE is not served");

        let (status, _, _) = post_raw(&server.url(), "DELETE", "");
        assert_eq!(status, 200, "session teardown answered locally");
    }

    /// A JSON-RPC batch (top-level array) is a named 400, never silently
    /// half-handled.
    #[test]
    fn a_batch_post_is_a_named_400() {
        let server = McpHttpServer::replay(Vec::new(), BTreeMap::new(), 0).expect("starts");
        let (status, body, _) = post_raw(
            &server.url(),
            "POST",
            &serde_json::json!([{ "jsonrpc": "2.0", "id": 1, "method": "ping" }]).to_string(),
        );
        assert_eq!(status, 400);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("batch"));
    }

    /// Past-the-end: more calls than the recording answers a JSON-RPC error
    /// and records the divergence.
    #[test]
    fn past_the_end_is_a_recorded_divergence() {
        let server = McpHttpServer::replay(Vec::new(), BTreeMap::new(), 0).expect("starts");
        let (status, body, _) = post(
            &server.url(),
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "ping", "params": {} }),
        );
        assert_eq!(status, 200);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("more MCP calls"));
        assert!(server.log().divergence.is_some());
    }
}
