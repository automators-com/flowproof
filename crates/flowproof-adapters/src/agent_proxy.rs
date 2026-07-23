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

use flowproof_trace::cassette::{Cassette, Divergence, Message, ToolCall, TurnRequest};
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
}

/// A running proxy. Dropping it stops the listener.
pub struct AgentProxy {
    addr: SocketAddr,
    log: Arc<Mutex<ProxyLog>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AgentProxy {
    /// Start serving `cassette` on an ephemeral localhost port.
    ///
    /// Bound to 127.0.0.1 on purpose: this endpoint answers whatever asks
    /// it, with no authentication, so it must not be reachable off the
    /// machine running the test.
    pub fn start(cassette: Cassette, mocks: Mocks) -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?;
        // A short read timeout lets the accept loop notice `stop` even
        // when a client connects and then says nothing.
        listener.set_nonblocking(true)?;

        let log = Arc::new(Mutex::new(ProxyLog::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (log, stop) = (Arc::clone(&log), Arc::clone(&stop));
            std::thread::spawn(move || {
                let mut turn = 0usize;
                while !stop.load(Ordering::Relaxed) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            stream.set_nonblocking(false).ok();
                            serve_one(stream, &cassette, &mocks, &mut turn, &log);
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
        })
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

/// Read one request, answer it, close. `Connection: close` every time:
/// keep-alive would buy nothing here and multiplexing state machines are
/// where hand-rolled HTTP goes wrong.
fn serve_one(
    stream: TcpStream,
    cassette: &Cassette,
    mocks: &Mocks,
    turn: &mut usize,
    log: &Mutex<ProxyLog>,
) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;

    let Some((path, body)) = read_request(&mut reader) else {
        respond(
            &mut writer,
            &mut reader,
            &response(400, r#"{"error":"malformed request"}"#),
        );
        return;
    };
    if !path.contains("/chat/completions") {
        respond(
            &mut writer,
            &mut reader,
            &response(404, r#"{"error":"only /v1/chat/completions is served"}"#),
        );
        return;
    }

    let mut incoming = match parse_request(&body) {
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

    // Substitute mocked tool results BEFORE matching, exactly as record
    // did before storing, so the compared request is the one the model
    // actually saw - a volatile real tool result cannot fail replay.
    substitution::apply(&mut incoming, mocks);

    let index = *turn;
    *turn += 1;
    match cassette.turn(index, &incoming) {
        Ok(recorded) => {
            log.lock().unwrap_or_else(|e| e.into_inner()).served += 1;
            respond(
                &mut writer,
                &mut reader,
                &response(200, &completion_body(&recorded.message)),
            );
        }
        Err(divergence) => {
            // The agent is owed an answer or it will hang; the run is owed
            // the truth. A 409 with the divergence in the body does both,
            // and the recorded reason is what the test reports - an agent
            // that swallows the error must not turn a divergence into a
            // pass.
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
    }
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

/// Read the request line, headers, and exactly `content-length` bytes.
///
/// Reading a fixed-size buffer once would be shorter and wrong: a
/// trajectory's later prompts run to tens of kilobytes and arrive across
/// several TCP segments, so the body has to be read to its declared
/// length rather than to whatever happened to have landed.
fn read_request(reader: &mut BufReader<TcpStream>) -> Option<(String, Vec<u8>)> {
    let mut request_line = String::new();
    reader.read_line(&mut request_line).ok()?;
    let path = request_line.split_whitespace().nth(1)?.to_string();

    let mut length = 0usize;
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
            }
        }
    }
    if length > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).ok()?;
    Some((path, body))
}

/// Pull the comparable request out of an OpenAI-compatible payload.
///
/// Only the fields the cassette matches on are taken. Sampling knobs
/// (temperature, top_p, seed) are deliberately ignored: they do not change
/// which conversation this is, and matching on them would make a test fail
/// because someone tuned a dial.
fn parse_request(body: &[u8]) -> Result<TurnRequest, String> {
    let json: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not JSON ({e})"))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use flowproof_trace::cassette::{Turn, TurnResponse};

    fn cassette() -> Cassette {
        Cassette {
            turns: vec![
                Turn {
                    request: TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![Message::new("user", "Book a flight to Nairobi")],
                        tools: vec!["search_flights".into()],
                    },
                    response: TurnResponse {
                        message: Message {
                            role: "assistant".into(),
                            content: None,
                            tool_calls: vec![ToolCall {
                                id: "call_1".into(),
                                name: "search_flights".into(),
                                arguments: r#"{"destination":"NBO"}"#.into(),
                            }],
                            tool_call_id: None,
                        },
                    },
                },
                Turn {
                    request: TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![
                            Message::new("user", "Book a flight to Nairobi"),
                            Message::new("tool", r#"{"id":"KQ311"}"#),
                        ],
                        tools: vec!["search_flights".into()],
                    },
                    response: TurnResponse {
                        message: Message::new("assistant", "Booked KQ311."),
                    },
                },
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

    fn chat(messages: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "model": "gpt-4o",
            "messages": messages,
            "temperature": 0.7,
            "tools": [{"type": "function", "function": {"name": "search_flights"}}],
        })
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
                Turn {
                    request: TurnRequest {
                        model: "gpt-4o".into(),
                        messages: vec![Message::new("user", "What time is it there?")],
                        tools: vec!["clock".into()],
                    },
                    response: TurnResponse {
                        message: Message {
                            role: "assistant".into(),
                            content: None,
                            tool_calls: vec![ToolCall {
                                id: "call_clock".into(),
                                name: "clock".into(),
                                arguments: "{}".into(),
                            }],
                            tool_call_id: None,
                        },
                    },
                },
                Turn {
                    request: TurnRequest {
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
                    response: TurnResponse {
                        message: Message::new("assistant", "It is fixed o'clock."),
                    },
                },
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
