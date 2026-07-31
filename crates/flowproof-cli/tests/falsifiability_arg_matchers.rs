//! Falsifiability proof for `assert_tool_call ... where <path> <matcher>`
//! (issue #247).
//!
//! `a_failing_assertion_refuses_the_trace` (`agent_flow_e2e.rs:778`) already
//! proves the TOOL-NAME layer can fail: the flow demands a tool the agent never
//! calls, and record refuses the trace. Nothing proved the ARGUMENT layer can.
//!
//! That is the layer carrying the most weight. `docs/agent-testing.md` says
//! argument assertions are "usually where the bugs are", and names chained
//! arguments -- threading one tool's result into the next call -- as the
//! behaviour multi-step agents actually get wrong. Every existing use of a
//! `where` clause asserts an argument the model was always going to produce, so
//! all of them pass whether the matcher works or not.
//!
//! The violating input is one guilty call, committed as
//! `tests/falsifiability/fixtures/tool-call-wrong-argument.json`: the right
//! tool, the wrong city. It violates two assertions at once --
//! `where city equals Nairobi` (a value matcher, the value is Mombasa) and
//! `where city is absent` (a presence matcher, the key is there) -- so one
//! fixture covers both halves of the vocabulary.
//!
//! Two layers throughout: the record refuses, and no trace reaches disk.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// An obedient agent: calls whatever the model asks, feeds the result back.
const TOOL_AGENT: &str = r#"
import json, os, urllib.request

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
            messages.append({"role": "tool", "tool_call_id": call["id"],
                             "content": json.dumps({"sky": "clear"})})
        continue
    print(msg.get("content", ""))
    break
"#;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/tool-call-wrong-argument.json")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-args-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    while let Ok(n) = stream.read(&mut chunk) {
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        let text = String::from_utf8_lossy(&buf).to_string();
        let Some(head_end) = text.find("\r\n\r\n") else {
            continue;
        };
        let len = text
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                k.eq_ignore_ascii_case("content-length")
                    .then(|| v.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        if buf.len() >= head_end + 4 + len {
            break;
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Serve the committed guilty tool call, then a plain finish once the agent
/// reports the tool result back.
fn model_with_the_guilty_call() -> String {
    let raw = std::fs::read_to_string(fixture()).expect("read fixture");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("fixture is JSON");
    value
        .as_object_mut()
        .expect("fixture is an object")
        .remove("_comment");
    let tool_call = value.to_string();
    let finish = serde_json::json!({
        "choices": [{"index": 0, "finish_reason": "stop",
            "message": {"role": "assistant", "content": "Reported."}}]
    })
    .to_string();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            let body = if req.contains("\"role\":\"tool\"") || req.contains("\"role\": \"tool\"") {
                finish.clone()
            } else {
                tool_call.clone()
            };
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

/// Record the guilty trajectory against `assertion`, and return (success, out).
fn record_with(assertion: &str, tag: &str) -> (bool, String) {
    let dir = work_dir(tag);
    let agent = dir.join("agent.py");
    std::fs::write(&agent, TOOL_AGENT).expect("agent");
    let spec = dir.join("args.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Weather for Nairobi\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             tools:\n  - name: get_weather\n    result: {{ sky: clear }}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert_tool_call: {assertion}\n",
            agent = agent.display()
        ),
    )
    .expect("spec");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", model_with_the_guilty_call())
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record");

    let text = format!(
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let trace_written = dir.join("args.trace.jsonl").exists();
    assert!(
        !trace_written || out.status.success(),
        "a refused record must mint no trace: {text}"
    );
    std::fs::remove_dir_all(&dir).ok();
    (out.status.success(), text)
}

/// A value matcher must fail when the recorded argument differs.
#[test]
fn a_wrong_argument_value_fails_the_record() {
    let (ok, text) = record_with("get_weather where city equals Nairobi", "equals");
    assert!(
        !ok,
        "the model called get_weather with city=Mombasa; `where city equals \
         Nairobi` must fail, or the matcher vocabulary proves nothing about the \
         layer the docs call 'usually where the bugs are'. {text}"
    );
}

/// A presence matcher must fail when the argument is present after all.
#[test]
fn an_argument_asserted_absent_but_present_fails_the_record() {
    let (ok, text) = record_with("get_weather where city is absent", "absent");
    assert!(
        !ok,
        "the call carries a `city` argument; `where city is absent` must fail. \
         A presence matcher that cannot fail would let a flow assert the \
         absence of anything at all. {text}"
    );
}
