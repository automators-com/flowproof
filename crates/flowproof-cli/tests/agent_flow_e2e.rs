//! End to end for `app: agent`, with no real model and no agent framework:
//! a fake model (a local HTTP server returning a scripted trajectory) and a
//! fake agent (a small Python process speaking the model dialect under
//! test - chat-completions, or the Anthropic Messages API). The full spec ->
//! record -> cassette -> replay path runs, exactly as CI proves it on every
//! push.
//!
//! Unix-only for the same reason as the other suite tests: the fake agent
//! is a `python3` process and the assertions are platform-neutral.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// Serializes the tests that mutate the process-global `FLOWPROOF_AGENT_UPSTREAM`
/// env var. `cargo test` runs a binary's tests on parallel threads and env vars
/// are process-global, so without this lock one test's `set_var` races another's
/// read: the agent child can pick up a different test's upstream address and the
/// run flakes. Each such test holds this guard for its whole body. Poison-tolerant
/// so one panicking test does not cascade a failure into all the others.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-agent-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// A fake model: two scripted turns. First it asks for `get_weather`; once
/// it has a tool result, it replies. Serves each connection once and
/// exits when the record run is done (bounded accept count).
fn fake_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            // Read the whole request to its content-length: a body can
            // arrive across segments, and closing with bytes still unread
            // makes the stack RST the client (ureq then sees a reset).
            let req = read_http_request(&mut stream);
            // The model asks for the tool until it sees a tool result.
            let reply = if req.contains("\"role\":\"tool\"") || req.contains("\"role\": \"tool\"") {
                serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "stop",
                        "message": {"role": "assistant",
                            "content": "It is sunny in Nairobi."}}]
                })
            } else {
                serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "tool_calls",
                        "message": {"role": "assistant", "content": null,
                            "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "get_weather",
                                    "arguments": "{\"city\":\"Nairobi\"}"}}]}}]
                })
            };
            let body = reply.to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Read an HTTP/1.1 request to the end of its declared body.
fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(stream);
    let mut head = String::new();
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        let done = line == "\r\n" || line == "\n";
        head.push_str(&line);
        if done {
            break;
        }
    }
    let mut body = vec![0u8; length];
    let _ = reader.read_exact(&mut body);
    head + &String::from_utf8_lossy(&body)
}

/// A fake agent: reads the prompt and model URL from the env flowproof
/// injects, drives the model until it gets a text reply, and executes its
/// "real" weather tool - which returns a VOLATILE value, so replay only
/// works because the mock is substituted.
const FAKE_AGENT: &str = r#"
import json, os, time, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
messages = [{"role": "user", "content": prompt}]

for _ in range(5):
    payload = json.dumps({
        "model": "gpt-4o",
        "messages": messages,
        "tools": [{"type": "function", "function": {"name": "get_weather"}}],
    }).encode()
    req = urllib.request.Request(base + "/chat/completions", data=payload,
                                headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as resp:
        msg = json.load(resp)["choices"][0]["message"]
    if msg.get("tool_calls"):
        messages.append(msg)
        for call in msg["tool_calls"]:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"observed_at": time.time_ns(), "sky": "clear"})
            messages.append({"role": "tool", "tool_call_id": call["id"], "content": real})
        continue
    print(msg.get("content", ""))
    break
"#;

fn write_spec(dir: &Path, agent_py: &Path) -> PathBuf {
    let spec = dir.join("weather.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Weather assistant\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             tools:\n  - name: get_weather\n    result: {{ sky: clear, temp: 25 }}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert_tool_call: get_weather where city equals Nairobi\n\
             \x20 - assert_no_tool_call: send_alert\n\
             \x20 - assert: reply contains sunny\n",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

#[test]
fn records_and_replays_an_agent_flow() {
    let _env = lock_env();
    let dir = work_dir("weather");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let spec = write_spec(&dir, &agent_py);

    // RECORD against the fake model.
    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording an agent flow should succeed");

    let trace = dir.join("weather.trace.jsonl");
    assert!(trace.exists(), "a cassette trace must be written");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(contents.contains("\"app\": \"agent\""), "{contents}");
    // The mock, not the volatile real result, is what the trajectory pins.
    assert!(
        contents.contains("clear"),
        "the mock is snapshotted: {contents}"
    );
    assert!(
        !contents.contains("observed_at"),
        "the volatile real tool result must not be in the trace: {contents}"
    );

    // REPLAY with no model at all - unset the upstream so a stray real
    // call would fail loudly rather than sneak through.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay must reproduce the recorded trajectory");

    std::fs::remove_dir_all(&dir).ok();
}

// ---- the Anthropic Messages dialect ----

/// A fake Anthropic Messages upstream: the same two scripted turns as
/// [`fake_model`], spoken in the other dialect. It asks for `get_weather`
/// until a `tool_result` block comes back, then answers with text.
///
/// The base URL it hands out carries NO `/v1`, because the record path
/// appends `/v1/messages` itself - the same shape `https://api.anthropic.com`
/// has. So the URL this test drives is the URL a real recording builds,
/// rather than one bent to suit the test.
fn fake_anthropic_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            // The model asks for the tool until it sees a tool result.
            let (content, stop_reason) = if req.contains("tool_result") {
                (
                    serde_json::json!([{"type": "text", "text": "It is sunny in Nairobi."}]),
                    "end_turn",
                )
            } else {
                (
                    serde_json::json!([{"type": "tool_use", "id": "toolu_1",
                        "name": "get_weather", "input": {"city": "Nairobi"}}]),
                    "tool_use",
                )
            };
            let body = serde_json::json!({
                "id": "msg_fake",
                "type": "message",
                "role": "assistant",
                "model": "claude-sonnet-4-5",
                "content": content,
                "stop_reason": stop_reason,
                "stop_sequence": serde_json::Value::Null,
                "usage": {"input_tokens": 0, "output_tokens": 0},
            })
            .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The Anthropic-dialect sibling of [`FAKE_AGENT`]: it reads
/// `ANTHROPIC_BASE_URL` (the `/v1`-less form the SDK expects, which is why it
/// appends `/v1/messages` itself), speaks content blocks rather than
/// `tool_calls`, and returns its tool result in a user turn - the shape the
/// Messages API actually uses. Its "real" tool is volatile in the same way,
/// so replay only works because the mock is substituted.
const ANTHROPIC_AGENT: &str = r#"
import json, os, time, urllib.request

base = os.environ["ANTHROPIC_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
messages = [{"role": "user", "content": prompt}]

for _ in range(5):
    payload = json.dumps({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "messages": messages,
        "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
    }).encode()
    req = urllib.request.Request(base + "/v1/messages", data=payload,
                                headers={"content-type": "application/json",
                                         "anthropic-version": "2023-06-01"})
    with urllib.request.urlopen(req) as resp:
        blocks = json.load(resp)["content"]
    uses = [b for b in blocks if b.get("type") == "tool_use"]
    if uses:
        messages.append({"role": "assistant", "content": blocks})
        results = []
        for use in uses:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"observed_at": time.time_ns(), "sky": "clear"})
            results.append({"type": "tool_result", "tool_use_id": use["id"],
                            "content": real})
        messages.append({"role": "user", "content": results})
        continue
    print("".join(b.get("text", "") for b in blocks if b.get("type") == "text"))
    break
"#;

fn write_anthropic_spec(dir: &Path, agent_py: &Path) -> PathBuf {
    let spec = dir.join("weather-anthropic.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Weather assistant (Anthropic)\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             tools:\n  - name: get_weather\n    result: {{ sky: clear, temp: 25 }}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert_tool_call: get_weather where city equals Nairobi\n\
             \x20 - assert_no_tool_call: send_alert\n\
             \x20 - assert: reply contains sunny\n",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

/// Issue #209: the Anthropic Messages dialect had never been recorded end to
/// end, only replayed from cassettes written by hand. That left the record
/// leg - the upstream URL, the `x-api-key` conversion, the block-shaped
/// request parser, the `stop_reason` capture - resting on an untested claim
/// in `docs/agent-testing.md`.
///
/// This is `records_and_replays_an_agent_flow` in the other dialect, and it
/// asserts the same thing the OpenAI path does: the mock is what the
/// trajectory pins, the volatile real result never reaches disk, and the
/// recording replays with no upstream at all.
#[test]
fn records_and_replays_an_anthropic_agent_flow() {
    let _env = lock_env();
    let dir = work_dir("weather-anthropic");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, ANTHROPIC_AGENT).expect("agent");
    let spec = write_anthropic_spec(&dir, &agent_py);

    // RECORD against the fake Messages upstream.
    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_anthropic_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording an Anthropic agent flow should succeed");

    let trace = dir.join("weather-anthropic.trace.jsonl");
    assert!(trace.exists(), "a cassette trace must be written");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(contents.contains("\"app\": \"agent\""), "{contents}");
    // The turn is stamped with the dialect it was spoken in, which is what
    // makes replay serve it back as Messages rather than chat-completions.
    assert!(
        contents.contains("\"protocol\": \"anthropic\""),
        "the recorded turn names its dialect: {contents}"
    );
    // The Messages API's own stop reason is captured, not inferred.
    assert!(
        contents.contains("\"stop_reason\": \"tool_use\""),
        "the upstream stop_reason is recorded: {contents}"
    );
    // The tool call the flow asserts on is in the recording, with the
    // argument the assertion matches - so a green replay is not vacuous.
    assert!(
        contents.contains("\"name\": \"get_weather\"") && contents.contains("Nairobi"),
        "the recorded trajectory carries the tool call: {contents}"
    );
    // The mock, not the volatile real result, is what the trajectory pins.
    assert!(
        contents.contains("clear"),
        "the mock is snapshotted: {contents}"
    );
    assert!(
        !contents.contains("observed_at"),
        "the volatile real tool result must not be in the trace: {contents}"
    );

    // REPLAY with no model at all - unset every upstream handle so a stray
    // real call would fail loudly rather than sneak through.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    std::env::remove_var("ANTHROPIC_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay must reproduce the recorded trajectory");

    std::fs::remove_dir_all(&dir).ok();
}

// ---- streaming replay ----

/// A streaming sibling of [`FAKE_AGENT`]: it asks for `stream: true` and
/// reads the answer as Server-Sent Events, recording the FRAME BOUNDARIES it
/// saw rather than only the text it assembled. Each run appends one JSON line
/// to `__LOG__` - a list with one entry per model call, each entry the ordered
/// frames of that call - so the record leg and the replay leg can be compared
/// chunk for chunk.
///
/// The first frame recorded is the response's content type, so a stream that
/// was collapsed into one buffered JSON body is visible in the log instead of
/// silently assembling into the same final text.
const STREAMING_AGENT: &str = r#"
import json, os, time, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
messages = [{"role": "user", "content": prompt}]
log = []

for _ in range(5):
    payload = json.dumps({
        "model": "gpt-4o",
        "stream": True,
        "stream_options": {"include_usage": True},
        "messages": messages,
        "tools": [{"type": "function", "function": {"name": "get_weather"}}],
    }).encode()
    req = urllib.request.Request(base + "/chat/completions", data=payload,
                                headers={"content-type": "application/json"})
    frames = []
    content = ""
    calls = []
    with urllib.request.urlopen(req) as resp:
        kind = resp.headers.get("content-type", "")
        frames.append("content-type:" + kind)
        if "text/event-stream" not in kind:
            # Tolerate a buffered answer rather than failing on it: the same
            # trajectory still assembles, so the ONLY evidence that the stream
            # was collapsed is the frame log.
            msg = json.load(resp)["choices"][0]["message"]
            content = msg.get("content") or ""
            calls = msg.get("tool_calls") or []
        else:
            for raw in resp:
                line = raw.decode("utf-8").strip()
                if not line.startswith("data:"):
                    continue
                data = line[len("data:"):].strip()
                if data == "[DONE]":
                    frames.append("DONE")
                    break
                choice = json.loads(data)["choices"][0]
                delta = choice.get("delta", {})
                if "role" in delta:
                    frames.append("role:" + delta["role"])
                if delta.get("content"):
                    frames.append("content:" + delta["content"])
                    content += delta["content"]
                for call in delta.get("tool_calls", []):
                    fn = call["function"]
                    frames.append("tool:" + fn["name"] + ":" + fn["arguments"])
                    calls.append(call)
                if choice.get("finish_reason"):
                    frames.append("finish:" + choice["finish_reason"])
    log.append(frames)
    if calls:
        messages.append({"role": "assistant", "content": None, "tool_calls": calls})
        for call in calls:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"observed_at": time.time_ns(), "sky": "clear"})
            messages.append({"role": "tool", "tool_call_id": call["id"], "content": real})
        continue
    print(content)
    break

with open("__LOG__", "a") as fh:
    fh.write(json.dumps(log) + "\n")
"#;

/// The frames a well-formed synthetic stream delivers for this trajectory:
/// an event-stream content type, the role on its own, the tool call carrying
/// its WHOLE arguments in one delta, the finish reason, then the terminator -
/// and then the same shape for the text turn that closes the trajectory.
fn expected_stream_frames() -> serde_json::Value {
    serde_json::json!([
        [
            "content-type:text/event-stream",
            "role:assistant",
            r#"tool:get_weather:{"city":"Nairobi"}"#,
            "finish:tool_calls",
            "DONE",
        ],
        [
            "content-type:text/event-stream",
            "role:assistant",
            "content:It is sunny in Nairobi.",
            "finish:stop",
            "DONE",
        ],
    ])
}

/// Read the frame log one run appended, by line: line 0 is the record leg,
/// line 1 the replay leg.
fn stream_frames(log: &Path, line: usize) -> serde_json::Value {
    let contents = std::fs::read_to_string(log).expect("the agent wrote its frame log");
    let line = contents
        .lines()
        .nth(line)
        .unwrap_or_else(|| panic!("the frame log has no line {line}: {contents}"));
    serde_json::from_str(line).expect("each line is a JSON list of frames")
}

/// Issue #210: streaming replay had no end-to-end cover at all. The unit
/// tests drive the proxy directly, so nothing proved that a `stream: true`
/// agent driven by `flowproof record` and then `flowproof run` is served a
/// stream in both phases - and the record-mode synthesis had no test of any
/// kind.
///
/// What makes this test worth having is WHERE it asserts. A streaming client
/// that was handed one buffered response would still assemble the same final
/// text and still satisfy `assert: reply contains sunny`, so asserting on the
/// text would be a test that cannot fail for its own bug. The assertion is on
/// the frame boundaries the agent observed: the content type, the role frame
/// on its own, the arguments in one delta, the finish frame, the terminator.
///
/// Chunk boundaries are synthesized, not recorded (see `docs/agent-testing.md`
/// on v2) - so the second thing asserted is that the record leg and the
/// replay leg produce the SAME boundaries from a cassette that contains no
/// stream at all.
#[test]
fn a_streaming_agent_is_served_a_stream_at_record_and_at_replay() {
    let _env = lock_env();
    let dir = work_dir("streaming");
    let agent_py = dir.join("agent.py");
    let log = dir.join("frames.jsonl");
    std::fs::write(
        &agent_py,
        STREAMING_AGENT.replace("__LOG__", log.to_str().expect("utf8")),
    )
    .expect("agent");
    let spec = write_spec(&dir, &agent_py);

    // RECORD. The upstream is answered non-streaming on purpose: `stream` is
    // transport, so the proxy strips it before forwarding and synthesizes the
    // stream back to the agent itself. That synthesis is what this leg covers.
    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording a streaming agent flow should succeed");

    let recorded = stream_frames(&log, 0);
    assert_eq!(
        recorded,
        expected_stream_frames(),
        "the record leg must serve the agent a stream, frame for frame"
    );

    // The cassette holds the assembled turn and nothing of the transport:
    // no frames, no event-stream, no `stream` flag left in the request.
    let trace = dir.join("weather.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        !contents.contains("event-stream") && !contents.contains("chat.completion.chunk"),
        "chunk boundaries are synthesized, never recorded: {contents}"
    );
    assert!(
        !contents.contains("\"stream\""),
        "`stream` is transport and must not enter the comparison: {contents}"
    );

    // REPLAY with no model at all - a stray real call would fail loudly.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay must reproduce the recorded trajectory");

    // The whole point: replay serves a STREAM, with the same boundaries the
    // record leg had. A replay that collapsed the turn into one buffered
    // response would still pass `assert: reply contains sunny` and fails
    // here - on the content type and on every frame after it.
    let replayed = stream_frames(&log, 1);
    assert_eq!(
        replayed,
        expected_stream_frames(),
        "replay must serve a stream, not a buffered response"
    );
    assert_eq!(
        replayed, recorded,
        "record and replay must agree chunk for chunk"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The Anthropic-dialect sibling of [`STREAMING_AGENT`]. Its frames are the
/// Messages event names rather than chat-completion chunks, so it pins the
/// OTHER synthesis path: `message_start`, one `content_block_start` /
/// delta / `content_block_stop` per block, `message_delta` carrying the stop
/// reason, `message_stop`. Same buffered-body tolerance, for the same reason.
const ANTHROPIC_STREAMING_AGENT: &str = r#"
import json, os, time, urllib.request

base = os.environ["ANTHROPIC_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
messages = [{"role": "user", "content": prompt}]
log = []

for _ in range(5):
    payload = json.dumps({
        "model": "claude-sonnet-4-5",
        "max_tokens": 1024,
        "stream": True,
        "messages": messages,
        "tools": [{"name": "get_weather", "input_schema": {"type": "object"}}],
    }).encode()
    req = urllib.request.Request(base + "/v1/messages", data=payload,
                                headers={"content-type": "application/json",
                                         "anthropic-version": "2023-06-01"})
    frames = []
    blocks = []
    with urllib.request.urlopen(req) as resp:
        kind = resp.headers.get("content-type", "")
        frames.append("content-type:" + kind)
        if "text/event-stream" not in kind:
            blocks = json.load(resp)["content"]
        else:
            for raw in resp:
                line = raw.decode("utf-8").strip()
                if not line.startswith("data:"):
                    continue
                event = json.loads(line[len("data:"):].strip())
                name = event["type"]
                if name == "content_block_start":
                    block = event["content_block"]
                    blocks.append(block)
                    frames.append("block_start:" + block["type"] + ":"
                                  + block.get("name", "-"))
                elif name == "content_block_delta":
                    delta = event["delta"]
                    block = blocks[event["index"]]
                    if delta["type"] == "text_delta":
                        frames.append("text_delta:" + delta["text"])
                        block["text"] = block.get("text", "") + delta["text"]
                    else:
                        frames.append("input_json_delta:" + delta["partial_json"])
                        block["input"] = json.loads(delta["partial_json"])
                elif name == "content_block_stop":
                    frames.append("block_stop:%d" % event["index"])
                elif name == "message_delta":
                    frames.append("message_delta:" + event["delta"]["stop_reason"])
                else:
                    frames.append(name)
    log.append(frames)
    uses = [b for b in blocks if b.get("type") == "tool_use"]
    if uses:
        messages.append({"role": "assistant", "content": blocks})
        results = []
        for use in uses:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"observed_at": time.time_ns(), "sky": "clear"})
            results.append({"type": "tool_result", "tool_use_id": use["id"],
                            "content": real})
        messages.append({"role": "user", "content": results})
        continue
    print("".join(b.get("text", "") for b in blocks if b.get("type") == "text"))
    break

with open("__LOG__", "a") as fh:
    fh.write(json.dumps(log) + "\n")
"#;

/// The Messages-dialect frames for the same trajectory: the tool call's whole
/// arguments arrive as one `input_json_delta`, the reply as one `text_delta`,
/// each inside its own start/stop pair.
fn expected_anthropic_stream_frames() -> serde_json::Value {
    serde_json::json!([
        [
            "content-type:text/event-stream",
            "message_start",
            "block_start:tool_use:get_weather",
            r#"input_json_delta:{"city":"Nairobi"}"#,
            "block_stop:0",
            "message_delta:tool_use",
            "message_stop",
        ],
        [
            "content-type:text/event-stream",
            "message_start",
            "block_start:text:-",
            "text_delta:It is sunny in Nairobi.",
            "block_stop:0",
            "message_delta:end_turn",
            "message_stop",
        ],
    ])
}

/// The other half of #210: the Messages dialect synthesizes its stream in a
/// different function, with different frames, and had the same hole - unit
/// tests drove the proxy directly, and the record-mode synthesis had nothing
/// at all. Same shape as the OpenAI case, same reason the assertion is on
/// boundaries rather than on the assembled text.
#[test]
fn a_streaming_anthropic_agent_is_served_a_stream_at_record_and_at_replay() {
    let _env = lock_env();
    let dir = work_dir("streaming-anthropic");
    let agent_py = dir.join("agent.py");
    let log = dir.join("frames.jsonl");
    std::fs::write(
        &agent_py,
        ANTHROPIC_STREAMING_AGENT.replace("__LOG__", log.to_str().expect("utf8")),
    )
    .expect("agent");
    let spec = write_anthropic_spec(&dir, &agent_py);

    // RECORD against the fake Messages upstream, which answers non-streaming.
    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_anthropic_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(
        code, 0,
        "recording a streaming Messages flow should succeed"
    );
    assert_eq!(
        stream_frames(&log, 0),
        expected_anthropic_stream_frames(),
        "the record leg must serve the agent Messages events, frame for frame"
    );

    // REPLAY with no model at all - a stray real call would fail loudly.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    std::env::remove_var("ANTHROPIC_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay must reproduce the recorded trajectory");
    assert_eq!(
        stream_frames(&log, 1),
        expected_anthropic_stream_frames(),
        "replay must serve a stream, not a buffered response"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #188, end to end: an agent that cannot even start must be reported
/// as a dead process with its own stderr attached, not as "0 model calls".
///
/// This is the README's frictionless first green run on a machine that is
/// missing the agent's OWN dependency - many adopters' first contact with
/// flowproof. The symptom used to read as *flowproof could not replay* while
/// the traceback that explained everything was captured and thrown away, so
/// the first conclusion an adopter reached was that the tool is broken.
///
/// A good cassette is recorded first, then the agent is broken underneath
/// it: the recording is fine, the agent is not, and the message must say so.
#[test]
fn an_agent_that_cannot_start_blames_the_agent_and_prints_its_stderr() {
    let _env = lock_env();
    let dir = work_dir("dead-agent");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let spec = write_spec(&dir, &agent_py);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "the cassette must record cleanly first");
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");

    // Now break the agent the way a clean machine does: its import fails, it
    // exits 1, and it never reaches the proxy.
    std::fs::write(&agent_py, "import definitely_not_installed_pkg\n").expect("agent");
    let output = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec.to_str().expect("utf8")])
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("run flowproof run");

    assert!(!output.status.success(), "a dead agent is not a pass");
    // The verdict line goes to stdout.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("exited 1 without making any model call"),
        "the failure names the dead process: {stdout}"
    );
    assert!(
        stdout.contains("definitely_not_installed_pkg"),
        "the agent's own stderr is what explains it: {stdout}"
    );
    assert!(
        stdout.contains("agent stderr:"),
        "the stderr is labelled as the agent's, not flowproof's: {stdout}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A failing assertion at record time refuses the trace, the same rule
/// every other app kind has. Here the flow demands a tool the agent never
/// calls.
#[test]
fn a_failing_assertion_refuses_the_trace() {
    let _env = lock_env();
    let dir = work_dir("refuse");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let spec = dir.join("bad.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Demands a missing tool\n\
             app: agent\n\
             agent:\n  command: python3 {}\n\
             tools:\n  - name: get_weather\n    result: {{ sky: clear }}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert_tool_call: book_flight\n",
            agent_py.display()
        ),
    )
    .expect("spec");

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_ne!(code, 0, "an unmet assertion must fail the record");
    assert!(
        !dir.join("bad.trace.jsonl").exists(),
        "no trace for a trajectory that failed its assertions"
    );

    std::fs::remove_dir_all(&dir).ok();
}

// ---- assert_no_secret_leak (security spine) ----

/// A resolved DB password that is long enough to scan for precisely and
/// distinctive enough that a substring hit is unambiguous.
const SECRET: &str = "s3cr3t-db-pw-90210";

/// The flowproof binary, so a record run's stderr can be READ - the message
/// assertions (names the variable, names the step, never prints the value)
/// need the text, which `run_cli` prints to this process's stderr. Running
/// out of process also scopes the env to the child via `.env(...)`, so these
/// tests never mutate the process-global `FLOWPROOF_AGENT_UPSTREAM` and need
/// no `ENV_LOCK`.
const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// A one-turn fake model: it always replies with a fixed text, so the run is
/// driven entirely by whatever the agent sends. Serves a bounded number of
/// connections then exits.
fn secret_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_http_request(&mut stream);
            let body = serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "acknowledged"}}]
            })
            .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// A fake agent that echoes the `LEAK_ME` env var (if flowproof injected
/// one) into the message it sends to the model - so the resolved secret
/// lands in the cassette request body, the corpus the scan reads. When
/// `LEAK_ME` is unset, the message carries no secret and the run is clean.
const SECRET_AGENT: &str = r#"
import json, os, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
leak = os.environ.get("LEAK_ME", "")
content = prompt if not leak else prompt + " connection=postgres://user:" + leak + "@db"
payload = json.dumps({
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": content}],
}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                            headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    msg = json.load(resp)["choices"][0]["message"]
print(msg.get("content", ""))
"#;

/// Write a secret-handling spec. When `leak` is true the agent is handed the
/// DB password (which it echoes into its model call); either way the flow
/// asserts the password must never surface.
fn write_secret_spec(dir: &Path, agent_py: &Path, leak: bool) -> PathBuf {
    let leak_env = if leak {
        "\n  env:\n    LEAK_ME: ${DB_PASSWORD}"
    } else {
        ""
    };
    let spec = dir.join("secret.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Handles the DB password\n\
             app: agent\n\
             agent:\n  command: python3 {agent}{leak_env}\n\
             control:\n  id: sec.portal.no-db-password-leak\n  title: The DB password never surfaces\n\
             steps:\n\
             \x20 - prompt: Connect to the reporting database\n\
             \x20 - assert: reply contains acknowledged\n\
             \x20 - assert_no_secret_leak: ${{DB_PASSWORD}}\n",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

/// (a) A run whose model request carries the resolved `${DB_PASSWORD}` FAILS
/// the record, names the variable and the step index, mints NO trace, and
/// NEVER prints the secret value.
#[test]
fn a_leaked_secret_fails_the_record_and_mints_no_trace() {
    let dir = work_dir("secret-leak");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, true);

    let output = std::process::Command::new(FLOWPROOF_BIN)
        .args(["record", spec.to_str().expect("utf8")])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("run flowproof record");

    assert!(
        !output.status.success(),
        "a leaked secret must fail the record"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Names the variable and the step index...
    assert!(
        stderr.contains("${DB_PASSWORD}"),
        "the message names the variable: {stderr}"
    );
    assert!(
        stderr.contains("step 3"),
        "the message names the step index: {stderr}"
    );
    // ...and NEVER the resolved value.
    assert!(
        !stderr.contains(SECRET),
        "the failure message must never contain the secret value: {stderr}"
    );
    // The store-guard: no trace reaches disk when a secret leaked into it.
    assert!(
        !dir.join("secret.trace.jsonl").exists(),
        "no trace for a run that leaked a secret"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// (b) A clean flow with the SAME assertion PASSES and mints a trace, and
/// (c) it replays green with zero network (no real model).
#[test]
fn a_clean_secret_flow_records_and_replays_deterministically() {
    let dir = work_dir("secret-clean");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, false);
    let trace = dir.join("secret.trace.jsonl");

    // RECORD: the secret is set (so the assertion resolves) but never enters
    // the corpus, so the scan finds nothing and the trace is minted.
    let record = std::process::Command::new(FLOWPROOF_BIN)
        .args(["record", spec.to_str().expect("utf8")])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("run flowproof record");
    assert!(
        record.status.success(),
        "a clean flow must record: {}",
        String::from_utf8_lossy(&record.stderr)
    );
    assert!(trace.exists(), "a clean flow mints its trace");
    // Belt and suspenders: the secret value is not in the minted trace.
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        !contents.contains(SECRET),
        "the secret value is never written to disk: {contents}"
    );

    // REPLAY with NO upstream at all - a stray real call would fail loudly.
    let replay = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("run flowproof run");
    assert!(
        replay.status.success(),
        "the clean flow must replay green with zero network: {}",
        String::from_utf8_lossy(&replay.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `flowproof audit` folds the control-bearing flow into a control-coverage
/// report: the control id, its pass verdict, and (for the secret-leak flow)
/// the secrets_checked / corpus / excluded fields - in both YAML and JSON,
/// naming the variable but never its value. Audit READS the run record, so the
/// flow is recorded AND run before auditing; audit never re-replays.
#[test]
fn audit_renders_the_control_map_in_yaml_and_json() {
    let dir = work_dir("secret-audit");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, false);

    // Record the clean flow.
    let record = std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "record",
            dir.join("secret.flow.yaml").to_str().expect("utf8"),
        ])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("record");
    assert!(record.status.success(), "record for audit");

    // Run it so a run record is written for audit to read - zero network.
    let run = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("run");
    assert!(
        run.status.success(),
        "the clean flow replays green: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // Audit as YAML (the default).
    let yaml = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("audit yaml");
    assert!(
        yaml.status.success(),
        "audit exits clean when the control holds: {}",
        String::from_utf8_lossy(&yaml.stderr)
    );
    let yaml_out = String::from_utf8_lossy(&yaml.stdout);
    assert!(
        yaml_out.contains("sec.portal.no-db-password-leak"),
        "audit names the control id: {yaml_out}"
    );
    assert!(
        yaml_out.contains("verdict: pass"),
        "the control passed: {yaml_out}"
    );
    assert!(
        yaml_out.contains("${DB_PASSWORD}"),
        "secrets_checked names the variable: {yaml_out}"
    );
    assert!(
        yaml_out.contains("secrets_checked"),
        "the corpus/exclusion fields are present: {yaml_out}"
    );
    // Never the value.
    assert!(
        !yaml_out.contains(SECRET),
        "the audit never prints the secret value: {yaml_out}"
    );

    // Audit as JSON.
    let json = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8"), "--json"])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("audit json");
    assert!(json.status.success(), "audit --json exits clean");
    let value: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("audit --json is valid JSON");
    let control = &value["controls"][0];
    assert_eq!(control["id"], "sec.portal.no-db-password-leak");
    assert_eq!(control["verdict"], "pass");
    assert_eq!(control["secrets_checked"][0], "${DB_PASSWORD}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A recorded agent flow must replay when the suite is run by DIRECTORY,
/// not only when the spec is named directly.
///
/// This is the gap that made agent flows unusable in CI. `run_suite` had no
/// `app: agent` branch, so it fell through to the step-replay loader, which
/// reads a trace one JSON object per line. An agent cassette is a single
/// `{app, mocks, cassette}` document, so the loader failed on line 1 and
/// every agent flow in the directory errored with "invalid trace line" -
/// traces `flowproof record` had just written, and that `flowproof run
/// <spec>` replayed green one at a time.
///
/// Directory mode is what a suite, a `pnpm test` script and a CI job all
/// invoke, so this asserts the two modes agree.
#[test]
fn a_recorded_agent_flow_replays_in_directory_mode() {
    let _env = lock_env();
    let dir = work_dir("suite-dispatch");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let spec = write_spec(&dir, &agent_py);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording an agent flow should succeed");

    // No model at all for the replay: a stray real call fails loudly.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");

    // The single-spec path, which already worked.
    let single = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(single, 0, "replaying the spec directly must pass");

    // The DIRECTORY path, which errored before this branch existed.
    let suite = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(
        suite, 0,
        "the same flow must replay when the suite is run by directory"
    );

    std::fs::remove_dir_all(&dir).ok();
}
