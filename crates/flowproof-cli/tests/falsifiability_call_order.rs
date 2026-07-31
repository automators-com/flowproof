//! Falsifiability proof for cassette call-order tolerance (#258).
//!
//! 0.8.0 dropped positional matching: "Order between concurrent calls is
//! therefore not asserted -- the agent does not guarantee it, so a recording
//! cannot either." goose issues its task call and a session-title call
//! concurrently without waiting, so a positional matcher reported a divergence
//! when nothing about the agent had changed.
//!
//! Nothing in the suite exercised either half of that change.
//!
//! Both halves are needed, and the second is the one that keeps the first
//! honest:
//!
//!   1. TOLERANCE -- two independent calls recorded in one order replay in the
//!      other, and the run still passes.
//!   2. DISCRIMINATION -- an agent that changes WHAT it sends still diverges.
//!
//! Without (2), "order-tolerant matching" is indistinguishable from "the
//! request is not checked at all", and the tolerance itself becomes the false
//! green. A matcher that accepted anything would satisfy (1) perfectly.
//!
//! The knobs live in files beside the agent rather than the environment, the
//! convention `mcp_stdio_e2e.rs` uses, so record and replay differ without a
//! race.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/two-call-agent.py")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-order-{name}"));
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

/// Replies `done` to everything. The calls differ in what they SEND, which is
/// what the matcher keys on; the answers need not differ.
fn fake_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_http_request(&mut stream);
            let reply = serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "done"}}]
            })
            .to_string();
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                     content-length: {}\r\nconnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .as_bytes(),
            );
            let _ = stream.flush();
            let _ = stream.shutdown(std::net::Shutdown::Write);
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// Stage the agent and a spec, and record it in call order "ab".
fn record_baseline(dir: &Path) -> PathBuf {
    let agent = dir.join("two-call-agent.py");
    std::fs::copy(fixture(), &agent).expect("stage agent");
    std::fs::write(dir.join("order.txt"), "ab").expect("order");

    let spec = dir.join("order.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Two independent calls\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             steps:\n\
             \x20 - prompt: Handle the incident.\n\
             \x20 - assert: reply contains done\n",
            agent = agent.display()
        ),
    )
    .expect("spec");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", fake_model())
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record");
    assert!(
        out.status.success(),
        "the baseline recording must be honest and green; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        dir.join("order.trace.jsonl").exists(),
        "a green record mints a trace, or there is nothing to replay against"
    );
    spec
}

/// Replay with no model reachable at all: zero LLM calls, per invariant 1.
fn replay(dir: &Path, spec: &Path) -> (bool, String) {
    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("run")
        .arg(spec)
        .current_dir(dir)
        .output()
        .expect("replay");
    (
        out.status.success(),
        format!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
    )
}

/// TOLERANCE: two independent calls, recorded a-then-b, replayed b-then-a.
#[test]
fn reordered_independent_calls_still_replay() {
    let dir = work_dir("tolerant");
    let spec = record_baseline(&dir);

    // The only change: the agent issues the same two calls the other way round.
    std::fs::write(dir.join("order.txt"), "ba").expect("flip order");

    let (ok, text) = replay(&dir, &spec);
    assert!(
        ok,
        "reordering two INDEPENDENT calls must not be a divergence -- the agent \
         never guaranteed their order, so the recording cannot assert it. This \
         is the goose case that forced the 0.8.0 change. {text}"
    );
}

/// DISCRIMINATION: order is tolerated, content is not. Without this, the
/// tolerance above would be satisfied by a matcher that checks nothing.
#[test]
fn a_changed_request_body_still_diverges() {
    let dir = work_dir("strict");
    let spec = record_baseline(&dir);

    // Same order as recorded; only the second call's CONTENT differs.
    std::fs::write(dir.join("mutate.txt"), "1").expect("mutate");

    let (ok, text) = replay(&dir, &spec);
    assert!(
        !ok,
        "order tolerance must not become 'the request is never checked'. The \
         agent sent different text; that is a real divergence and replay must \
         say so, or every cassette in the product proves nothing about what was \
         sent. {text}"
    );
}
