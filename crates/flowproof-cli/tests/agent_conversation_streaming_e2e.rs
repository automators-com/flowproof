//! Multi-turn `conversation:` flows with a STREAMING agent, end to end
//! (#375).
//!
//! The second half of the coverage gap `plans/012-agent-multiturn-
//! conversations.md` left open. Streaming is the combination most worth a
//! real fixture rather than an argument from construction, because it is
//! the one where delivery gating could plausibly be wrong: a delivery
//! settles when the proxy has no in-flight request and stays quiet, and an
//! SSE response is in flight for longer than a buffered one. Releasing the
//! next delivery while the previous stream was still draining would be a
//! real bug.
//!
//! So this asserts on FRAME BOUNDARIES, not assembled text - the same
//! reason the single-turn streaming test does. A conversation whose streams
//! were collapsed into buffered bodies, or cut short by an early release,
//! would still assemble the same replies and still satisfy every `assert:
//! reply contains` in the spec. It would not produce complete frame lists,
//! one per model call, each terminated.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};

static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-conversation-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    use std::io::{BufRead, Read};
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

/// A fake chat-completions upstream playing the confirmation gate. Answered
/// BUFFERED on purpose: `stream` is transport, so the proxy strips it before
/// forwarding and synthesizes the stream back to the agent itself. That
/// synthesis, across a multi-delivery conversation, is what this covers.
fn fake_conversation_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(12) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            // Both spacings: the body may be re-serialized in flight, and
            // matching only one form leaves the model asking for the tool
            // for ever.
            let saw_tool_result =
                req.contains("\"role\":\"tool\"") || req.contains("\"role\": \"tool\"");
            let message = if saw_tool_result {
                serde_json::json!({
                    "role": "assistant", "content": "Order A-4471 is cancelled."
                })
            } else if req.contains("confirm") {
                serde_json::json!({
                    "role": "assistant",
                    "content": serde_json::Value::Null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "cancel_order",
                            "arguments": "{\"order\":\"A-4471\"}"
                        }
                    }]
                })
            } else {
                serde_json::json!({
                    "role": "assistant", "content": "Are you sure? This cannot be undone."
                })
            };
            let finish = if message.get("tool_calls").is_some() {
                "tool_calls"
            } else {
                "stop"
            };
            let body = serde_json::json!({
                "id": "chatcmpl-fake",
                "object": "chat.completion",
                "model": "gpt-4o",
                "choices": [{"index": 0, "message": message, "finish_reason": finish}],
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

/// A real multi-turn STREAMING agent: delivery 0 from `FLOWPROOF_PROMPT`,
/// later deliveries as JSON lines on stdin, history kept across them, and
/// every model call made with `stream: true`. It appends one JSON line per
/// RUN to `__LOG__`, holding the ordered frames of every call that run made,
/// so the record leg and the replay leg can be compared call for call.
const STREAMING_CONVERSATION_AGENT: &str = r#"
import json, os, sys, time, urllib.request

base = os.environ["OPENAI_BASE_URL"]
messages = []
log = []

def settle():
    for _ in range(5):
        payload = json.dumps({
            "model": "gpt-4o",
            "stream": True,
            "messages": messages,
            "tools": [{"type": "function", "function": {"name": "cancel_order"}}],
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
                # Tolerate a buffered answer rather than failing on it: the
                # trajectory still assembles, so the ONLY evidence the stream
                # was collapsed is this frame log.
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
        if not calls:
            messages.append({"role": "assistant", "content": content})
            print(content, flush=True)
            return
        messages.append({"role": "assistant", "content": None, "tool_calls": calls})
        for call in calls:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"cancelled_at": time.time_ns(), "status": "gone"})
            messages.append({"role": "tool", "tool_call_id": call["id"], "content": real})

messages.append({"role": "user", "content": os.environ["FLOWPROOF_PROMPT"]})
settle()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    messages.append({"role": "user", "content": json.loads(line)["prompt"]})
    settle()

with open("__LOG__", "a") as fh:
    fh.write(json.dumps(log) + "\n")
"#;

/// The frames a well-formed synthetic stream delivers for this conversation:
/// one complete, terminated stream per model call - delivery 0's single text
/// turn, then delivery 1's tool call and the text that closes it.
fn expected_frames() -> serde_json::Value {
    serde_json::json!([
        [
            "content-type:text/event-stream",
            "role:assistant",
            "content:Are you sure? This cannot be undone.",
            "finish:stop",
            "DONE",
        ],
        [
            "content-type:text/event-stream",
            "role:assistant",
            r#"tool:cancel_order:{"order":"A-4471"}"#,
            "finish:tool_calls",
            "DONE",
        ],
        [
            "content-type:text/event-stream",
            "role:assistant",
            "content:Order A-4471 is cancelled.",
            "finish:stop",
            "DONE",
        ],
    ])
}

fn frames(log: &Path, line: usize) -> serde_json::Value {
    let contents = std::fs::read_to_string(log).expect("the agent wrote its frame log");
    let line = contents
        .lines()
        .nth(line)
        .unwrap_or_else(|| panic!("the frame log has no line {line}: {contents}"));
    serde_json::from_str(line).expect("each line is a JSON list of frames")
}

fn write_conversation_spec(dir: &Path, agent_py: &Path) -> PathBuf {
    let spec = dir.join("cancel-streaming.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Cancel with confirmation (streaming)\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             tools:\n  - name: cancel_order\n    result: {{ order: A-4471, status: cancelled }}\n\
             steps:\n\
             \x20 - conversation:\n\
             \x20     - user: Please cancel my order A-4471.\n\
             \x20       assert_no_tool_call: cancel_order\n\
             \x20       assert: reply contains sure\n\
             \x20     - user: Yes, I confirm - go ahead and cancel it.\n\
             \x20       assert_tool_call: cancel_order where order equals A-4471\n\
             \x20       assert: reply contains cancelled\n",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

#[test]
fn records_and_replays_a_streaming_conversation() {
    let _env = lock_env();
    let dir = work_dir("streaming");
    let agent_py = dir.join("agent.py");
    let log = dir.join("frames.jsonl");
    std::fs::write(
        &agent_py,
        STREAMING_CONVERSATION_AGENT.replace("__LOG__", log.to_str().expect("utf8")),
    )
    .expect("agent");
    let spec = write_conversation_spec(&dir, &agent_py);

    // RECORD.
    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_conversation_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording a streaming conversation should succeed");

    let recorded = frames(&log, 0);
    assert_eq!(
        recorded,
        expected_frames(),
        "every delivery must be served a complete stream at record"
    );

    let trace = dir.join("cancel-streaming.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    let doc: serde_json::Value = serde_json::from_str(&contents).expect("trace is JSON");

    // Transport stays out of the recording, in the conversation path too.
    assert!(
        !contents.contains("event-stream") && !contents.contains("chat.completion.chunk"),
        "chunk boundaries are synthesized, never recorded: {contents}"
    );
    assert!(
        !contents.contains("\"stream\""),
        "`stream` is transport and must not enter the comparison: {contents}"
    );

    // Delivery grouping survived streaming: the second delivery owns both
    // the tool call and the text that closed it.
    let deliveries = doc["cassette"]["deliveries"]
        .as_array()
        .expect("conversation metadata is stamped");
    assert_eq!(deliveries.len(), 2, "{contents}");
    assert_eq!(deliveries[0]["turn_count"], 1, "{contents}");
    assert_eq!(
        deliveries[1]["turn_count"], 2,
        "delivery 1 must own BOTH of its turns: {contents}"
    );

    // REPLAY with no model at all - a stray real call would fail loudly.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(
        code, 0,
        "the whole conversation must replay with zero upstream model calls"
    );

    // The claim worth having: replay serves a STREAM for every delivery,
    // with the same boundaries, from a cassette holding no stream at all.
    // Each list ending in DONE is also the evidence that no delivery was
    // released while the previous one's stream was still draining.
    let replayed = frames(&log, 1);
    assert_eq!(
        replayed,
        expected_frames(),
        "replay must serve a stream per delivery, not a buffered response"
    );
    assert_eq!(
        replayed, recorded,
        "record and replay must agree chunk for chunk across the conversation"
    );

    std::fs::remove_dir_all(&dir).ok();
}
