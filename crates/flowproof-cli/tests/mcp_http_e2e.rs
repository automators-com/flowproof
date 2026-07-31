//! End-to-end cover for the streamable-HTTP MCP boundary (#212).
//!
//! `docs/agent-testing.md` names this as the one remaining gap in its coverage
//! table: "the boundary is exercised over real HTTP, but driven directly rather
//! than through `flowproof record`/`run` with a real agent". `mcp_stdio_e2e.rs`
//! does the full CLI round trip for stdio; the HTTP transport had no equivalent.
//!
//! What only the round trip can prove: that the env var flowproof injects
//! (`FLOWPROOF_MCP_URL_<NAME>`) is what a real agent actually reaches, that the
//! lane is captured through `record`, and that `run` serves it back with the
//! real server gone from the machine entirely.
//!
//! The real server logs every request it receives, so the test has an
//! INDEPENDENT oracle rather than only flowproof's account of itself. That is
//! how #273 was provable, and it is what makes "the real server was never
//! asked" a checkable claim rather than an inference.
//!
//! No model credential: the upstream is a loopback fake, and replay makes zero
//! model calls by invariant 1.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// A real streamable-HTTP MCP server, logging every request it is asked for.
const REAL_HTTP_MCP_SERVER: &str = r#"
import json, sys
from http.server import BaseHTTPRequestHandler, HTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]

class H(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(n)
        msg = json.loads(raw or b"{}")
        with open(LOG, "a") as f:
            f.write(json.dumps(msg) + "\n")
        method = msg.get("method")
        if method == "initialize":
            result = {"protocolVersion": "2024-11-05",
                      "serverInfo": {"name": "weather", "version": "1"},
                      "capabilities": {"tools": {}}}
        elif method == "tools/list":
            result = {"tools": [{"name": "get_weather"}]}
        elif method == "tools/call":
            result = {"content": [{"type": "text",
                                   "text": "REAL:" + msg["params"]["name"]}],
                      "isError": False}
        else:
            result = {}
        out = json.dumps({"jsonrpc": "2.0", "id": msg.get("id"), "result": result}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *a):
        pass

HTTPServer(("127.0.0.1", PORT), H).serve_forever()
"#;

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/mcp-http-agent.py")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-mcp-http-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("probe binds");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p
}

fn wait_for_port(port: u16) -> bool {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
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

/// Kill the server on drop, so a failing assertion cannot leak a process.
struct Server(std::process::Child);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Full CLI round trip over the HTTP MCP transport: record against a real
/// server, then replay with that server not running at all.
#[test]
fn records_and_replays_the_http_mcp_lane() {
    let dir = work_dir("roundtrip");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    std::fs::copy(fixture(), &agent_py).expect("stage agent");
    std::fs::write(&server_py, REAL_HTTP_MCP_SERVER).expect("server");

    let server_port = free_port();
    let listener_port = free_port();

    let server = Server(
        std::process::Command::new("python3")
            .arg(&server_py)
            .arg(server_port.to_string())
            .arg(&log)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the real MCP server"),
    );
    assert!(
        wait_for_port(server_port),
        "the real MCP server must be listening before record"
    );

    let spec = dir.join("weather.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: MCP over streamable HTTP\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             mcp:\n  - name: weather\n    \
             url: http://127.0.0.1:{server_port}/mcp\n    port: {listener_port}\n\
             steps:\n\
             \x20 - prompt: What is the weather?\n\
             \x20 - assert: reply contains done\n",
            agent = agent_py.display(),
        ),
    )
    .expect("spec");

    let rec = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", fake_model())
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record the http MCP flow");
    assert!(
        rec.status.success(),
        "recording an http MCP flow must succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&rec.stdout),
        String::from_utf8_lossy(&rec.stderr)
    );

    // Independent oracle: the REAL server's own log proves the exchange
    // happened, so a lane that looks complete cannot be vacuously so.
    let logged = std::fs::read_to_string(&log).expect("server log readable");
    assert!(
        logged.contains("tools/call") && logged.contains("get_weather"),
        "the real server was reached through the listener at record: {logged}"
    );

    // The lane is captured, asserted on CONTENT rather than on a file existing.
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
            "the http MCP lane must capture {needle}: {contents}"
        );
    }

    // REPLAY with the real server stopped AND deleted: the lane must be served
    // from the trace, with no external process and no network.
    drop(server);
    std::fs::remove_file(&server_py).expect("remove the real server");
    std::fs::remove_file(&log).ok();

    let run = std::process::Command::new(FLOWPROOF_BIN)
        .arg("run")
        .arg(&spec)
        .current_dir(&dir)
        .output()
        .expect("replay the http MCP flow");
    assert!(
        run.status.success(),
        "replay must serve the lane with the real server gone; stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !log.exists(),
        "replay contacted no real server, so its log was never re-created"
    );

    std::fs::remove_dir_all(&dir).ok();
}
