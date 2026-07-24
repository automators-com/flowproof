//! End to end for `app: agent`, with no real model and no agent framework:
//! a fake model (a local HTTP server returning a scripted trajectory) and a
//! fake agent (a small Python process that speaks chat-completions). The
//! full spec -> record -> cassette -> replay path runs, exactly as CI
//! proves it on every push.
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
