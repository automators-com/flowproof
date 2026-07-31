//! Falsifiability proof for `assert: reply contains` (issue #250).
//!
//! Every end-to-end use of this assertion in the suite asserts text the model
//! was always going to produce (`agent_flow_e2e.rs:150`, `:304`;
//! `mcp_stdio_e2e.rs:363`). Those flows pass whether the assertion works or
//! not, so nothing anywhere proves it can FAIL.
//!
//! The 0.9.0 record makes the stakes concrete rather than theoretical. A
//! streaming client handed one buffered body assembles the identical final
//! text, so `assert: reply contains` stayed green for exactly the defect it
//! existed to catch. That is the class of bug an unexercised failing direction
//! hides, and this assertion has already hosted one.
//!
//! The violating input is the model's own final answer, committed as
//! `tests/falsifiability/fixtures/reply-missing-text.json` so a reviewer can
//! see what makes it guilty without reading this harness: the flow asserts
//! "sunny", the reply says it is raining.
//!
//! Two layers:
//!   1. the VERDICT -- record refuses, and mints NO trace;
//!   2. the process EXIT CODE the caller branches on.
//!
//! The no-trace check carries real weight here. A record that failed loudly
//! while still writing a trace would leave a cassette whose reply assertion
//! never held, and replay would serve it green from then on.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// A minimal agent: one round trip, print what the model said. No tools, so
/// nothing but the reply is under test.
const PLAIN_AGENT: &str = r#"
import json, os, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
payload = json.dumps({
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": prompt}],
}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                            headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    msg = json.load(resp)["choices"][0]["message"]
print(msg.get("content", ""))
"#;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/reply-missing-text.json")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-reply-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// Read an HTTP/1.1 request to the end of its declared body: a body can arrive
/// across segments, and closing with bytes unread makes the stack RST the peer.
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

/// Serve the committed guilty reply, verbatim, to every request.
fn model_serving(reply_body: String) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_http_request(&mut stream);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{reply_body}",
                    reply_body.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// A reply that does not contain the asserted text must fail the record.
#[test]
fn a_reply_missing_the_asserted_text_fails_the_record_and_mints_no_trace() {
    let dir = work_dir("missing");
    let agent = dir.join("agent.py");
    std::fs::write(&agent, PLAIN_AGENT).expect("agent");

    // The fixture carries a `_comment` block for the reader; strip it so the
    // wire body is a clean chat-completions response.
    let raw = std::fs::read_to_string(fixture()).expect("read fixture");
    let mut value: serde_json::Value = serde_json::from_str(&raw).expect("fixture is JSON");
    value
        .as_object_mut()
        .expect("fixture is an object")
        .remove("_comment");
    let reply_body = value.to_string();
    assert!(
        !reply_body.contains("sunny"),
        "the fixture must not contain the asserted text, or it is not guilty: {reply_body}"
    );

    let spec = dir.join("reply.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: The assistant reports sunny weather\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert: reply contains sunny\n",
            agent = agent.display()
        ),
    )
    .expect("spec");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", model_serving(reply_body))
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record the guilty trajectory");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Layer 2: the exit code a caller branches on.
    assert!(
        !out.status.success(),
        "a reply that does not contain the asserted text must fail the record; \
         if this passes, `assert: reply contains` cannot fail -- the assertion \
         that already hosted one false green in 0.9.0. stdout={stdout} stderr={stderr}"
    );

    // Layer 1: no trace on disk. A refused record that still minted one would
    // leave a cassette whose reply assertion never held, replaying green.
    let trace = dir.join("reply.trace.jsonl");
    assert!(
        !trace.exists(),
        "a refused record mints no trace: {}",
        trace.display()
    );

    std::fs::remove_dir_all(&dir).ok();
}
