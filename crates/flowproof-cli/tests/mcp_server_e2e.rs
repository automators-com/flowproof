//! `flowproof mcp` end to end: the real binary over stdio, driven the way a
//! coding agent's client drives it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

struct Server {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn start(cwd: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flowproof"))
            .arg("mcp")
            .current_dir(cwd)
            .env_remove("FLOWPROOF_MCP_EXE")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn flowproof mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
        }
    }

    fn send(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write");
        self.stdin.flush().expect("flush");
    }

    fn next(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read");
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON ({e}): {line:?}"))
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.send(&message.to_string());
        let reply = self.next();
        assert_eq!(reply["id"], id, "{reply}");
        reply
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-mcp-server-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

#[test]
fn handshake_and_protocol_errors() {
    let dir = work_dir("handshake");
    let mut server = Server::start(&dir);

    let init = server.request(1, "initialize", json!({ "protocolVersion": "2025-06-18" }));
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "flowproof");
    assert!(
        init["result"]["capabilities"]["tools"].is_object(),
        "{init}"
    );
    let unknown = server.request(2, "initialize", json!({ "protocolVersion": "1999-01-01" }));
    // The one advertised revision. Bumping it is its own PR (plan 17).
    assert_eq!(
        unknown["result"]["protocolVersion"], "2025-11-25",
        "{unknown}"
    );

    server.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    let tools = server.request(3, "tools/list", json!({}));
    assert_eq!(tools["result"]["tools"], json!([]), "{tools}");

    server.send("not json");
    assert_eq!(server.next()["error"]["code"], -32700);
    assert_eq!(
        server.request(4, "nope", json!({}))["error"]["code"],
        -32601
    );
    let params = json!({ "name": "flowproof_nope", "arguments": {} });
    assert_eq!(
        server.request(5, "tools/call", params)["error"]["code"],
        -32602
    );
    std::fs::remove_dir_all(&dir).ok();
}
