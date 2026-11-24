//! `flowproof mcp` end to end: the real binary over stdio, driven the way a
//! coding agent's client drives it. The flow is gated off by
//! `skip_unless_env`, so a run returns a real report with no browser.

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
            .env_remove("MCP_E2E_FLAG")
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

    fn run(&mut self, id: u64, arguments: Value) -> Value {
        let params = json!({ "name": "flowproof_run", "arguments": arguments });
        self.request(id, "tools/call", params)["result"].clone()
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
    std::fs::create_dir_all(dir.join("flows")).expect("work dir");
    std::fs::write(
        dir.join("flows/gated.flow.yaml"),
        "name: Gated\napp: api\nskip_unless_env: [MCP_E2E_FLAG]\nsteps:\n  - assert_api:\n      \
         request: GET http://127.0.0.1:1/x\n      timeout_seconds: 1\n",
    )
    .expect("spec");
    dir.canonicalize().expect("canonical")
}

#[test]
fn handshake_lists_run_and_answers_protocol_errors() {
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
    let names: Vec<&str> = tools["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    assert_eq!(names, ["flowproof_run"]);

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

#[test]
fn run_uses_a_per_call_cwd_without_moving_the_server() {
    let dir = work_dir("cwd");
    let mut server = Server::start(&dir);

    let result = server.run(1, json!({ "spec": "gated.flow.yaml", "cwd": "flows" }));
    assert_eq!(result["isError"], false, "{result}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["passed"], true, "{structured}");
    assert_eq!(structured["exit_code"], 0);
    assert_eq!(
        structured["report"]["report"]["name"], "Gated",
        "{structured}"
    );
    assert!(
        Path::new(structured["cwd"].as_str().expect("cwd")).ends_with("flows"),
        "{structured}"
    );
    let text: Value = serde_json::from_str(result["content"][0]["text"].as_str().expect("text"))
        .expect("text content is the same JSON");
    assert_eq!(&text, structured);

    // The next call, with no cwd, still runs where the server started.
    let result = server.run(2, json!({ "spec": "flows/gated.flow.yaml" }));
    assert_eq!(result["structuredContent"]["passed"], true, "{result}");
    assert_eq!(
        Path::new(result["structuredContent"]["cwd"].as_str().expect("cwd")),
        dir
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn bad_input_and_engine_errors_are_tool_results() {
    let dir = work_dir("errors");
    let mut server = Server::start(&dir);

    // A spec that looks like a flag stays a path: it cannot smuggle in
    // `--allow-exec` or any other option.
    let result = server.run(1, json!({ "spec": "--allow-exec" }));
    assert_eq!(result["isError"], true, "{result}");
    let error = result["structuredContent"]["error"]
        .as_str()
        .expect("error");
    assert!(error.contains("--allow-exec"), "{error}");
    assert_eq!(result["structuredContent"]["exit_code"], 2);

    let unknown = json!({ "spec": "flows/gated.flow.yaml", "allow_everything": true });
    let missing = json!({ "spec": "flows/gated.flow.yaml", "cwd": "missing" });
    for (id, arguments) in [(2, json!({})), (3, unknown), (4, missing)] {
        assert_eq!(server.run(id, arguments)["isError"], true);
    }
    std::fs::remove_dir_all(&dir).ok();
}
