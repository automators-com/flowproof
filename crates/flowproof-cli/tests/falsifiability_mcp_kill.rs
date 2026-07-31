//! Falsifiability proof for the MCP stand-in's recording durability (#257).
//!
//! Before 0.8.0 the MCP lane was written only at stdin EOF. An agent that
//! terminated its stand-in abruptly never got there, and the ENTIRE recording
//! was lost. The fix persists the lane after every captured call, atomically.
//!
//! Nothing exercised it. Every MCP test in the suite closes stdin politely and
//! waits -- exactly the path the defect did not affect -- so all of them would
//! stay green if the fix were reverted tomorrow. That is the shape of an
//! unguarded fix: the bug is gone, and nothing would notice its return.
//!
//! The violating input is one hostile agent
//! (`tests/falsifiability/fixtures/mcp-killing-agent.py`), identical to the
//! suite's polite one but for its last act: `proc.kill()` instead of
//! `stdin.close(); wait()`.
//!
//! Two layers, and the second is the one that matters. A trace merely EXISTING
//! proves nothing -- the model cassette would still be written even with the
//! MCP lane empty, which is precisely what the defect produced. So the test
//! reads the lane and requires the captured call to be in it.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// The deterministic stdio MCP server, logging every request it receives.
const FAKE_MCP_SERVER: &str = r#"
import json, sys
log = open(sys.argv[1], "a")
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    msg = json.loads(line)
    log.write(line + "\n"); log.flush()
    mid = msg.get("id")
    if mid is None:
        continue
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05",
                  "serverInfo": {"name": "weather", "version": "1"},
                  "capabilities": {"tools": {}}}
    elif method == "tools/list":
        result = {"tools": [{"name": "get_weather"}]}
    elif method == "tools/call":
        result = {"content": [{"type": "text", "text": "REAL:" + msg["params"]["name"]}],
                  "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/mcp-killing-agent.py")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-mcpkill-{name}"));
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

/// Replies `done` to everything: enough to satisfy the model-boundary guard.
/// The MCP boundary is what is under test here, not the model one.
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

/// An agent that kills its stand-in must not take the recording with it.
#[test]
fn an_agent_that_kills_the_stand_in_still_leaves_a_recorded_lane() {
    let dir = work_dir("kill");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    std::fs::copy(fixture(), &agent_py).expect("stage the hostile agent");
    std::fs::write(&server_py, FAKE_MCP_SERVER).expect("server");

    let spec = dir.join("weather.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: MCP weather, agent kills the stand-in\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             mcp:\n  - name: weather\n    command: python3 {server} {log}\n\
             steps:\n\
             \x20 - prompt: What is the weather?\n\
             \x20 - assert: reply contains done\n",
            agent = agent_py.display(),
            server = server_py.display(),
            log = log.display(),
        ),
    )
    .expect("spec");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", fake_model())
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .env("FLOWPROOF_MCP_EXE", FLOWPROOF_BIN)
        .output()
        .expect("record with the hostile agent");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(
        out.status.success(),
        "an agent that kills its stand-in is rude, not wrong: the record must \
         still succeed. stdout={stdout} stderr={stderr}"
    );

    // The real server WAS asked -- otherwise the exchange never happened and
    // the lane would be legitimately empty, proving nothing.
    let logged = std::fs::read_to_string(&log).expect("server log readable");
    assert!(
        logged.contains("get_weather"),
        "the exchange must actually have happened, or an empty lane is \
         vacuously 'preserved': {logged}"
    );

    // Layer 2: the lane survived the kill. A trace merely existing proves
    // nothing -- the model cassette is written regardless, and an empty MCP
    // lane beside it is exactly what the pre-0.8.0 defect produced.
    let trace = dir.join("weather.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    for needle in [
        "\"mcp\"",
        "\"weather\"",
        "initialize",
        "tools/call",
        "get_weather",
    ] {
        assert!(
            contents.contains(needle),
            "the MCP lane must survive an abrupt kill, missing {needle}: {contents}"
        );
    }
    assert!(
        contents.contains("REAL:get_weather"),
        "and the captured RESULT survived too, not just the request: {contents}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
