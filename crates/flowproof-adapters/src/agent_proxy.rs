//! The model-boundary proxy: an OpenAI-compatible chat-completions
//! endpoint that answers from a recorded cassette.
//!
//! This is what makes an agent test deterministic and free. The system
//! under test is launched with its API base URL pointed here, so it keeps
//! making the same HTTP calls it always makes, to what it believes is the
//! model. Nothing about the agent changes; the nondeterminism is removed
//! from underneath it.
//!
//! Hand-rolled HTTP/1.1 on a plain [`TcpListener`], deliberately. Serving
//! a recording needs no upstream call, so it needs no TLS, no HTTP client
//! and no async runtime, and this workspace has none of those - adding an
//! async stack to answer localhost POSTs from one process would be a large
//! dependency for a small job. Record mode, which DOES have to reach a
//! real API over TLS, is a separate slice and can take that decision with
//! its own evidence.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use flowproof_trace::cassette::{
    Cassette, Divergence, Message, ToolCall, Turn, TurnRequest, TurnResponse,
};
use flowproof_trace::substitution::{self, Mocks};

/// The largest request body the proxy will read. Prompts are large and
/// grow every turn; this is a guard against a malformed `content-length`,
/// not a limit anyone should reach.
const MAX_BODY: usize = 32 * 1024 * 1024;

/// What the proxy observed while serving. Read after the run to assert
/// against the trajectory.
#[derive(Debug, Default)]
pub struct ProxyLog {
    /// Requests served, in order.
    pub served: usize,
    /// The first divergence, which is also the last: serving stops being
    /// meaningful once the trajectory has left its recording.
    pub divergence: Option<Divergence>,
    /// The first upstream error, in record mode: a real model call that
    /// failed. A recording is not minted when this is set.
    pub upstream_error: Option<String>,
}

/// A running proxy. Dropping it stops the listener.
pub struct AgentProxy {
    addr: SocketAddr,
    log: Arc<Mutex<ProxyLog>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Turns captured in record mode, in order. Empty in replay mode.
    captured: Arc<Mutex<Vec<Turn>>>,
}

impl AgentProxy {
    /// Start serving `cassette` on an ephemeral localhost port.
    ///
    /// Bound to 127.0.0.1 on purpose: this endpoint answers whatever asks
    /// it, with no authentication, so it must not be reachable off the
    /// machine running the test.
    pub fn start(cassette: Cassette, mocks: Mocks) -> std::io::Result<Self> {
        Self::spawn(Mode::Replay(cassette), mocks)
    }

    /// Start in RECORD mode: forward each request to the real model at
    /// `upstream` (an OpenAI-compatible base URL like
    /// `https://api.openai.com/v1`), substituting mocked tool results
    /// first, and capture the exchange. The captured cassette is read with
    /// [`AgentProxy::captured`] after the run.
    ///
    /// This is the ONE place flowproof reaches a real model, and the only
    /// non-hermetic step in the whole feature: record touches reality by
    /// design, replay does not.
    pub fn record(upstream: &str, auth: Option<String>, mocks: Mocks) -> std::io::Result<Self> {
        Self::spawn(
            Mode::Record {
                upstream: upstream.trim_end_matches('/').to_string(),
                auth,
            },
            mocks,
        )
    }

    fn spawn(mode: Mode, mocks: Mocks) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        // A short read timeout lets the accept loop notice `stop` even
        // when a client connects and then says nothing.
        listener.set_nonblocking(true)?;

        let log = Arc::new(Mutex::new(ProxyLog::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(Mutex::new(Vec::new()));
        let thread = {
            let (log, stop, captured) =
                (Arc::clone(&log), Arc::clone(&stop), Arc::clone(&captured));
            std::thread::spawn(move || {
                let mut turn = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).ok();
                            serve_one(stream, &mode, &mocks, &mut turn, &log, &captured);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
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

    /// The cassette captured in record mode, in order. Empty in replay.
    pub fn captured(&self) -> Cassette {
        Cassette {
            turns: self
                .captured
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// The base URL to hand the system under test, in the shape
    /// `OPENAI_BASE_URL` expects.
    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.addr)
    }

    pub fn log(&self) -> std::sync::MutexGuard<'_, ProxyLog> {
        self.log.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for AgentProxy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Replay serves a cassette; record forwards to a real model and captures.
enum Mode {
    Replay(Cassette),
    Record {
        upstream: String,
        /// The value for the outbound `Authorization` header, when the real
        /// model needs one. Comes from flowproof's own environment straight
        /// into this header - it is never read into the trace, which stores
        /// request BODIES only, so a recorded cassette carries no secret.
        auth: Option<String>,
    },
}

/// Read one request, answer it, close. `Connection: close` every time:
/// keep-alive would buy nothing here and multiplexing state machines are
/// where hand-rolled HTTP goes wrong.
fn serve_one(
    stream: TcpStream,
    mode: &Mode,
    mocks: &Mocks,
    turn: &mut usize,
    log: &Mutex<ProxyLog>,
    captured: &Mutex<Vec<Turn>>,
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

    // Dispatch by path: the OpenAI chat-completions endpoint and the
    // Anthropic Messages endpoint are served side by side, each parsed and
    // rendered in its own dialect but reduced to the same neutral turn. The
    // substring style is kept so a base URL with or without a trailing
    // segment still routes.
    let protocol = if request.path.contains("/v1/messages") {
        "anthropic"
    } else if request.path.contains("/chat/completions") {
        "openai"
    } else {
        respond(
            &mut writer,
            &mut reader,
            &response(
                404,
                r#"{"error":"only /v1/chat/completions and /v1/messages are served"}"#,
            ),
        );
        return;
    };
    let is_anthropic = protocol == "anthropic";

    // Substitute on the raw body FIRST - the same transform in both phases,
    // so the request the model sees (record) and the request matched
    // (replay) are the same one, and a volatile real tool result cannot
    // fail replay. Each dialect has its own sibling transform.
    let mut json: serde_json::Value = match serde_json::from_slice(&request.body) {
        Ok(json) => json,
        Err(e) => {
            respond(
                &mut writer,
                &mut reader,
                &response(400, &error_body(&format!("request is not JSON: {e}"))),
            );
            return;
        }
    };
    if is_anthropic {
        substitution::apply_anthropic_json(&mut json, mocks);
    } else {
        substitution::apply_json(&mut json, mocks);
    }

    // `stream` is transport, not conversation. It never changes which turn
    // this is (the request parsers ignore it, alongside the sampling knobs),
    // so a cassette recorded from a non-streaming client serves a streaming
    // one and vice versa. Read the client's intent, then strip it (and
    // stream_options) so that in record mode the upstream answers with one
    // non-streaming body to read, and the request captured/compared is
    // stream-free.
    let wants_stream = json
        .get("stream")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    if let Some(obj) = json.as_object_mut() {
        obj.remove("stream");
        obj.remove("stream_options");
    }

    let parsed = if is_anthropic {
        request_from_anthropic_json(&json)
    } else {
        request_from_json(&json)
    };
    let incoming = match parsed {
        Ok(request) => request,
        Err(why) => {
            respond(
                &mut writer,
                &mut reader,
                &response(
                    400,
                    &error_body(&format!("could not read the request: {why}")),
                ),
            );
            return;
        }
    };

    let index = *turn;
    *turn += 1;

    match mode {
        Mode::Replay(cassette) => match cassette.turn(index, &incoming, protocol) {
            Ok(recorded) => {
                log.lock().unwrap_or_else(|e| e.into_inner()).served += 1;
                // Render the recorded assistant message in the dialect the
                // agent asked in. OpenAI honors `stream: true` with a
                // synthetic SSE stream. Each dialect streams when the client
                // asked (`stream: true`) and serves one JSON body otherwise.
                let stop = recorded.stop_reason.as_deref();
                let bytes = match (is_anthropic, wants_stream) {
                    (true, true) => stream_response(&messages_stream_body(&recorded.message, stop)),
                    (true, false) => response(200, &messages_body(&recorded.message, stop)),
                    (false, true) => stream_response(&completion_stream_body(&recorded.message)),
                    (false, false) => response(200, &completion_body(&recorded.message)),
                };
                respond(&mut writer, &mut reader, &bytes);
            }
            Err(divergence) => {
                // The agent is owed an answer or it will hang; the run is
                // owed the truth. A 409 with the divergence in the body
                // does both, and the recorded reason is what the test
                // reports - an agent that swallows the error must not turn
                // a divergence into a pass.
                let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                if log.divergence.is_none() {
                    log.divergence = Some(divergence.clone());
                }
                drop(log);
                respond(
                    &mut writer,
                    &mut reader,
                    &response(409, &error_body(&divergence.to_string())),
                );
            }
        },
        Mode::Record { upstream, auth } => {
            let outcome = if is_anthropic {
                forward_anthropic(
                    upstream,
                    auth.as_deref(),
                    &json,
                    request.anthropic_version.as_deref(),
                )
            } else {
                forward(upstream, auth.as_deref(), &json).map(|(message, raw)| (message, None, raw))
            };
            match outcome {
                Ok((message, stop_reason, raw)) => {
                    // Capture the POST-substitution request and the model's
                    // reply, stamped with the protocol it spoke: that triple
                    // is exactly what replay will match and serve.
                    captured
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(Turn {
                            protocol: protocol.to_string(),
                            request: incoming,
                            response: TurnResponse {
                                message: message.clone(),
                                stop_reason: stop_reason.clone(),
                            },
                        });
                    log.lock().unwrap_or_else(|e| e.into_inner()).served += 1;
                    // Hand the model's OWN response body back to the agent
                    // verbatim, so record is transparent. When an OpenAI
                    // agent asked for a stream, the upstream was forced
                    // non-streaming (stream was stripped), so synthesize the
                    // stream from the captured message: the agent then sees
                    // the SAME transport and chunking at record as at replay,
                    // and no record/replay asymmetry can hide in transport.
                    // Otherwise hand the model's own body back verbatim.
                    let bytes = match (wants_stream, is_anthropic) {
                        (true, true) => {
                            stream_response(&messages_stream_body(&message, stop_reason.as_deref()))
                        }
                        (true, false) => stream_response(&completion_stream_body(&message)),
                        (false, _) => response(200, &raw),
                    };
                    respond(&mut writer, &mut reader, &bytes);
                }
                Err(why) => {
                    let mut log = log.lock().unwrap_or_else(|e| e.into_inner());
                    if log.upstream_error.is_none() {
                        log.upstream_error = Some(why.clone());
                    }
                    drop(log);
                    respond(
                        &mut writer,
                        &mut reader,
                        &response(
                            502,
                            &error_body(&format!("upstream model call failed: {why}")),
                        ),
                    );
                }
            }
        }
    }
}

/// Forward a request to the upstream model and read its reply. Returns the
/// parsed assistant message (for the cassette) and the raw response body
/// (handed back to the agent unchanged).
fn forward(
    upstream: &str,
    auth: Option<&str>,
    body: &serde_json::Value,
) -> Result<(Message, String), String> {
    let url = format!("{upstream}/chat/completions");
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut request = ureq::post(&url).header("content-type", "application/json");
    if let Some(auth) = auth {
        request = request.header("authorization", auth);
    }
    let mut response = request.send(&bytes[..]).map_err(|e| e.to_string())?;
    let raw = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("upstream returned non-JSON: {e}"))?;
    let message = parsed
        .get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .ok_or("upstream response has no choices[0].message")?;
    Ok((parse_message(message), raw))
}

/// Forward a request to the upstream Anthropic Messages API and read its
/// reply. Returns the parsed assistant message, the recorded `stop_reason`,
/// and the raw response body (handed back to the agent unchanged).
///
/// The auth story differs from OpenAI on purpose: Anthropic authenticates
/// with an `x-api-key` header carrying the BARE key, not `Authorization:
/// Bearer`. A `Bearer ` prefix is stripped defensively so a key threaded
/// through the shared record plumbing still lands in the right shape. The
/// incoming `anthropic-version` is passed through, defaulting to the pinned
/// `2023-06-01` the SDKs send.
fn forward_anthropic(
    upstream: &str,
    auth: Option<&str>,
    body: &serde_json::Value,
    version: Option<&str>,
) -> Result<(Message, Option<String>, String), String> {
    let url = format!("{upstream}/v1/messages");
    let bytes = serde_json::to_vec(body).map_err(|e| e.to_string())?;
    let mut request = ureq::post(&url)
        .header("content-type", "application/json")
        .header("anthropic-version", version.unwrap_or("2023-06-01"));
    if let Some(auth) = auth {
        let key = auth.strip_prefix("Bearer ").unwrap_or(auth);
        request = request.header("x-api-key", key);
    }
    let mut response = request.send(&bytes[..]).map_err(|e| e.to_string())?;
    let raw = response
        .body_mut()
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("upstream returned non-JSON: {e}"))?;
    let message = message_from_anthropic_response(&parsed)?;
    let stop_reason = parsed
        .get("stop_reason")
        .and_then(|s| s.as_str())
        .map(str::to_string);
    Ok((message, stop_reason, raw))
}

/// Parse an Anthropic Messages RESPONSE into the neutral assistant message:
/// text blocks concatenate into `content`, `tool_use` blocks become
/// `ToolCall`s whose arguments are the canonical JSON of the block `input`.
/// The same normalization a chat-completions reply gets from
/// [`parse_message`], so a recorded turn is dialect-independent thereafter.
fn message_from_anthropic_response(parsed: &serde_json::Value) -> Result<Message, String> {
    let content = parsed
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or("anthropic response has no content array")?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    for block in content {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    arguments: input.to_string(),
                });
            }
            _ => {}
        }
    }
    Ok(Message {
        role: "assistant".into(),
        // An assistant turn that only called tools said nothing; None keeps
        // it byte-identical to the OpenAI shape through the round trip.
        content: if text.is_empty() { None } else { Some(text) },
        tool_calls,
        tool_call_id: None,
    })
}

/// Send a response and close the write half cleanly.
///
/// Dropping the socket straight after `write_all` is what a first draft
/// does, and on Windows it intermittently costs the client its answer:
/// closing a socket that still has anything pending makes the stack send
/// RST rather than FIN, and the client's read fails with
/// WSAECONNRESET (10054) instead of returning the response it was
/// already sent. It surfaced as a flaky windows-latest job, but the same
/// race would hand a real agent a connection error in place of its model
/// reply, which the agent would report as a model outage.
///
/// So: flush, then shut down the write half so the peer sees an orderly
/// FIN, then drain whatever the client still had in flight before the
/// socket goes away.
fn respond(writer: &mut TcpStream, reader: &mut BufReader<TcpStream>, bytes: &[u8]) {
    let _ = writer.write_all(bytes);
    let _ = writer.flush();
    let _ = writer.shutdown(std::net::Shutdown::Write);
    // Unread bytes in the receive buffer are the other way to earn an RST.
    // Bounded so a client that keeps talking cannot hold the thread.
    let mut sink = [0u8; 4096];
    for _ in 0..16 {
        match reader.read(&mut sink) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// The parts of a request the proxy acts on: the path (which API), the
/// `anthropic-version` header (passed through to the real Messages API when
/// recording), and the body.
struct Request {
    path: String,
    anthropic_version: Option<String>,
    body: Vec<u8>,
}

/// Read the request line, headers, and exactly `content-length` bytes.
///
/// Reading a fixed-size buffer once would be shorter and wrong: a
/// trajectory's later prompts run to tens of kilobytes and arrive across
/// several TCP segments, so the body has to be read to its declared
/// length rather than to whatever happened to have landed.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<Request> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1)?.to_string();

    let mut length = 0usize;
    let mut anthropic_version = None;
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
            } else if name.eq_ignore_ascii_case("anthropic-version") {
                anthropic_version = Some(value.trim().to_string());
            }
        }
    }
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        path,
        anthropic_version,
        body,
    })
}

/// Pull the comparable request out of an OpenAI-compatible payload.
///
/// Only the fields the cassette matches on are taken. Sampling knobs
/// (temperature, top_p, seed) are deliberately ignored: they do not change
/// which conversation this is, and matching on them would make a test fail
/// because someone tuned a dial.
fn request_from_json(json: &serde_json::Value) -> Result<TurnRequest, String> {
    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or("no model")?
        .to_string();
    let messages = json
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or("no messages")?
        .iter()
        .map(parse_message)
        .collect();
    let tools = json
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    t.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(str::to_string)
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(TurnRequest {
        model,
        messages,
        tools,
    })
}

/// Pull the comparable request out of an Anthropic Messages payload,
/// producing the SAME neutral [`TurnRequest`] the OpenAI parser does.
///
/// The dialects disagree on shape but agree on meaning, and the neutral
/// form is where they meet: a top-level `system` becomes a leading system
/// message, each content array is normalized block by block, and tools are
/// named by their top-level `name` (Anthropic has no `function` wrapper).
/// The normalization is one fixed helper used identically at record and
/// replay, so a request recorded one run matches the same request the next.
fn request_from_anthropic_json(json: &serde_json::Value) -> Result<TurnRequest, String> {
    let model = json
        .get("model")
        .and_then(|m| m.as_str())
        .ok_or("no model")?
        .to_string();

    let mut messages = Vec::new();
    // A system prompt is top-level in Anthropic, not a message; it becomes a
    // leading system message so the neutral roles line up with OpenAI.
    if let Some(system) = json.get("system") {
        if let Some(text) = anthropic_text(system) {
            messages.push(Message {
                role: "system".into(),
                content: Some(text),
                tool_calls: Vec::new(),
                tool_call_id: None,
            });
        }
    }
    let raw = json
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or("no messages")?;
    for message in raw {
        let role = message
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or_default();
        let content = message.get("content").unwrap_or(&serde_json::Value::Null);
        messages.extend(anthropic_messages_from_content(role, content));
    }

    let tools = json
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Ok(TurnRequest {
        model,
        messages,
        tools,
    })
}

/// The text of an Anthropic `system` or block content: a bare string, or an
/// array of `{type:"text", text}` blocks joined in order. Anything else has
/// no text to contribute.
fn anthropic_text(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_string());
    }
    if let Some(blocks) = value.as_array() {
        let joined: String = blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
        return Some(joined);
    }
    None
}

/// The neutral text of a `tool_result` block's content: a bare string kept
/// verbatim, or an array of text blocks joined. A substituted mock is a bare
/// JSON string, so this reads it back byte-identical - the property replay
/// matching depends on.
fn anthropic_tool_result_content(block: &serde_json::Value) -> String {
    let Some(content) = block.get("content") else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(blocks) = content.as_array() {
        return blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
            .collect();
    }
    // A structured object result with no text blocks: fall back to canonical
    // JSON so it is at least deterministic.
    content.to_string()
}

/// Normalize one Anthropic message's content into neutral messages.
///
/// A string content is one message. A block array is split in block order:
/// `text` blocks concatenate into this message's content, `tool_use` blocks
/// become tool calls on it, and each `tool_result` block becomes a SEPARATE
/// neutral `role:"tool"` message (the OpenAI shape). The tool messages come
/// first, then the text/tool_use message if it carried anything - so a user
/// turn mixing a tool result and a follow-up sentence splits into (tool) +
/// (user text), deterministically, the same way in both phases.
fn anthropic_messages_from_content(role: &str, content: &serde_json::Value) -> Vec<Message> {
    if let Some(text) = content.as_str() {
        return vec![Message {
            role: role.to_string(),
            content: Some(text.to_string()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }];
    }
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };

    let mut text = String::new();
    let mut tool_calls = Vec::new();
    let mut tool_messages = Vec::new();
    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    text.push_str(t);
                }
            }
            Some("tool_use") => {
                let input = block.get("input").cloned().unwrap_or(serde_json::json!({}));
                tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(|i| i.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    arguments: input.to_string(),
                });
            }
            Some("tool_result") => {
                tool_messages.push(Message {
                    role: "tool".into(),
                    content: Some(anthropic_tool_result_content(block)),
                    tool_calls: Vec::new(),
                    tool_call_id: block
                        .get("tool_use_id")
                        .and_then(|i| i.as_str())
                        .map(str::to_string),
                });
            }
            _ => {}
        }
    }

    let mut out = tool_messages;
    // The carrying message follows the tool results, and only when it
    // actually said or asked something - a user turn that was nothing but a
    // tool result adds no empty message.
    if !text.is_empty() || !tool_calls.is_empty() {
        out.push(Message {
            role: role.to_string(),
            content: if text.is_empty() { None } else { Some(text) },
            tool_calls,
            tool_call_id: None,
        });
    }
    out
}

fn parse_message(value: &serde_json::Value) -> Message {
    Message {
        role: value
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or_default()
            .to_string(),
        // Content is absent on a pure tool-call message and null on some
        // clients; both mean "nothing said".
        content: value
            .get("content")
            .and_then(|c| c.as_str())
            .map(str::to_string),
        tool_calls: value
            .get("tool_calls")
            .and_then(|c| c.as_array())
            .map(|calls| calls.iter().filter_map(parse_tool_call).collect())
            .unwrap_or_default(),
        tool_call_id: value
            .get("tool_call_id")
            .and_then(|c| c.as_str())
            .map(str::to_string),
    }
}

fn parse_tool_call(value: &serde_json::Value) -> Option<ToolCall> {
    let function = value.get("function")?;
    Some(ToolCall {
        id: value
            .get("id")
            .and_then(|i| i.as_str())
            .unwrap_or_default()
            .to_string(),
        name: function.get("name")?.as_str()?.to_string(),
        arguments: function
            .get("arguments")
            .and_then(|a| a.as_str())
            .unwrap_or("{}")
            .to_string(),
    })
}

/// Render a recorded assistant message as a chat-completions response.
fn completion_body(message: &Message) -> String {
    let tool_calls: Vec<serde_json::Value> = message
        .tool_calls
        .iter()
        .map(|call| {
            serde_json::json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            })
        })
        .collect();
    let mut rendered = serde_json::json!({
        "role": message.role,
        "content": message.content,
    });
    if !tool_calls.is_empty() {
        rendered["tool_calls"] = serde_json::Value::Array(tool_calls);
    }
    serde_json::json!({
        "id": "flowproof-replay",
        "object": "chat.completion",
        "model": "flowproof-replay",
        "choices": [{
            "index": 0,
            "message": rendered,
            // A recorded turn that asked for tools finished for that
            // reason; anything else finished by stopping.
            "finish_reason": if message.tool_calls.is_empty() { "stop" } else { "tool_calls" },
        }],
    })
    .to_string()
}

/// Render a recorded assistant message as a synthetic OpenAI
/// chat-completions SSE stream: a role chunk, one delta carrying the whole
/// content, one delta per tool call carrying the whole arguments, a finish
/// chunk, and the terminating `[DONE]`. Deliberately minimal and
/// un-lifelike - chunk boundaries are noise flowproof does not record, so it
/// does not reproduce them. The whole stream is precomputed into one body: a
/// client reading it incrementally cannot tell the frames all arrived at
/// once, and no chunked transfer encoding is needed.
fn completion_stream_body(message: &Message) -> String {
    let finish = if message.tool_calls.is_empty() {
        "stop"
    } else {
        "tool_calls"
    };
    let frame = |delta: serde_json::Value, finish_reason: serde_json::Value| -> String {
        let chunk = serde_json::json!({
            "id": "flowproof-replay",
            "object": "chat.completion.chunk",
            "model": "flowproof-replay",
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
        });
        format!("data: {chunk}\n\n")
    };
    let null = serde_json::Value::Null;
    let mut out = String::new();
    // 1. The role, on its own, as a streaming client expects first.
    out.push_str(&frame(
        serde_json::json!({ "role": message.role }),
        null.clone(),
    ));
    // 2. The whole content as a single delta, if the turn said anything.
    if let Some(content) = &message.content {
        if !content.is_empty() {
            out.push_str(&frame(
                serde_json::json!({ "content": content }),
                null.clone(),
            ));
        }
    }
    // 3. One delta per tool call, whole arguments in one piece.
    for (index, call) in message.tool_calls.iter().enumerate() {
        out.push_str(&frame(
            serde_json::json!({ "tool_calls": [{
                "index": index,
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            }] }),
            null.clone(),
        ));
    }
    // 4. The finish reason, then the terminator every SSE client waits for.
    out.push_str(&frame(serde_json::json!({}), serde_json::json!(finish)));
    out.push_str("data: [DONE]\n\n");
    out
}

/// Render a recorded assistant message as an Anthropic Messages RESPONSE.
///
/// The neutral message becomes content blocks: a text block when it said
/// something, then one `tool_use` block per call with its arguments parsed
/// back to the `input` object. The `stop_reason` is served from the
/// recording when it was captured, and otherwise inferred - `tool_use` if
/// the turn called tools, `end_turn` if it just spoke - so a hand-written
/// cassette still yields a plausible reason.
fn messages_body(message: &Message, stop_reason: Option<&str>) -> String {
    let mut content: Vec<serde_json::Value> = Vec::new();
    if let Some(text) = &message.content {
        if !text.is_empty() {
            content.push(serde_json::json!({ "type": "text", "text": text }));
        }
    }
    for call in &message.tool_calls {
        let input: serde_json::Value =
            serde_json::from_str(&call.arguments).unwrap_or(serde_json::json!({}));
        content.push(serde_json::json!({
            "type": "tool_use",
            "id": call.id,
            "name": call.name,
            "input": input,
        }));
    }
    let stop_reason = stop_reason.map(str::to_string).unwrap_or_else(|| {
        if message.tool_calls.is_empty() {
            "end_turn".into()
        } else {
            "tool_use".into()
        }
    });
    serde_json::json!({
        "id": "flowproof-replay",
        "type": "message",
        "role": "assistant",
        "model": "flowproof-replay",
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": serde_json::Value::Null,
    })
    .to_string()
}

/// Render a recorded assistant message as a synthetic Anthropic Messages SSE
/// stream: a `message_start` envelope, then for each content block a
/// `content_block_start` / one full delta / `content_block_stop`, a
/// `message_delta` carrying the stop reason, and `message_stop`. The same
/// deliberately-minimal, precomputed approach as `completion_stream_body`:
/// chunk boundaries are not a recorded fact, so they are not reproduced, and
/// the whole stream ships as one body with a content-length.
fn messages_stream_body(message: &Message, stop_reason: Option<&str>) -> String {
    let stop_reason = stop_reason.map(str::to_string).unwrap_or_else(|| {
        if message.tool_calls.is_empty() {
            "end_turn".into()
        } else {
            "tool_use".into()
        }
    });
    let frame = |event: &str, data: serde_json::Value| -> String {
        format!("event: {event}\ndata: {data}\n\n")
    };

    let mut out = String::new();
    // The opening envelope: an empty assistant message the deltas fill in.
    out.push_str(&frame(
        "message_start",
        serde_json::json!({
            "type": "message_start",
            "message": {
                "id": "flowproof-replay",
                "type": "message",
                "role": "assistant",
                "model": "flowproof-replay",
                "content": [],
                "stop_reason": serde_json::Value::Null,
                "stop_sequence": serde_json::Value::Null,
                "usage": { "input_tokens": 0, "output_tokens": 0 },
            },
        }),
    ));

    // The content blocks, same order as the non-streaming body: a text block
    // if the turn spoke, then one tool_use block per call. Each block is
    // (content_block, delta) - the delta type differs by block, everything
    // else is uniform, so build them once and stream them enumerated.
    let mut blocks: Vec<(serde_json::Value, serde_json::Value)> = Vec::new();
    if let Some(text) = &message.content {
        if !text.is_empty() {
            blocks.push((
                serde_json::json!({ "type": "text", "text": "" }),
                serde_json::json!({ "type": "text_delta", "text": text }),
            ));
        }
    }
    for call in &message.tool_calls {
        blocks.push((
            serde_json::json!({
                "type": "tool_use", "id": call.id, "name": call.name, "input": {},
            }),
            // The whole arguments string as one input_json_delta the client
            // accumulates - the streaming echo of the non-streaming `input`.
            serde_json::json!({ "type": "input_json_delta", "partial_json": call.arguments }),
        ));
    }
    for (index, (block, delta)) in blocks.iter().enumerate() {
        out.push_str(&frame(
            "content_block_start",
            serde_json::json!({
                "type": "content_block_start", "index": index, "content_block": block,
            }),
        ));
        out.push_str(&frame(
            "content_block_delta",
            serde_json::json!({
                "type": "content_block_delta", "index": index, "delta": delta,
            }),
        ));
        out.push_str(&frame(
            "content_block_stop",
            serde_json::json!({ "type": "content_block_stop", "index": index }),
        ));
    }

    // The stop reason rides the message_delta, then the terminator.
    out.push_str(&frame(
        "message_delta",
        serde_json::json!({
            "type": "message_delta",
            "delta": { "stop_reason": stop_reason, "stop_sequence": serde_json::Value::Null },
            "usage": { "output_tokens": 0 },
        }),
    ));
    out.push_str(&frame(
        "message_stop",
        serde_json::json!({ "type": "message_stop" }),
    ));
    out
}

fn error_body(message: &str) -> String {
    serde_json::json!({ "error": { "type": "flowproof_divergence", "message": message } })
        .to_string()
}

fn response(status: u16, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Conflict",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A 200 whose body is a precomputed SSE stream. Same hand-rolled HTTP as
/// `response` with a `content-length` - the stream is fully built, so it
/// needs no chunked transfer encoding and no incremental writes.
fn stream_response(body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowproof_trace::cassette::{Turn, TurnResponse};

    /// Build an OpenAI turn, the default protocol, so the fixtures stay
    /// terse while `Turn`/`TurnResponse` carry their v2 fields.
    fn oai_turn(request: TurnRequest, message: Message) -> Turn {
        Turn {
            protocol: "openai".into(),
            request,
            response: TurnResponse {
                message,
                stop_reason: None,
            },
        }
    }

    /// Build an Anthropic turn with an optional recorded `stop_reason`.
    fn ant_turn(request: TurnRequest, message: Message, stop_reason: Option<&str>) -> Turn {
        Turn {
            protocol: "anthropic".into(),
            request,
            response: TurnResponse {
                message,
                stop_reason: stop_reason.map(str::to_string),
            },
        }
    }

    fn cassette() -> Cassette {
        Cassette {
            turns: vec![
                oai_turn(
                    TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![Message::new("user", "Book a flight to Nairobi")],
                        tools: vec!["search_flights".into()],
                    },
                    Message {
                        role: "assistant".into(),
                        content: None,
                        tool_calls: vec![ToolCall {
                            id: "call_1".into(),
                            name: "search_flights".into(),
                            arguments: r#"{"destination":"NBO"}"#.into(),
                        }],
                        tool_call_id: None,
                    },
                ),
                oai_turn(
                    TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![
                            Message::new("user", "Book a flight to Nairobi"),
                            Message::new("tool", r#"{"id":"KQ311"}"#),
                        ],
                        tools: vec!["search_flights".into()],
                    },
                    Message::new("assistant", "Booked KQ311."),
                ),
            ],
        }
    }

    /// A minimal client: POST a JSON body, return (status, body).
    fn post(base: &str, payload: serde_json::Value) -> (u16, serde_json::Value) {
        let addr = base
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let body = payload.to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {addr}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
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
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
        (status, serde_json::from_str(body).unwrap_or_default())
    }

    /// POST a JSON body to a given path (so the Messages endpoint can be
    /// exercised as well as chat-completions), return (status, body).
    fn post_to(base: &str, path: &str, payload: serde_json::Value) -> (u16, serde_json::Value) {
        let addr = base
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let body = payload.to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let request = format!(
            "POST {path} HTTP/1.1\r\nhost: {addr}\r\nanthropic-version: 2023-06-01\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
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
        let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default();
        (status, serde_json::from_str(body).unwrap_or_default())
    }

    /// POST an Anthropic Messages body to `/v1/messages`.
    fn post_messages(base: &str, payload: serde_json::Value) -> (u16, serde_json::Value) {
        post_to(base, "/v1/messages", payload)
    }

    fn chat(messages: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "model": "gpt-4o",
            "messages": messages,
            "temperature": 0.7,
            "tools": [{"type": "function", "function": {"name": "search_flights"}}],
        })
    }

    /// A two-turn Anthropic trajectory: the model calls a tool, the tool
    /// result comes back in a user message's content array, the model
    /// replies with text. The neutral turns are exactly what the Messages
    /// parser produces from the bodies `post_messages` sends below.
    fn anthropic_cassette() -> Cassette {
        let tool_use = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: "toolu_1".into(),
                name: "get_weather".into(),
                arguments: r#"{"city":"Paris"}"#.into(),
            }],
            tool_call_id: None,
        };
        Cassette {
            turns: vec![
                ant_turn(
                    TurnRequest {
                        model: "claude-sonnet-4-5".into(),
                        messages: vec![Message::new("user", "What's the weather in Paris?")],
                        tools: vec!["get_weather".into()],
                    },
                    tool_use.clone(),
                    Some("tool_use"),
                ),
                ant_turn(
                    TurnRequest {
                        model: "claude-sonnet-4-5".into(),
                        messages: vec![
                            Message::new("user", "What's the weather in Paris?"),
                            tool_use,
                            Message {
                                role: "tool".into(),
                                content: Some("sunny".into()),
                                tool_calls: Vec::new(),
                                tool_call_id: Some("toolu_1".into()),
                            },
                        ],
                        tools: vec!["get_weather".into()],
                    },
                    Message::new("assistant", "It is sunny in Paris."),
                    Some("end_turn"),
                ),
            ],
        }
    }

    /// The whole point: an agent making its usual HTTP calls gets the
    /// recorded trajectory back, turn by turn, with no model involved.
    #[test]
    fn a_recorded_trajectory_is_served_over_http() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");

        let (status, body) = post(
            &proxy.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "Book a flight to Nairobi"}])),
        );
        assert_eq!(status, 200);
        let call = &body["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(call["function"]["name"], "search_flights");
        assert_eq!(call["function"]["arguments"], r#"{"destination":"NBO"}"#);
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");

        let (status, body) = post(
            &proxy.base_url(),
            chat(serde_json::json!([
                {"role": "user", "content": "Book a flight to Nairobi"},
                {"role": "tool", "content": r#"{"id":"KQ311"}"#},
            ])),
        );
        assert_eq!(status, 200);
        assert_eq!(body["choices"][0]["message"]["content"], "Booked KQ311.");
        assert_eq!(body["choices"][0]["finish_reason"], "stop");

        assert_eq!(proxy.log().served, 2);
        assert!(proxy.log().divergence.is_none());
    }

    /// POST and return `(status, content-type, raw body)` without parsing -
    /// a streaming reply is SSE frames, not one JSON object.
    fn post_stream(base: &str, payload: serde_json::Value) -> (u16, Option<String>, String) {
        let addr = base
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let body = payload.to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let request = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {addr}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
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
        let content_type = head
            .lines()
            .find_map(|l| l.strip_prefix("content-type: ").map(str::to_string));
        (status, content_type, body.to_string())
    }

    /// A `stream: true` client gets the SAME recorded turn as a well-formed
    /// synthetic SSE stream: chunk boundaries are not recorded, so a
    /// non-streaming cassette serves a streaming agent, turn for turn.
    #[test]
    fn a_streaming_client_is_served_a_synthetic_sse_stream() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");

        let (status, content_type, body) = post_stream(
            &proxy.base_url(),
            serde_json::json!({
                "model": "gpt-4o",
                "stream": true,
                "stream_options": { "include_usage": true },
                "messages": [{"role": "user", "content": "Book a flight to Nairobi"}],
                "tools": [{"type": "function", "function": {"name": "search_flights"}}],
            }),
        );
        assert_eq!(status, 200, "diverged unexpectedly: {body}");
        assert_eq!(content_type.as_deref(), Some("text/event-stream"));
        // Well formed: a role chunk first, the tool call carrying its WHOLE
        // arguments in one delta, a finish chunk, then the terminator.
        assert!(
            body.contains(r#""delta":{"role":"assistant"}"#),
            "role chunk missing: {body}"
        );
        assert!(body.contains("chat.completion.chunk"), "not chunks: {body}");
        assert!(
            body.contains(r#""name":"search_flights""#),
            "tool call: {body}"
        );
        assert!(
            body.contains("destination") && body.contains("NBO"),
            "whole arguments in one delta: {body}"
        );
        assert!(
            body.contains(r#""finish_reason":"tool_calls""#),
            "finish chunk: {body}"
        );
        assert!(
            body.trim_end().ends_with("data: [DONE]"),
            "terminator: {body}"
        );

        // The streamed turn was consumed exactly like a non-streaming one:
        // the next turn is the content reply, also streamed and terminated.
        let (status, _ct, body) = post_stream(
            &proxy.base_url(),
            serde_json::json!({
                "model": "gpt-4o",
                "stream": true,
                "messages": [
                    {"role": "user", "content": "Book a flight to Nairobi"},
                    {"role": "tool", "content": r#"{"id":"KQ311"}"#},
                ],
                "tools": [{"type": "function", "function": {"name": "search_flights"}}],
            }),
        );
        assert_eq!(status, 200, "second turn diverged: {body}");
        assert!(
            body.contains(r#""delta":{"content":"Booked KQ311."}"#),
            "content delta: {body}"
        );
        assert!(
            body.contains(r#""finish_reason":"stop""#),
            "stop finish: {body}"
        );
        assert!(
            body.trim_end().ends_with("data: [DONE]"),
            "terminator: {body}"
        );
        assert_eq!(proxy.log().served, 2);
        assert!(proxy.log().divergence.is_none());
    }

    /// End to end through the proxy: a mocked tool result makes an
    /// otherwise-divergent request MATCH. The cassette was recorded with
    /// the mock (post-substitution); at replay the agent sends a volatile
    /// real result, and substitution rewrites it to the mock before
    /// matching, so a tool that returns a fresh value every run does not
    /// fail replay.
    #[test]
    fn a_mocked_tool_result_lets_a_volatile_request_match() {
        // A two-turn cassette: turn 2's request carries the assistant
        // tool_call (naming the id) and the tool result, stored as the
        // MOCK - which is what record wrote after substituting.
        let cassette = Cassette {
            turns: vec![
                oai_turn(
                    TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![Message::new("user", "What time is it there?")],
                        tools: vec!["clock".into()],
                    },
                    Message {
                        role: "assistant".into(),
                        content: None,
                        tool_calls: vec![ToolCall {
                            id: "call_clock".into(),
                            name: "clock".into(),
                            arguments: "{}".into(),
                        }],
                        tool_call_id: None,
                    },
                ),
                oai_turn(
                    TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![
                            Message::new("user", "What time is it there?"),
                            Message {
                                role: "assistant".into(),
                                content: None,
                                tool_calls: vec![ToolCall {
                                    id: "call_clock".into(),
                                    name: "clock".into(),
                                    arguments: "{}".into(),
                                }],
                                tool_call_id: None,
                            },
                            // Stored as the mock, canonically.
                            Message {
                                role: "tool".into(),
                                content: Some(r#"{"now":"FIXED"}"#.into()),
                                tool_calls: Vec::new(),
                                tool_call_id: Some("call_clock".into()),
                            },
                        ],
                        tools: vec!["clock".into()],
                    },
                    Message::new("assistant", "It is fixed o'clock."),
                ),
            ],
        };
        let mocks: Mocks = [("clock".to_string(), serde_json::json!({"now": "FIXED"}))]
            .into_iter()
            .collect();
        let proxy = AgentProxy::start(cassette, mocks).expect("starts");

        // Turn 1.
        let (status, _) = post(
            &proxy.base_url(),
            serde_json::json!({
                "model": "gpt-4o",
                "messages": [{"role": "user", "content": "What time is it there?"}],
                "tools": [{"type": "function", "function": {"name": "clock"}}],
            }),
        );
        assert_eq!(status, 200);

        // Turn 2: the agent's REAL clock returned a live timestamp, which
        // is not what was recorded. Without substitution this diverges.
        let (status, body) = post(
            &proxy.base_url(),
            serde_json::json!({
                "model": "gpt-4o",
                "messages": [
                    {"role": "user", "content": "What time is it there?"},
                    {"role": "assistant", "tool_calls": [
                        {"id": "call_clock", "type": "function",
                         "function": {"name": "clock", "arguments": "{}"}}]},
                    {"role": "tool", "tool_call_id": "call_clock",
                     "content": "2026-07-23T09:41:07.123456Z"},
                ],
                "tools": [{"type": "function", "function": {"name": "clock"}}],
            }),
        );
        assert_eq!(
            status, 200,
            "substitution must make the volatile result match"
        );
        assert_eq!(
            body["choices"][0]["message"]["content"],
            "It is fixed o'clock."
        );
        assert!(proxy.log().divergence.is_none());
    }

    /// The Anthropic analogue of the headline OpenAI test: an agent making
    /// its usual Messages calls gets the recorded trajectory back over
    /// `/v1/messages`, turn by turn, Anthropic-shaped, with no model.
    #[test]
    fn an_anthropic_trajectory_is_served_over_v1_messages() {
        let proxy = AgentProxy::start(anthropic_cassette(), Mocks::new()).expect("starts");

        // Turn 1: a tool_use reply, rendered as Messages content blocks.
        let (status, body) = post_messages(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": "What's the weather in Paris?"}],
                "tools": [{"name": "get_weather", "description": "look it up",
                           "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(body["type"], "message");
        assert_eq!(body["role"], "assistant");
        let block = &body["content"][0];
        assert_eq!(block["type"], "tool_use");
        assert_eq!(block["name"], "get_weather");
        assert_eq!(block["id"], "toolu_1");
        assert_eq!(block["input"]["city"], "Paris");
        assert_eq!(body["stop_reason"], "tool_use");
        assert!(body["stop_sequence"].is_null());

        // Turn 2: the tool result comes back in a user content array, and
        // the recorded text reply is served with its recorded stop_reason.
        let (status, body) = post_messages(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "messages": [
                    {"role": "user", "content": "What's the weather in Paris?"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                         "input": {"city": "Paris"}}]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1",
                         "content": "sunny"}]},
                ],
                "tools": [{"name": "get_weather", "description": "look it up",
                           "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(status, 200);
        assert_eq!(body["content"][0]["type"], "text");
        assert_eq!(body["content"][0]["text"], "It is sunny in Paris.");
        assert_eq!(body["stop_reason"], "end_turn");

        assert_eq!(proxy.log().served, 2);
        assert!(proxy.log().divergence.is_none());
    }

    /// POST an Anthropic Messages body to `/v1/messages` and return
    /// `(status, content-type, raw body)` - a streamed reply is SSE frames.
    fn post_messages_stream(
        base: &str,
        payload: serde_json::Value,
    ) -> (u16, Option<String>, String) {
        let addr = base
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let body = payload.to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let request = format!(
            "POST /v1/messages HTTP/1.1\r\nhost: {addr}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
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
        let content_type = head
            .lines()
            .find_map(|l| l.strip_prefix("content-type: ").map(str::to_string));
        (status, content_type, body.to_string())
    }

    /// The Anthropic streaming analogue: a `stream: true` Messages client
    /// gets the recorded turn as a well-formed Anthropic SSE stream
    /// (`message_start` / `content_block_*` / `message_delta` /
    /// `message_stop`), synthesized from the same cassette a non-streaming
    /// client replays, turn for turn.
    #[test]
    fn an_anthropic_streaming_client_is_served_a_synthetic_sse_stream() {
        let proxy = AgentProxy::start(anthropic_cassette(), Mocks::new()).expect("starts");

        // Turn 1: a tool_use reply, streamed as content blocks.
        let (status, content_type, body) = post_messages_stream(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "stream": true,
                "messages": [{"role": "user", "content": "What's the weather in Paris?"}],
                "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(status, 200, "diverged: {body}");
        assert_eq!(content_type.as_deref(), Some("text/event-stream"));
        assert!(body.contains("event: message_start"), "start: {body}");
        assert!(
            body.contains(r#""type":"tool_use""#) && body.contains(r#""name":"get_weather""#),
            "tool_use block: {body}"
        );
        assert!(
            body.contains(r#""type":"input_json_delta""#) && body.contains("Paris"),
            "whole arguments in one delta: {body}"
        );
        assert!(
            body.contains("event: message_delta") && body.contains(r#""stop_reason":"tool_use""#),
            "message_delta stop reason: {body}"
        );
        assert!(body.contains("event: message_stop"), "terminator: {body}");

        // Turn 2: a text reply, streamed as a text block ending end_turn.
        let (status, _ct, body) = post_messages_stream(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "stream": true,
                "messages": [
                    {"role": "user", "content": "What's the weather in Paris?"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_1", "name": "get_weather",
                         "input": {"city": "Paris"}}]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "sunny"}]},
                ],
                "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(status, 200, "diverged: {body}");
        assert!(
            body.contains(r#""type":"text_delta""#) && body.contains("It is sunny in Paris."),
            "text delta: {body}"
        );
        assert!(
            body.contains(r#""stop_reason":"end_turn""#),
            "end_turn: {body}"
        );
        assert!(body.contains("event: message_stop"), "terminator: {body}");
        assert_eq!(proxy.log().served, 2);
        assert!(proxy.log().divergence.is_none());
    }

    /// The Anthropic substitution sibling: a mocked tool_result lets an
    /// otherwise-volatile Messages request MATCH. The cassette stored the
    /// mock (canonical) at record; at replay the agent's real tool_result
    /// arrives as a volatile text block and substitution rewrites it back to
    /// the mock before matching.
    #[test]
    fn a_mocked_tool_result_lets_a_volatile_anthropic_request_match() {
        let tool_use = Message {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: "toolu_c".into(),
                name: "thermo".into(),
                arguments: "{}".into(),
            }],
            tool_call_id: None,
        };
        let cassette = Cassette {
            turns: vec![
                ant_turn(
                    TurnRequest {
                        model: "claude-sonnet-4-5".into(),
                        messages: vec![Message::new("user", "Temperature?")],
                        tools: vec!["thermo".into()],
                    },
                    tool_use.clone(),
                    Some("tool_use"),
                ),
                ant_turn(
                    TurnRequest {
                        model: "claude-sonnet-4-5".into(),
                        messages: vec![
                            Message::new("user", "Temperature?"),
                            tool_use,
                            // Stored as the mock, canonically - what record
                            // wrote after substituting.
                            Message {
                                role: "tool".into(),
                                content: Some(r#"{"temp":"FIXED"}"#.into()),
                                tool_calls: Vec::new(),
                                tool_call_id: Some("toolu_c".into()),
                            },
                        ],
                        tools: vec!["thermo".into()],
                    },
                    Message::new("assistant", "It is fixed degrees."),
                    Some("end_turn"),
                ),
            ],
        };
        let mocks: Mocks = [("thermo".to_string(), serde_json::json!({"temp": "FIXED"}))]
            .into_iter()
            .collect();
        let proxy = AgentProxy::start(cassette, mocks).expect("starts");

        // Turn 1.
        let (status, _) = post_messages(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "messages": [{"role": "user", "content": "Temperature?"}],
                "tools": [{"name": "thermo", "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(status, 200);

        // Turn 2: the agent's REAL thermometer returned a live reading, as a
        // text block. Substitution must rewrite it to the mock so it matches.
        let (status, body) = post_messages(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 1024,
                "messages": [
                    {"role": "user", "content": "Temperature?"},
                    {"role": "assistant", "content": [
                        {"type": "tool_use", "id": "toolu_c", "name": "thermo", "input": {}}]},
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_c",
                         "content": [{"type": "text", "text": "18.7C at 09:41:07.123"}]}]},
                ],
                "tools": [{"name": "thermo", "input_schema": {"type": "object"}}],
            }),
        );
        assert_eq!(
            status, 200,
            "substitution must make the volatile result match"
        );
        assert_eq!(body["content"][0]["text"], "It is fixed degrees.");
        assert!(proxy.log().divergence.is_none());
    }

    /// Protocol is part of a turn's identity: a cassette recorded in the
    /// Anthropic dialect, replayed through the OpenAI endpoint, diverges on
    /// protocol FIRST - before any body diff between two shapes.
    #[test]
    fn a_protocol_mismatch_diverges() {
        // One anthropic turn, replayed via /chat/completions (openai).
        let cassette = Cassette {
            turns: vec![anthropic_cassette().turns.remove(0)],
        };
        let proxy = AgentProxy::start(cassette, Mocks::new()).expect("starts");

        let (status, body) = post(
            &proxy.base_url(),
            serde_json::json!({
                "model": "claude-sonnet-4-5",
                "messages": [{"role": "user", "content": "What's the weather in Paris?"}],
                "tools": [{"type": "function", "function": {"name": "get_weather"}}],
            }),
        );
        assert_eq!(status, 409);
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("protocol changed"), "{message}");
        assert!(
            message.contains("recorded anthropic, replayed openai"),
            "{message}"
        );
        assert!(message.starts_with("turn 1:"), "{message}");
        assert_eq!(proxy.log().served, 0);
        assert!(proxy.log().divergence.is_some());
    }

    /// A drifted prompt must not quietly succeed. The agent is owed an
    /// answer so it does not hang, and the run is owed the reason.
    #[test]
    fn a_divergence_is_reported_to_both_the_agent_and_the_run() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");
        let (status, body) = post(
            &proxy.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "Book a flight to Mombasa"}])),
        );
        assert_eq!(status, 409);
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(message.contains("content changed"), "{message}");
        assert!(message.starts_with("turn 1:"), "{message}");

        let log = proxy.log();
        assert_eq!(log.served, 0, "a divergent turn is not a served turn");
        assert!(log.divergence.is_some());
    }

    /// Sampling knobs do not change which conversation this is. A test
    /// must not break because somebody tuned temperature.
    #[test]
    fn sampling_parameters_are_ignored() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");
        let mut payload =
            chat(serde_json::json!([{"role": "user", "content": "Book a flight to Nairobi"}]));
        payload["temperature"] = serde_json::json!(0.0);
        payload["top_p"] = serde_json::json!(0.1);
        payload["seed"] = serde_json::json!(42);
        assert_eq!(post(&proxy.base_url(), payload).0, 200);
    }

    /// A body split across TCP segments must still be read whole. This is
    /// the failure mode of "read once into a buffer", and real prompts are
    /// big enough to hit it.
    #[test]
    fn a_body_arriving_in_pieces_is_read_to_its_declared_length() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");
        // Pad with an ignored field so the body comfortably exceeds a
        // single small segment.
        let mut payload =
            chat(serde_json::json!([{"role": "user", "content": "Book a flight to Nairobi"}]));
        payload["user"] = serde_json::json!("x".repeat(200_000));
        let body = payload.to_string();

        let addr = proxy
            .base_url()
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        let head = format!(
            "POST /v1/chat/completions HTTP/1.1\r\nhost: {addr}\r\n\
             content-type: application/json\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).expect("head");
        for chunk in body.as_bytes().chunks(8192) {
            stream.write_all(chunk).expect("chunk");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read");
        assert!(
            raw.starts_with("HTTP/1.1 200"),
            "{}",
            &raw[..60.min(raw.len())]
        );
    }

    #[test]
    fn other_endpoints_are_refused_rather_than_guessed_at() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");
        let addr = proxy
            .base_url()
            .trim_start_matches("http://")
            .trim_end_matches("/v1")
            .to_string();
        let mut stream = TcpStream::connect(&addr).expect("connect");
        stream
            .write_all(
                format!("GET /v1/models HTTP/1.1\r\nhost: {addr}\r\ncontent-length: 0\r\n\r\n")
                    .as_bytes(),
            )
            .expect("write");
        let mut raw = String::new();
        stream.read_to_string(&mut raw).expect("read");
        assert!(raw.starts_with("HTTP/1.1 404"), "{raw}");
    }

    /// A local fake model, so record mode can be exercised without a real
    /// API or any tokens - the same bet the SAP simulator makes.
    fn fake_model(reply: serde_json::Value) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 8192];
                let _ = stream.read(&mut buf);
                let body = reply.to_string();
                let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    /// The real round trip: RECORD against a fake model captures a
    /// cassette, and REPLAYING that cassette serves the same reply back
    /// with no model at all.
    #[test]
    fn a_recorded_exchange_replays_from_the_captured_cassette() {
        let upstream = fake_model(serde_json::json!({
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "Recorded reply."}}],
        }));

        // Record.
        let rec = AgentProxy::record(&upstream, None, Mocks::new()).expect("record starts");
        let (status, body) = post(
            &rec.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "hi"}])),
        );
        assert_eq!(status, 200);
        assert_eq!(body["choices"][0]["message"]["content"], "Recorded reply.");
        let cassette = rec.captured();
        assert_eq!(cassette.len(), 1);
        assert!(rec.log().upstream_error.is_none());
        drop(rec);

        // Replay the captured cassette - no upstream this time.
        let replay = AgentProxy::start(cassette, Mocks::new()).expect("replay starts");
        let (status, body) = post(
            &replay.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "hi"}])),
        );
        assert_eq!(status, 200);
        assert_eq!(body["choices"][0]["message"]["content"], "Recorded reply.");
        assert_eq!(replay.log().served, 1);
        assert!(replay.log().divergence.is_none());
    }

    /// An upstream that fails is recorded as an error, not a success, and
    /// no cassette turn is captured - a broken record must not mint a
    /// trace.
    #[test]
    fn a_failed_upstream_is_an_error_not_a_capture() {
        // Point at a port nothing answers.
        let rec = AgentProxy::record("http://127.0.0.1:9/v1", None, Mocks::new()).expect("starts");
        let (status, _) = post(
            &rec.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "hi"}])),
        );
        assert_eq!(status, 502);
        assert!(rec.log().upstream_error.is_some());
        assert_eq!(rec.captured().len(), 0);
    }

    /// The auth header flowproof was given is forwarded to the upstream,
    /// and it never enters the captured cassette - the trace stores bodies
    /// only, so a recording carries no secret.
    #[test]
    fn the_auth_header_is_forwarded_but_never_captured() {
        use std::sync::mpsc;
        let (tx, rx) = mpsc::channel();
        // A fake model that reports back the Authorization header it saw.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            if let Some(stream) = listener.incoming().next() {
                let mut stream = stream.expect("accept");
                let mut buf = vec![0u8; 8192];
                let n = std::io::Read::read(&mut stream, &mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let seen = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("authorization:"))
                    .unwrap_or("(none)")
                    .to_string();
                let _ = tx.send(seen);
                let body = serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "stop",
                        "message": {"role": "assistant", "content": "ok"}}]
                })
                .to_string();
                let _ = std::io::Write::write_all(
                    &mut stream,
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                let _ = std::io::Write::flush(&mut stream);
                let _ = stream.shutdown(std::net::Shutdown::Write);
            }
        });
        let upstream = format!("http://127.0.0.1:{port}/v1");

        let rec = AgentProxy::record(&upstream, Some("Bearer sekret-123".into()), Mocks::new())
            .expect("record starts");
        let (status, _) = post(
            &rec.base_url(),
            chat(serde_json::json!([{"role": "user", "content": "hi"}])),
        );
        assert_eq!(status, 200);

        let seen = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("header seen");
        assert!(seen.to_lowercase().contains("bearer sekret-123"), "{seen}");

        // The secret must NOT be in the captured cassette.
        let cassette = rec.captured();
        let json = serde_json::to_string(&cassette).expect("serialize");
        assert!(
            !json.contains("sekret-123"),
            "no secret in the trace: {json}"
        );
    }

    /// It answers whatever asks it, with no authentication, so it must not
    /// be reachable off this machine.
    #[test]
    fn the_proxy_listens_only_on_loopback() {
        let proxy = AgentProxy::start(cassette(), Mocks::new()).expect("starts");
        assert!(
            proxy.base_url().starts_with("http://127.0.0.1:"),
            "{}",
            proxy.base_url()
        );
    }
}
