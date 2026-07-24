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
//! The agent is served a single `application/json` JSON-RPC response to each
//! POST in BOTH phases - never SSE on the POST reply toward the agent. At
//! record the REAL server may answer a POST over `text/event-stream`; those
//! `data:` frames are parsed into JSON-RPC messages and the one matching the
//! request id is normalized into that single JSON body. At replay there is
//! zero network.
//!
//! v3.3 adds server-initiated NOTIFICATIONS (a `method`, no `id`). At record
//! they are captured wherever they cross: inline in a POST's SSE body, or on
//! the standalone GET SSE stream the agent may open (which flowproof bridges
//! to a matching upstream GET on its own thread). At replay the recorded
//! notifications ride back out that same standalone GET stream, anchored to
//! the client-call count so each lands where it was recorded. Anchors are an
//! emission cue, never an assertion - the verdict still judges `calls` only.
//! Each accepted connection is served on its own thread: POSTs serialize
//! under a single lock (preserving the ordered lane the counter relies on),
//! while the long-lived GET stream runs outside it. A server-initiated
//! REQUEST (sampling / elicitation) stays the named v3.4 record failure.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;

use crate::agent_proxy::ProxyError;
use crate::mcp_core::{
    error_envelope, match_call, mock_tool_result, notification_envelope, result_envelope,
    server_request_named, McpCall,
};
use crate::mcp_core::{McpDivergence, McpServerEvent};

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
/// loop (and, via `stop`, any long-lived GET stream thread).
pub struct McpHttpServer {
    addr: SocketAddr,
    ctx: Arc<Ctx>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Replay serves a recorded lane; record forwards to the real server and
/// captures. Both carry the per-server mocks, answered locally in either
/// phase and never forwarded. Replay also carries the recorded server
/// notifications to re-emit over the standalone GET stream.
enum Mode {
    Replay {
        calls: Vec<McpCall>,
        events: Vec<McpServerEvent>,
    },
    Record {
        upstream: String,
    },
}

/// Everything a connection thread shares, behind one `Arc`. The POST lock
/// both serializes POST handling and guards the lane/capture counter it
/// wraps, so the ordered lane the v3.2 single-threaded loop relied on
/// survives per-connection threading; the GET stream never takes it.
struct Ctx {
    mode: Mode,
    mocks: BTreeMap<String, Value>,
    log: Mutex<McpHttpLog>,
    /// Calls captured in record mode, in request order. Empty in replay.
    captured: Mutex<Vec<McpCall>>,
    /// The POST lane/capture counter, and the lock that serializes POSTs.
    post: Mutex<usize>,
    /// Server notifications captured in record mode (POST-inline and
    /// standalone GET), in arrival order. Empty in replay.
    rec_events: Mutex<Vec<McpServerEvent>>,
    /// Replay: whether a standalone GET SSE stream is already open, so a
    /// second concurrent GET is answered `409` rather than duplicated.
    get_open: AtomicBool,
    stop: Arc<AtomicBool>,
}

impl McpHttpServer {
    /// Start in REPLAY mode on `port` (`0` picks an ephemeral one), serving
    /// `calls` positionally with ZERO network and re-emitting `events` over
    /// the standalone GET stream. Mocks are answered locally.
    pub fn replay(
        calls: Vec<McpCall>,
        events: Vec<McpServerEvent>,
        mocks: BTreeMap<String, Value>,
        port: u16,
    ) -> Result<Self, ProxyError> {
        Self::spawn(Mode::Replay { calls, events }, mocks, port)
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

        let ctx = Arc::new(Ctx {
            mode,
            mocks,
            log: Mutex::new(McpHttpLog::default()),
            captured: Mutex::new(Vec::new()),
            post: Mutex::new(0),
            rec_events: Mutex::new(Vec::new()),
            get_open: AtomicBool::new(false),
            stop: Arc::new(AtomicBool::new(false)),
        });
        let thread = {
            let ctx = Arc::clone(&ctx);
            std::thread::spawn(move || {
                // Each accepted connection is served on its OWN thread: a POST
                // is quick and takes the post-lock; the GET stream is
                // long-lived and must not block a following POST, so it runs
                // on its own thread outside the lock. The threads are
                // detached - a GET stream thread ends on its own when `stop`
                // is set (drop) or its client hangs up.
                while !ctx.stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).ok();
                            let ctx = Arc::clone(&ctx);
                            std::thread::spawn(move || serve_conn(stream, &ctx));
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
            ctx,
            thread: Some(thread),
        })
    }

    /// The MCP endpoint URL to inject into the agent's environment.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }

    /// The calls captured in record mode, in request order. Empty in replay.
    pub fn captured(&self) -> Vec<McpCall> {
        self.ctx
            .captured
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// The server notifications captured in record mode, in arrival order.
    /// Empty in replay.
    pub fn events(&self) -> Vec<McpServerEvent> {
        self.ctx
            .rec_events
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, McpHttpLog> {
        self.ctx.log.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.ctx.stop.store(true, Ordering::Relaxed);
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

/// Read one request and dispatch by method. `Connection: close` on the POST
/// and DELETE replies, as in the model proxy; the GET stream is the one
/// long-lived connection.
fn serve_conn(stream: TcpStream, ctx: &Ctx) {
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
        // The standalone GET SSE stream: at record it bridges to a matching
        // upstream GET and captures notifications; at replay it re-emits the
        // recorded ones. Held open for the run, so it runs OUTSIDE the
        // post-lock. (D5)
        "GET" => serve_get(&request, ctx, &mut writer, &mut reader),
        // Session teardown: answered locally in both phases. There is
        // nothing to tear down at replay, and at record the real session
        // drops when the run ends.
        "DELETE" => respond(&mut writer, &mut reader, &response(200, "")),
        // Serialize POSTs under the post-lock, which also guards the
        // lane/capture counter: the ordered lane the v3.2 single-threaded
        // loop relied on survives per-connection threading.
        "POST" => {
            let mut counter = ctx.post.lock().unwrap_or_else(|e| e.into_inner());
            serve_post(&request, ctx, &mut counter, &mut writer, &mut reader);
        }
        _ => respond(
            &mut writer,
            &mut reader,
            &response(405, r#"{"error":"method not allowed"}"#),
        ),
    }
}

/// The POST path: the MCP JSON-RPC exchange. Split out so the method
/// dispatch above stays readable.
fn serve_post(
    request: &Request,
    ctx: &Ctx,
    counter: &mut usize,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    let mode = &ctx.mode;
    let mocks = &ctx.mocks;
    let log = &ctx.log;
    let captured = &ctx.captured;
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
            let anchor = captured.lock().unwrap_or_else(|e| e.into_inner()).len();
            let _ = forward(
                upstream,
                request.authorization.as_deref(),
                request.session_id.as_deref(),
                &request.body,
                &ctx.rec_events,
                anchor,
            );
        }
        respond(writer, reader, &response(202, ""));
        return;
    };

    match mode {
        Mode::Replay { calls, .. } => {
            serve_replay(calls, method, &params, &id, counter, log, writer, reader)
        }
        Mode::Record { upstream } => serve_record(
            upstream,
            request,
            method,
            &params,
            &id,
            mocks,
            captured,
            &ctx.rec_events,
            log,
            writer,
            reader,
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
    rec_events: &Mutex<Vec<McpServerEvent>>,
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

    // Anchor a notification inline in this POST's SSE body to the count of
    // calls captured BEFORE this one - which is this call's own index, since
    // POSTs are serialized and only they push calls. The response is pushed
    // after the forward returns, so the notification lands just ahead of it.
    let anchor = captured.lock().unwrap_or_else(|e| e.into_inner()).len();
    match forward(
        upstream,
        request.authorization.as_deref(),
        request.session_id.as_deref(),
        &request.body,
        rec_events,
        anchor,
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

/// The standalone GET SSE stream (D5). At record it bridges to a matching
/// upstream GET and captures the server's notifications; at replay it
/// re-emits the recorded ones as the client-call count reaches their anchor.
/// Runs OUTSIDE the post-lock, so a following POST is never blocked by it.
fn serve_get(
    request: &Request,
    ctx: &Ctx,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    match &ctx.mode {
        Mode::Replay { events, .. } => serve_get_replay(events, ctx, writer, reader),
        Mode::Record { upstream } => serve_get_record(upstream, request, ctx, writer),
    }
}

/// The SSE response headers for the standalone GET stream: no
/// `content-length`, held open, `Connection: close`.
const SSE_STREAM_HEADER: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
     cache-control: no-cache\r\nconnection: close\r\n\r\n";

/// REPLAY GET: hold the stream open and flush each recorded notification when
/// the served-call count reaches its anchor (`after <= served`; anchor-0 goes
/// the instant the stream exists). A SECOND concurrent GET is a `409`. The
/// loop ends when the run ends (`stop`) or the client hangs up; an agent that
/// never opens the stream leaves the events undelivered without hanging or
/// failing the run.
fn serve_get_replay(
    events: &[McpServerEvent],
    ctx: &Ctx,
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) {
    // At most one push stream at a time: a second is a 409, not a duplicate.
    if ctx
        .get_open
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        respond(
            writer,
            reader,
            &response(
                409,
                r#"{"error":"a server-push SSE stream is already open"}"#,
            ),
        );
        return;
    }

    if writer.write_all(SSE_STREAM_HEADER.as_bytes()).is_err() || writer.flush().is_err() {
        ctx.get_open.store(false, Ordering::SeqCst);
        return;
    }

    let mut emitted = vec![false; events.len()];
    loop {
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        let served = ctx.log.lock().unwrap_or_else(|e| e.into_inner()).served;
        let mut hung_up = false;
        for (i, event) in events.iter().enumerate() {
            if emitted[i] || event.after > served {
                continue;
            }
            let frame = sse_frame(event);
            if writer.write_all(frame.as_bytes()).is_err() || writer.flush().is_err() {
                hung_up = true;
                break;
            }
            emitted[i] = true;
        }
        if hung_up {
            break;
        }
        // Poll the served counter with the same sleep idiom the accept loop
        // uses; the events are few and the run is short.
        std::thread::sleep(Duration::from_millis(5));
    }
    ctx.get_open.store(false, Ordering::SeqCst);
}

/// RECORD GET: open a matching upstream GET on this thread and pump its SSE
/// frames to the client VERBATIM, capturing each notification as an event and
/// naming a server-initiated request as the v3.4 record failure. Response
/// frames pass through (the POST side owns responses). If the agent never
/// opens a GET, flowproof never opens one upstream.
fn serve_get_record(upstream: &str, request: &Request, ctx: &Ctx, writer: &mut TcpStream) {
    // Answer the client with our own SSE headers up front, then stream.
    if writer.write_all(SSE_STREAM_HEADER.as_bytes()).is_err() || writer.flush().is_err() {
        return;
    }

    let config = ureq::config::Config::builder()
        .http_status_as_error(false)
        .timeout_global(Some(FORWARD_TIMEOUT))
        .build();
    let agent = config.new_agent();
    let mut req = agent.get(upstream).header("accept", "text/event-stream");
    if let Some(auth) = request.authorization.as_deref() {
        req = req.header("authorization", auth);
    }
    if let Some(session) = request.session_id.as_deref() {
        req = req.header("mcp-session-id", session);
    }
    // A server that offers no GET stream (or is unreachable) simply yields no
    // notifications: leave the client stream open and empty.
    let mut response = match req.call() {
        Ok(response) => response,
        Err(_) => return,
    };

    let mut upstream_reader = BufReader::new(response.body_mut().as_reader());
    let mut data = String::new();
    let mut line = String::new();
    loop {
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        line.clear();
        match upstream_reader.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        // Pump verbatim to the client; a write failure means the agent closed
        // its stream, so stop.
        if writer.write_all(line.as_bytes()).is_err() || writer.flush().is_err() {
            break;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            if !data.is_empty() {
                // No request id on the GET stream: a response frame here is
                // not "ours" to normalize (Skip), a notification is captured,
                // a server-initiated request is the named v3.4 failure.
                match classify_sse_frame(&data, None) {
                    SseFrame::Notification { method, params } => {
                        let after = ctx.captured.lock().unwrap_or_else(|e| e.into_inner()).len();
                        ctx.rec_events
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(McpServerEvent::notification(after, method, params));
                    }
                    SseFrame::ServerRequest(named) => {
                        let mut log = ctx.log.lock().unwrap_or_else(|e| e.into_inner());
                        if log.record_error.is_none() {
                            log.record_error = Some(named);
                        }
                        break;
                    }
                    SseFrame::Response(_) | SseFrame::Skip => {}
                }
                data.clear();
            }
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("data:") {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
    }
}

/// One `event: message` SSE frame carrying a recorded notification's
/// JSON-RPC envelope.
fn sse_frame(event: &McpServerEvent) -> String {
    let envelope = notification_envelope(&event.method, &event.params);
    format!("event: message\ndata: {envelope}\n\n")
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
    /// elicitation) mid-response: the one thing v3.3 cannot record (v3.4).
    ServerRequest(String),
    /// A transport or parse failure reaching the real server.
    Failed(String),
}

/// Forward the agent's exact POST body to the real server, passing through
/// its `Authorization` and `Mcp-Session-Id` headers (headers are never
/// stored). The real server may answer `application/json` (one message) or
/// `text/event-stream` (SSE `data:` frames); either way the message whose
/// id matches the request is returned as a single JSON value, and any
/// notification frame that precedes it inline is captured into `rec_events`
/// anchored at `anchor`.
fn forward(
    upstream: &str,
    auth: Option<&str>,
    session: Option<&str>,
    body: &[u8],
    rec_events: &Mutex<Vec<McpServerEvent>>,
    anchor: usize,
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
        read_sse_message(
            &mut reader,
            request_id.as_ref(),
            session_id,
            rec_events,
            anchor,
        )
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
/// NOT wait for the stream to close. A notification (no id) that precedes the
/// response inline is CAPTURED into `rec_events` (anchored at `anchor`) and
/// then dropped from the reply; a server-initiated request (a message
/// carrying both `method` and `id`) is the named v3.4 record failure.
fn read_sse_message(
    reader: &mut impl BufRead,
    request_id: Option<&Value>,
    session_id: Option<String>,
    rec_events: &Mutex<Vec<McpServerEvent>>,
    anchor: usize,
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
                    SseFrame::Notification { method, params } => {
                        capture_notification(rec_events, anchor, method, params)
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
            SseFrame::Notification { method, params } => {
                capture_notification(rec_events, anchor, method, params)
            }
            SseFrame::ServerRequest(named) => return ForwardResult::ServerRequest(named),
            SseFrame::Skip => {}
        }
    }
    ForwardResult::Failed(
        "the real MCP server's SSE stream ended before answering the request".to_string(),
    )
}

/// Append a captured notification to the record-time events lane.
fn capture_notification(
    rec_events: &Mutex<Vec<McpServerEvent>>,
    anchor: usize,
    method: String,
    params: Value,
) {
    rec_events
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(McpServerEvent::notification(anchor, method, params));
}

/// One classified SSE frame.
enum SseFrame {
    Response(Value),
    /// A server notification (a method, no id): captured as an event in v3.3.
    Notification {
        method: String,
        params: Value,
    },
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
        // id to answer. Recording it is v3.4.
        (Some(method), Some(_)) => SseFrame::ServerRequest(server_request_named(method)),
        // A server notification: a method, no id. Captured as an event.
        (Some(method), None) => SseFrame::Notification {
            method: method.to_string(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        },
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
        409 => "Conflict",
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
        let server = McpHttpServer::replay(lane, Vec::new(), BTreeMap::new(), 0).expect("starts");
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
        let server = McpHttpServer::replay(lane, Vec::new(), BTreeMap::new(), 0).expect("starts");
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

    /// A client notification is 202, and session teardown is answered
    /// locally. (v3.3 makes the standalone GET an SSE stream, covered below,
    /// so it is no longer a 405.)
    #[test]
    fn a_notification_gets_202_and_delete_gets_200() {
        let server =
            McpHttpServer::replay(Vec::new(), Vec::new(), BTreeMap::new(), 0).expect("starts");
        let (status, _, _) = post(
            &server.url(),
            serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        );
        assert_eq!(status, 202, "notification");

        let (status, _, _) = post_raw(&server.url(), "DELETE", "");
        assert_eq!(status, 200, "session teardown answered locally");
    }

    /// A JSON-RPC batch (top-level array) is a named 400, never silently
    /// half-handled.
    #[test]
    fn a_batch_post_is_a_named_400() {
        let server =
            McpHttpServer::replay(Vec::new(), Vec::new(), BTreeMap::new(), 0).expect("starts");
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
        let server =
            McpHttpServer::replay(Vec::new(), Vec::new(), BTreeMap::new(), 0).expect("starts");
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

    /// Open a standalone GET SSE stream: send the GET, read the status line
    /// and drain the response head, and return the status plus a reader
    /// positioned at the body. A read timeout keeps a held-open empty stream
    /// from blocking the test.
    fn open_get(url: &str) -> (u16, BufReader<TcpStream>) {
        let rest = url.trim_start_matches("http://");
        let (addr, path) = rest
            .split_once('/')
            .map(|(a, p)| (a.to_string(), format!("/{p}")))
            .expect("url has a path");
        let stream = TcpStream::connect(&addr).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_millis(800)))
            .ok();
        let mut writer = stream.try_clone().expect("clone");
        let request =
            format!("GET {path} HTTP/1.1\r\nhost: {addr}\r\naccept: text/event-stream\r\n\r\n");
        writer.write_all(request.as_bytes()).expect("write");
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).ok();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        // Drain the response headers up to the blank line.
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            if header == "\r\n" || header == "\n" {
                break;
            }
        }
        (status, reader)
    }

    /// Read up to `want` SSE `data:` notification frames off a stream, giving
    /// up when the read times out.
    fn read_notifications(reader: &mut BufReader<TcpStream>, want: usize) -> Vec<Value> {
        let mut out = Vec::new();
        let mut data = String::new();
        let mut line = String::new();
        while out.len() < want {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let trimmed = line.trim_end_matches(['\r', '\n']);
            if trimmed.is_empty() {
                if !data.is_empty() {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        out.push(v);
                    }
                    data.clear();
                }
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("data:") {
                let rest = rest.strip_prefix(' ').unwrap_or(rest);
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(rest);
            }
        }
        out
    }

    /// REPLAY: recorded notifications are re-emitted over the standalone GET
    /// stream at their anchors - anchor-0 the instant the stream opens, a
    /// later one only after the calls it followed are answered - and a second
    /// concurrent GET is a 409.
    #[test]
    fn replay_get_stream_emits_notifications_at_their_anchors_and_409s_a_second() {
        let calls = vec![
            call(
                "initialize",
                serde_json::json!({ "protocolVersion": "2024-11-05" }),
                serde_json::json!({ "protocolVersion": "2024-11-05" }),
            ),
            call(
                "tools/call",
                serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } }),
                serde_json::json!({ "content": [] }),
            ),
        ];
        let events = vec![
            McpServerEvent::notification(
                0,
                "notifications/message".into(),
                serde_json::json!({ "level": "info", "data": "starting" }),
            ),
            McpServerEvent::notification(
                2,
                "notifications/tools/list_changed".into(),
                serde_json::json!({}),
            ),
        ];
        let server = McpHttpServer::replay(calls, events, BTreeMap::new(), 0).expect("starts");
        let url = server.url();

        let (status, mut reader) = open_get(&url);
        assert_eq!(status, 200, "the GET stream opens");
        // A second concurrent GET is rejected while the first is open.
        let (status2, _reader2) = open_get(&url);
        assert_eq!(status2, 409, "a second GET stream is a 409");

        // The anchor-0 event is due the instant the stream exists.
        let first = read_notifications(&mut reader, 1);
        assert_eq!(first.len(), 1, "anchor-0 delivered");
        assert_eq!(first[0]["method"], "notifications/message");

        // The after=2 event is not due until both calls are answered.
        post(
            &url,
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": { "protocolVersion": "2024-11-05" } }),
        );
        post(
            &url,
            serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": { "name": "get_weather", "arguments": { "city": "Paris" } } }),
        );
        let second = read_notifications(&mut reader, 1);
        assert_eq!(second.len(), 1, "the after=2 event delivered once due");
        assert_eq!(second[0]["method"], "notifications/tools/list_changed");
        assert_eq!(server.log().served, 2);
    }

    /// Read one HTTP request off a fake server socket:
    /// `(http_method, jsonrpc_id, jsonrpc_method, params)`.
    fn read_server_request(stream: &mut TcpStream) -> (String, Value, String, Value) {
        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
        let mut request_line = String::new();
        reader.read_line(&mut request_line).ok();
        let http_method = request_line
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_string();
        let mut length = 0usize;
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).unwrap_or(0) == 0 {
                break;
            }
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            if let Some((n, v)) = header.split_once(':') {
                if n.eq_ignore_ascii_case("content-length") {
                    length = v.trim().parse().unwrap_or(0);
                }
            }
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).ok();
        let json: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        (
            http_method,
            json.get("id").cloned().unwrap_or(Value::Null),
            json.get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            json.get("params").cloned().unwrap_or(Value::Null),
        )
    }

    /// A fake REAL server that sends server-initiated notifications. A
    /// `tools/call` POST is answered over SSE with a notification frame BEFORE
    /// the response frame (the inline case); a standalone GET is answered with
    /// an SSE notification frame then closed (the bridge case). Serves exactly
    /// `count` connections, then exits so the test can join it.
    fn spawn_notifying_server(count: usize) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let url = format!("http://{addr}/mcp");
        let handle = std::thread::spawn(move || {
            for _ in 0..count {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let (http_method, id, method, _params) = read_server_request(&mut stream);
                if http_method == "GET" {
                    let notif = serde_json::json!({ "jsonrpc": "2.0",
                        "method": "notifications/resources/updated",
                        "params": { "uri": "file:///x" } });
                    let frame = format!("event: message\ndata: {notif}\n\n");
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                         connection: close\r\n\r\n{frame}"
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                } else {
                    let response_msg = serde_json::json!({ "jsonrpc": "2.0", "id": id,
                        "result": { "content": [{ "type": "text", "text": "ok" }] } });
                    let body = if method == "tools/call" {
                        let notif = serde_json::json!({ "jsonrpc": "2.0",
                            "method": "notifications/message",
                            "params": { "level": "info" } });
                        format!(
                            "event: message\ndata: {notif}\n\nevent: message\ndata: {response_msg}\n\n"
                        )
                    } else {
                        format!("event: message\ndata: {response_msg}\n\n")
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(resp.as_bytes());
                    let _ = stream.flush();
                }
            }
        });
        (url, handle)
    }

    /// Poll the record-time events lane until it holds at least `want`, so a
    /// test does not race the pump thread that captures them.
    fn wait_for_events(server: &McpHttpServer, want: usize) -> Vec<McpServerEvent> {
        for _ in 0..200 {
            let events = server.events();
            if events.len() >= want {
                return events;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        server.events()
    }

    /// RECORD: a notification inline in a POST's SSE body is captured as an
    /// event (anchored before the call it preceded) and stripped from the
    /// single JSON reply the agent gets.
    #[test]
    fn record_captures_a_notification_inline_in_a_post_sse_body() {
        let (real_url, handle) = spawn_notifying_server(1);
        let server = McpHttpServer::record(&real_url, BTreeMap::new(), 0).expect("starts");
        let (status, body, _) = post(
            &server.url(),
            serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": { "name": "get_weather", "arguments": { "city": "Paris" } } }),
        );
        assert_eq!(status, 200);
        assert_eq!(
            body["result"]["content"][0]["text"], "ok",
            "the response frame is returned, the notification stripped from the reply"
        );
        let events = server.events();
        assert_eq!(events.len(), 1, "the inline notification was captured");
        assert_eq!(events[0].method, "notifications/message");
        assert_eq!(events[0].after, 0, "anchored before this first call");
        drop(server);
        handle.join().ok();
    }

    /// RECORD: when the agent opens the standalone GET stream, flowproof
    /// bridges to a matching upstream GET, pumps each notification frame to
    /// the agent verbatim, and captures it as an event.
    #[test]
    fn record_captures_a_notification_from_the_standalone_get_bridge() {
        let (real_url, handle) = spawn_notifying_server(1);
        let server = McpHttpServer::record(&real_url, BTreeMap::new(), 0).expect("starts");
        let (status, mut reader) = open_get(&server.url());
        assert_eq!(status, 200, "the bridged GET stream opens");
        let notifs = read_notifications(&mut reader, 1);
        assert_eq!(
            notifs.len(),
            1,
            "the upstream notification reached the agent"
        );
        assert_eq!(notifs[0]["method"], "notifications/resources/updated");
        let events = wait_for_events(&server, 1);
        assert_eq!(events.len(), 1, "captured as an event");
        assert_eq!(events[0].method, "notifications/resources/updated");
        drop(server);
        handle.join().ok();
    }

    /// RECORD: a server-initiated REQUEST (method AND id) on the standalone
    /// GET bridge is the named v3.4 record failure, not a silent capture.
    #[test]
    fn record_names_a_server_request_on_the_get_bridge() {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let addr = listener.local_addr().expect("addr");
        let real_url = format!("http://{addr}/mcp");
        let handle = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = read_server_request(&mut stream);
                // A server-initiated request: a method to invoke AND an id.
                let req = serde_json::json!({ "jsonrpc": "2.0", "id": 99,
                    "method": "sampling/createMessage", "params": {} });
                let frame = format!("event: message\ndata: {req}\n\n");
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                     connection: close\r\n\r\n{frame}"
                );
                let _ = stream.write_all(resp.as_bytes());
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        let server = McpHttpServer::record(&real_url, BTreeMap::new(), 0).expect("starts");
        let (status, mut reader) = open_get(&server.url());
        assert_eq!(status, 200);
        let _ = read_notifications(&mut reader, 1);
        // The bridge names the failure rather than capturing it.
        for _ in 0..200 {
            if server.log().record_error.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let err = server.log().record_error.clone().expect("a named failure");
        assert!(err.contains("server-initiated request"), "{err}");
        assert!(err.contains("sampling/createMessage"), "{err}");
        assert!(err.contains("v3.4"), "{err}");
        assert!(server.events().is_empty(), "not captured as an event");
        drop(server);
        handle.join().ok();
    }
}
