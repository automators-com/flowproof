//! Multi-turn `conversation:` flows in the ANTHROPIC dialect, end to end
//! (#375).
//!
//! `plans/012-agent-multiturn-conversations.md` shipped delivery gating
//! with OpenAI-shaped fixtures only, and recorded the dialect coverage as a
//! known gap: the gating code never inspects `Turn.protocol`, so there was
//! "no known reason Anthropic would behave differently - but that is
//! confidence, not a fixture." This is the fixture.
//!
//! What it actually pins, beyond "it works": delivery 1 here produces TWO
//! turns (a `tool_use` and the text that follows its result) while delivery
//! 0 produces one. A grouping bug that stamped one turn per delivery, or
//! that closed a delivery at the first response rather than at settle,
//! would still assemble the same final text and still satisfy the reply
//! assertions - so the assertions below are on the per-delivery TURN
//! COUNTS, which only correct grouping can produce.

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

/// A fake Messages upstream that plays the confirmation gate. It keys off
/// the accumulated request, which is what makes the two deliveries produce
/// DIFFERENT turn counts:
///
/// * delivery 0 ("please cancel") -> one text turn asking for confirmation;
/// * delivery 1 ("yes, confirm")  -> a `tool_use` turn, then, once the
///   agent has fed the tool result back, a closing text turn.
fn fake_anthropic_conversation_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            let (content, stop_reason) = if req.contains("tool_result") {
                (
                    serde_json::json!([{"type": "text", "text": "Order A-4471 is cancelled."}]),
                    "end_turn",
                )
            } else if req.contains("confirm") {
                (
                    serde_json::json!([{"type": "tool_use", "id": "toolu_1",
                        "name": "cancel_order", "input": {"order": "A-4471"}}]),
                    "tool_use",
                )
            } else {
                (
                    serde_json::json!([{"type": "text",
                        "text": "Are you sure? This cannot be undone."}]),
                    "end_turn",
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

/// A real multi-turn Anthropic-dialect agent: delivery 0 from
/// `FLOWPROOF_PROMPT`, every later delivery as one JSON line on stdin, with
/// the message history kept across deliveries - the `command:` driver's
/// documented contract, in the Messages wire shape.
const ANTHROPIC_CONVERSATION_AGENT: &str = r#"
import json, os, sys, time, urllib.request

base = os.environ["ANTHROPIC_BASE_URL"]
messages = []

def settle():
    for _ in range(5):
        payload = json.dumps({
            "model": "claude-sonnet-4-5",
            "max_tokens": 1024,
            "messages": messages,
            "tools": [{"name": "cancel_order", "input_schema": {"type": "object"}}],
        }).encode()
        req = urllib.request.Request(base + "/v1/messages", data=payload,
                                    headers={"content-type": "application/json",
                                             "anthropic-version": "2023-06-01"})
        with urllib.request.urlopen(req) as resp:
            blocks = json.load(resp)["content"]
        uses = [b for b in blocks if b.get("type") == "tool_use"]
        if not uses:
            messages.append({"role": "assistant", "content": blocks})
            print("".join(b.get("text", "") for b in blocks if b.get("type") == "text"),
                  flush=True)
            return
        messages.append({"role": "assistant", "content": blocks})
        results = []
        for use in uses:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"cancelled_at": time.time_ns(), "status": "gone"})
            results.append({"type": "tool_result", "tool_use_id": use["id"],
                            "content": real})
        messages.append({"role": "user", "content": results})

messages.append({"role": "user", "content": os.environ["FLOWPROOF_PROMPT"]})
settle()
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    messages.append({"role": "user", "content": json.loads(line)["prompt"]})
    settle()
"#;

fn write_conversation_spec(dir: &Path, agent_py: &Path) -> PathBuf {
    let spec = dir.join("cancel-anthropic.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Cancel with confirmation (Anthropic)\n\
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
fn records_and_replays_an_anthropic_conversation() {
    let _env = lock_env();
    let dir = work_dir("anthropic");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, ANTHROPIC_CONVERSATION_AGENT).expect("agent");
    let spec = write_conversation_spec(&dir, &agent_py);

    // RECORD against the fake Messages upstream.
    std::env::set_var(
        "FLOWPROOF_AGENT_UPSTREAM",
        fake_anthropic_conversation_model(),
    );
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(
        code, 0,
        "recording an Anthropic conversation should succeed"
    );

    let trace = dir.join("cancel-anthropic.trace.jsonl");
    assert!(trace.exists(), "a cassette trace must be written");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    let doc: serde_json::Value = serde_json::from_str(&contents).expect("trace is JSON");

    // The dialect survived the conversation path: this is the claim plan 012
    // left as confidence rather than coverage.
    let turns = doc["cassette"]["turns"].as_array().expect("turns");
    assert!(
        turns.iter().all(|t| t["protocol"] == "anthropic"),
        "every recorded turn names the Anthropic dialect: {contents}"
    );

    // The grouping, which is the part a dialect change could plausibly
    // break: delivery 0 is one turn, delivery 1 is the tool_use plus the
    // text that follows its result.
    let deliveries = doc["cassette"]["deliveries"]
        .as_array()
        .expect("conversation metadata is stamped");
    assert_eq!(deliveries.len(), 2, "two deliveries recorded: {contents}");
    assert_eq!(deliveries[0]["turn_count"], 1, "{contents}");
    assert_eq!(
        deliveries[1]["turn_count"], 2,
        "delivery 1 must own BOTH of its turns: {contents}"
    );
    assert_eq!(
        deliveries[0]["user"], "Please cancel my order A-4471.",
        "{contents}"
    );

    // And the turns carry the matching index, densely and in order.
    let indexes: Vec<u64> = turns
        .iter()
        .map(|t| t["delivery_index"].as_u64().unwrap_or(0))
        .collect();
    assert_eq!(indexes, vec![0, 1, 1], "turn->delivery mapping: {contents}");

    // The mock is what the trajectory pins; the volatile real result is not
    // on disk, in this dialect either.
    assert!(
        !contents.contains("cancelled_at"),
        "the volatile real tool result must not be in the trace: {contents}"
    );

    // REPLAY with no model at all - unset every upstream handle so a stray
    // real call would fail loudly rather than sneak through.
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    std::env::remove_var("ANTHROPIC_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(
        code, 0,
        "the whole conversation must replay with zero upstream model calls"
    );

    std::fs::remove_dir_all(&dir).ok();
}
