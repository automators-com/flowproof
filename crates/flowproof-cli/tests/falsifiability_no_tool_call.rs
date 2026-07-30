//! Falsifiability proof for `assert_no_tool_call` (issue #248).
//!
//! This is the guard-path assertion the security story leans on: "the model
//! asked, and the code refused". `docs/agent-testing.md` calls it "arguably the
//! highest-value" assertion in the feature, and its coverage table admits the
//! gap this closes -- "the failing direction is unit-tested; the end-to-end case
//! only exercises the passing direction".
//!
//! That gap matters more than it sounds. Every end-to-end use of
//! `assert_no_tool_call` in the suite (`agent_flow_e2e.rs:149`, `:303`) sits in
//! a flow where the forbidden tool was never going to be called. Such a flow
//! passes whether the assertion works or not. If the assertion were replaced by
//! an unconditional PASS today, nothing end-to-end would notice -- and every
//! guard flow an adopter writes would be worthless while looking green.
//!
//! So the violating input has two halves, and both are deliberate:
//!   - a model that ASKS for the forbidden tool, and
//!   - an agent with no guard of its own, which obediently calls it
//!     (`tests/falsifiability/fixtures/guard-agent.py`).
//!
//! Two layers, as everywhere in this suite:
//!   1. the VERDICT -- record refuses, and mints NO trace;
//!   2. the process EXIT CODE the caller branches on.
//!
//! The trace check is not decoration. A record that failed loudly but left a
//! trace on disk would leave a cassette asserting a guard that never held, and
//! that cassette would replay green forever.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/falsifiability/fixtures/guard-agent.py")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-guard-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// Read an HTTP/1.1 request to the end of its declared body. A body can arrive
/// across segments, and closing with bytes still unread makes the stack RST the
/// client.
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

/// A model that ASKS for the forbidden tool. This is the other half of the
/// violating input: without a model that tries, a guard flow proves only that
/// the model behaved on the day it was recorded.
fn model_that_asks_for_the_forbidden_tool() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            let reply = if req.contains("\"role\":\"tool\"") || req.contains("\"role\": \"tool\"") {
                serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "stop",
                        "message": {"role": "assistant",
                            "content": "The alert has been sent."}}]
                })
            } else {
                serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "tool_calls",
                        "message": {"role": "assistant", "content": null,
                            "tool_calls": [{"id": "call_1", "type": "function",
                                "function": {"name": "send_alert",
                                    "arguments": "{\"severity\":\"high\"}"}}]}}]
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

/// A guard flow whose forbidden tool IS called must refuse the trace.
#[test]
fn a_forbidden_tool_that_is_called_fails_the_record_and_mints_no_trace() {
    let dir = work_dir("called");
    let agent = dir.join("guard-agent.py");
    std::fs::copy(fixture(), &agent).expect("stage the obedient agent");

    let spec = dir.join("guard.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: The agent must not send an alert\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             tools:\n  - name: send_alert\n    result: {{ delivered: true }}\n\
             steps:\n\
             \x20 - prompt: Summarise the incident.\n\
             \x20 - assert_no_tool_call: send_alert\n",
            agent = agent.display()
        ),
    )
    .expect("spec");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env(
            "FLOWPROOF_AGENT_UPSTREAM",
            model_that_asks_for_the_forbidden_tool(),
        )
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record the guilty trajectory");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    // Layer 2: the exit code a caller branches on.
    assert!(
        !out.status.success(),
        "a guard flow whose forbidden tool WAS called must fail; \
         if this passes, `assert_no_tool_call` cannot fail and every guard \
         flow written against it is worthless. stdout={stdout} stderr={stderr}"
    );

    // The failure must name the tool, or a reader cannot tell which guard broke.
    assert!(
        stdout.contains("send_alert") || stderr.contains("send_alert"),
        "the failure names the forbidden tool: stdout={stdout} stderr={stderr}"
    );

    // Layer 1: no trace on disk. A record that failed loudly but still minted a
    // trace would leave a cassette asserting a guard that never held, and it
    // would replay green forever.
    let trace = dir.join("guard.trace.jsonl");
    assert!(
        !trace.exists(),
        "a refused record mints no trace, or the guard is enshrined as a \
         passing cassette: {}",
        trace.display()
    );

    std::fs::remove_dir_all(&dir).ok();
}
