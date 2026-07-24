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

// ---- assert_no_secret_leak (security spine) ----

/// A resolved DB password that is long enough to scan for precisely and
/// distinctive enough that a substring hit is unambiguous.
const SECRET: &str = "s3cr3t-db-pw-90210";

/// The flowproof binary, so a record run's stderr can be READ - the message
/// assertions (names the variable, names the step, never prints the value)
/// need the text, which `run_cli` prints to this process's stderr. Running
/// out of process also scopes the env to the child via `.env(...)`, so these
/// tests never mutate the process-global `FLOWPROOF_AGENT_UPSTREAM` and need
/// no `ENV_LOCK`.
const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// A one-turn fake model: it always replies with a fixed text, so the run is
/// driven entirely by whatever the agent sends. Serves a bounded number of
/// connections then exits.
fn secret_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_http_request(&mut stream);
            let body = serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "acknowledged"}}]
            })
            .to_string();
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

/// A fake agent that echoes the `LEAK_ME` env var (if flowproof injected
/// one) into the message it sends to the model - so the resolved secret
/// lands in the cassette request body, the corpus the scan reads. When
/// `LEAK_ME` is unset, the message carries no secret and the run is clean.
const SECRET_AGENT: &str = r#"
import json, os, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
leak = os.environ.get("LEAK_ME", "")
content = prompt if not leak else prompt + " connection=postgres://user:" + leak + "@db"
payload = json.dumps({
    "model": "gpt-4o",
    "messages": [{"role": "user", "content": content}],
}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                            headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    msg = json.load(resp)["choices"][0]["message"]
print(msg.get("content", ""))
"#;

/// Write a secret-handling spec. When `leak` is true the agent is handed the
/// DB password (which it echoes into its model call); either way the flow
/// asserts the password must never surface.
fn write_secret_spec(dir: &Path, agent_py: &Path, leak: bool) -> PathBuf {
    let leak_env = if leak {
        "\n  env:\n    LEAK_ME: ${DB_PASSWORD}"
    } else {
        ""
    };
    let spec = dir.join("secret.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Handles the DB password\n\
             app: agent\n\
             agent:\n  command: python3 {agent}{leak_env}\n\
             control:\n  id: sec.portal.no-db-password-leak\n  title: The DB password never surfaces\n\
             steps:\n\
             \x20 - prompt: Connect to the reporting database\n\
             \x20 - assert: reply contains acknowledged\n\
             \x20 - assert_no_secret_leak: ${{DB_PASSWORD}}\n",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

/// (a) A run whose model request carries the resolved `${DB_PASSWORD}` FAILS
/// the record, names the variable and the step index, mints NO trace, and
/// NEVER prints the secret value.
#[test]
fn a_leaked_secret_fails_the_record_and_mints_no_trace() {
    let dir = work_dir("secret-leak");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, true);

    let output = std::process::Command::new(FLOWPROOF_BIN)
        .args(["record", spec.to_str().expect("utf8")])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("run flowproof record");

    assert!(
        !output.status.success(),
        "a leaked secret must fail the record"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Names the variable and the step index...
    assert!(
        stderr.contains("${DB_PASSWORD}"),
        "the message names the variable: {stderr}"
    );
    assert!(
        stderr.contains("step 3"),
        "the message names the step index: {stderr}"
    );
    // ...and NEVER the resolved value.
    assert!(
        !stderr.contains(SECRET),
        "the failure message must never contain the secret value: {stderr}"
    );
    // The store-guard: no trace reaches disk when a secret leaked into it.
    assert!(
        !dir.join("secret.trace.jsonl").exists(),
        "no trace for a run that leaked a secret"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// (b) A clean flow with the SAME assertion PASSES and mints a trace, and
/// (c) it replays green with zero network (no real model).
#[test]
fn a_clean_secret_flow_records_and_replays_deterministically() {
    let dir = work_dir("secret-clean");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, false);
    let trace = dir.join("secret.trace.jsonl");

    // RECORD: the secret is set (so the assertion resolves) but never enters
    // the corpus, so the scan finds nothing and the trace is minted.
    let record = std::process::Command::new(FLOWPROOF_BIN)
        .args(["record", spec.to_str().expect("utf8")])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("run flowproof record");
    assert!(
        record.status.success(),
        "a clean flow must record: {}",
        String::from_utf8_lossy(&record.stderr)
    );
    assert!(trace.exists(), "a clean flow mints its trace");
    // Belt and suspenders: the secret value is not in the minted trace.
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        !contents.contains(SECRET),
        "the secret value is never written to disk: {contents}"
    );

    // REPLAY with NO upstream at all - a stray real call would fail loudly.
    let replay = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("run flowproof run");
    assert!(
        replay.status.success(),
        "the clean flow must replay green with zero network: {}",
        String::from_utf8_lossy(&replay.stderr)
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `flowproof audit` folds the control-bearing flow into a control-coverage
/// report: the control id, its pass verdict, and (for the secret-leak flow)
/// the secrets_checked / corpus / excluded fields - in both YAML and JSON,
/// naming the variable but never its value. Audit READS the run record, so the
/// flow is recorded AND run before auditing; audit never re-replays.
#[test]
fn audit_renders_the_control_map_in_yaml_and_json() {
    let dir = work_dir("secret-audit");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, SECRET_AGENT).expect("agent");
    let spec = write_secret_spec(&dir, &agent_py, false);

    // Record the clean flow.
    let record = std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "record",
            dir.join("secret.flow.yaml").to_str().expect("utf8"),
        ])
        .env("FLOWPROOF_AGENT_UPSTREAM", secret_model())
        .env("DB_PASSWORD", SECRET)
        .output()
        .expect("record");
    assert!(record.status.success(), "record for audit");

    // Run it so a run record is written for audit to read - zero network.
    let run = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("run");
    assert!(
        run.status.success(),
        "the clean flow replays green: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    // Audit as YAML (the default).
    let yaml = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8")])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("audit yaml");
    assert!(
        yaml.status.success(),
        "audit exits clean when the control holds: {}",
        String::from_utf8_lossy(&yaml.stderr)
    );
    let yaml_out = String::from_utf8_lossy(&yaml.stdout);
    assert!(
        yaml_out.contains("sec.portal.no-db-password-leak"),
        "audit names the control id: {yaml_out}"
    );
    assert!(
        yaml_out.contains("verdict: pass"),
        "the control passed: {yaml_out}"
    );
    assert!(
        yaml_out.contains("${DB_PASSWORD}"),
        "secrets_checked names the variable: {yaml_out}"
    );
    assert!(
        yaml_out.contains("secrets_checked"),
        "the corpus/exclusion fields are present: {yaml_out}"
    );
    // Never the value.
    assert!(
        !yaml_out.contains(SECRET),
        "the audit never prints the secret value: {yaml_out}"
    );

    // Audit as JSON.
    let json = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8"), "--json"])
        .env("DB_PASSWORD", SECRET)
        .env_remove("FLOWPROOF_AGENT_UPSTREAM")
        .env_remove("OPENAI_BASE_URL")
        .output()
        .expect("audit json");
    assert!(json.status.success(), "audit --json exits clean");
    let value: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("audit --json is valid JSON");
    let control = &value["controls"][0];
    assert_eq!(control["id"], "sec.portal.no-db-password-leak");
    assert_eq!(control["verdict"], "pass");
    assert_eq!(control["secrets_checked"][0], "${DB_PASSWORD}");

    std::fs::remove_dir_all(&dir).ok();
}
