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
        Self::start_with(cwd, &[])
    }

    fn start_with(cwd: &Path, env: &[(&str, &str)]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_flowproof"))
            .arg("mcp")
            .env_remove("FLOWPROOF_ALLOW_EXEC")
            .envs(env.iter().copied())
            .current_dir(cwd)
            .env_remove("MCP_E2E_FLAG")
            .env_remove("FLOWPROOF_MCP_EXE")
            // No authoring model, whatever this machine has configured.
            .env("HOME", cwd)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("ANTHROPIC_API_KEY")
            .env_remove("OPENAI_API_KEY")
            .env_remove("FLOWPROOF_AI_API_KEY")
            .env_remove("FLOWPROOF_AI_PROVIDER")
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
        self.call(id, "flowproof_run", arguments)
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> Value {
        let params = json!({ "name": tool, "arguments": arguments });
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
    assert_eq!(
        names,
        [
            "flowproof_run",
            "flowproof_record",
            "flowproof_heal",
            "flowproof_doctor",
            "flowproof_audit"
        ]
    );

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

#[test]
fn a_step_that_needs_clarifying_is_a_result_not_an_error() {
    let dir = work_dir("clarify");
    std::fs::write(
        dir.join("flows/vague.flow.yaml"),
        "name: Vague\napp: api\nsteps:\n  - make required field changes\n",
    )
    .expect("spec");
    let mut server = Server::start(&dir);

    let result = server.call(
        1,
        "flowproof_record",
        json!({ "spec": "flows/vague.flow.yaml" }),
    );
    assert_eq!(result["isError"], false, "{result}");
    let structured = &result["structuredContent"];
    assert_eq!(structured["recorded"], false, "{structured}");
    let clarification = &structured["result"]["needs_clarification"];
    assert_eq!(
        clarification["step"], "make required field changes",
        "{structured}"
    );
    assert_eq!(clarification["stage"], "no_model", "{structured}");
    assert!(!dir.join("flows/vague.trace.jsonl").exists());

    // A gated flow records as skipped, and an unknown author is the
    // engine's own refusal, passed back as an error.
    let gated = json!({ "spec": "flows/gated.flow.yaml", "no_repair": true });
    let result = server.call(2, "flowproof_record", gated);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["recorded"], false, "{result}");
    assert!(result["structuredContent"]["result"]["skipped"].is_string());
    let bad = json!({ "spec": "flows/gated.flow.yaml", "author": "--allow-exec" });
    assert_eq!(server.call(3, "flowproof_record", bad)["isError"], true);
    std::fs::remove_dir_all(&dir).ok();
}

/// Plan 17, decision 4: the server never consents on anyone's behalf. Suite
/// shell is refused unless the call itself passes `allow_exec`, even when
/// the client's environment carries `FLOWPROOF_ALLOW_EXEC=1`.
#[test]
fn suite_shell_runs_only_when_the_call_consents() {
    let dir = work_dir("consent");
    std::fs::write(
        dir.join("flows/suite.yaml"),
        "env_from: |\n  touch hook-ran\n  echo MCP_E2E_MARK=1\n",
    )
    .expect("suite");
    let mut server = Server::start_with(&dir, &[("FLOWPROOF_ALLOW_EXEC", "1")]);
    let ran = dir.join("flows/hook-ran");

    for (id, tool) in [(1, "flowproof_record"), (2, "flowproof_run")] {
        let result = server.call(id, tool, json!({ "spec": "flows/gated.flow.yaml" }));
        assert_eq!(result["isError"], true, "{tool}: {result}");
        let error = result["structuredContent"]["error"]
            .as_str()
            .expect("error");
        assert!(
            error.contains("touch hook-ran"),
            "names the command: {error}"
        );
        assert!(error.contains("suite.yaml"), "names its file: {error}");
        assert!(
            result["structuredContent"]["consent"].is_string(),
            "{result}"
        );
        assert!(!ran.exists(), "{tool} ran suite shell without consent");
    }

    let consented = json!({ "spec": "flows/gated.flow.yaml", "allow_exec": true });
    let result = server.call(3, "flowproof_record", consented);
    assert_eq!(result["isError"], false, "{result}");
    assert!(ran.exists(), "consented call runs the hook");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn doctor_audit_and_heal_answer_as_data() {
    let dir = work_dir("doctor");
    let mut server = Server::start(&dir);

    // No model is configured here: a failed check is the answer.
    let result = server.call(1, "flowproof_doctor", json!({ "check": "ai" }));
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["passed"], false, "{result}");
    assert!(result["structuredContent"]["report"]["checks"].is_array());
    assert!(result["structuredContent"]["cwd"].is_string());

    // fiori logs in for real and agent runs a caller's command: refused,
    // with the reason, and nothing is spawned.
    for (id, check, reason) in [(2, "fiori", "live Fiori"), (3, "agent", "supplies")] {
        let result = server.call(id, "flowproof_doctor", json!({ "check": check }));
        assert_eq!(result["isError"], true, "{result}");
        let error = result["structuredContent"]["error"]
            .as_str()
            .expect("error");
        assert!(error.contains(reason), "{check}: {error}");
        assert!(result["structuredContent"].get("exit_code").is_none());
    }

    // Audit reads the record a run wrote; with none yet, the engine says so.
    let audit = json!({ "dir": "flows" });
    assert_eq!(
        server.call(4, "flowproof_audit", audit.clone())["isError"],
        true
    );
    server.run(5, json!({ "spec": "flows" }));
    let result = server.call(6, "flowproof_audit", audit);
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["report"]["suite"], "flows");

    let heal = json!({ "spec": "flows/gated.flow.yaml" });
    let result = server.call(7, "flowproof_heal", heal);
    assert_eq!(result["isError"], true, "{result}");
    let error = result["structuredContent"]["error"]
        .as_str()
        .expect("error");
    assert!(
        error.contains("gated.trace.jsonl"),
        "names the missing trace: {error}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Plan 17, step 7: the engine's stderr lines reach a client that asked for
/// progress with a token, numbered, and no client that did not.
#[test]
fn progress_reaches_only_a_client_that_asked() {
    let dir = work_dir("progress");
    std::fs::write(
        dir.join("flows/suite.yaml"),
        "env_from: |\n  echo minting test data >&2\n  echo MCP_E2E_MARK=1\n",
    )
    .expect("suite");
    let mut server = Server::start(&dir);
    let arguments = json!({ "spec": "flows/gated.flow.yaml", "allow_exec": true });

    // `request` checks that the next message is the reply: no progress.
    let quiet = server.call(1, "flowproof_record", arguments.clone());
    assert_eq!(quiet["isError"], false, "{quiet}");

    let params = json!({
        "name": "flowproof_record",
        "arguments": arguments,
        "_meta": { "progressToken": "t" },
    });
    let message = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": params });
    server.send(&message.to_string());
    let mut lines = Vec::new();
    let reply = loop {
        let message = server.next();
        if message["method"] != "notifications/progress" {
            break message;
        }
        assert_eq!(message["params"]["progressToken"], "t", "{message}");
        assert_eq!(message["params"]["progress"], lines.len() + 1, "{message}");
        let line = message["params"]["message"].as_str().expect("message");
        lines.push(line.to_string());
    };
    assert_eq!(reply["id"], 2, "{reply}");
    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert!(
        lines.iter().any(|line| line.contains("minting test data")),
        "{lines:?}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Plan 17, step 7: `notifications/cancelled` stops the call's engine and
/// everything it started, here a suite shell and its `sleep`, and the
/// cancelled call gets no reply.
#[cfg(unix)]
#[test]
fn a_cancelled_record_leaves_no_process_behind() {
    let dir = work_dir("cancel");
    std::fs::write(
        dir.join("flows/suite.yaml"),
        "env_from: |\n  echo $$ > shell.pid\n  sleep 120 &\n  echo $! > sleep.pid\n  \
         wait\n  echo MCP_E2E_MARK=1\n",
    )
    .expect("suite");
    let mut server = Server::start(&dir);
    let arguments = json!({ "spec": "flows/gated.flow.yaml", "allow_exec": true });
    let params = json!({ "name": "flowproof_record", "arguments": arguments });
    let message = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": params });
    server.send(&message.to_string());
    let pids = ["shell.pid", "sleep.pid"].map(|file| pid_in(&dir.join("flows").join(file)));

    let cancel = json!({
        "jsonrpc": "2.0",
        "method": "notifications/cancelled",
        "params": { "requestId": 7, "reason": "test" },
    });
    server.send(&cancel.to_string());
    for pid in &pids {
        assert!(exits(pid), "process {pid} outlived the cancel");
    }
    // `request` checks that the next message is the ping's reply.
    server.request(8, "ping", json!({}));
    std::fs::remove_dir_all(&dir).ok();
}

/// The pid a suite shell wrote to `path`, once it has.
#[cfg(unix)]
fn pid_in(path: &Path) -> String {
    for _ in 0..600 {
        let pid = std::fs::read_to_string(path).unwrap_or_default();
        if pid.ends_with('\n') {
            return pid.trim().to_string();
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("{} was never written", path.display());
}

/// Whether `pid` exits within 15 seconds, past the server's 5-second grace.
#[cfg(unix)]
fn exits(pid: &str) -> bool {
    (0..300).any(|_| {
        let alive = Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success());
        if alive {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        !alive
    })
}
