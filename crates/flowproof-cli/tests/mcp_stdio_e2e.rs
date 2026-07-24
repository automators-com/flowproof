//! End to end for the MCP boundary (v3.1), with NO real MCP server and no
//! agent framework: a fake stdio MCP server (a small Python process) and a
//! fake agent (a Python process that makes one model call, then drives the
//! MCP stand-in over JSON-RPC). The full spec -> record -> trace -> replay
//! path runs through flowproof's own `mcp-stdio` stand-in binary.
//!
//! Unix-only for the same reason as the other agent suite tests: the fakes
//! are `python3` processes and the assertions are platform-neutral.
#![cfg(unix)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

/// Serializes the tests that mutate the process-global `FLOWPROOF_AGENT_UPSTREAM`
/// env var. `cargo test` runs a binary's tests on parallel threads and env vars
/// are process-global, so without this lock one test's `set_var`/`remove_var`
/// races another's read: the agent child can pick up a different test's upstream
/// address and the run flakes (a fake model refuses the connection). Each such
/// test holds this guard for its whole body. Poison-tolerant so one panicking
/// test does not cascade a failure into all the others.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_env() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner())
}

/// Point flowproof's stand-in at the REAL `flowproof` binary. In-process
/// `run_cli` makes `current_exe()` the test harness, so without this the
/// agent would spawn the test binary instead of `flowproof mcp-stdio`.
fn use_real_flowproof_exe() {
    std::env::set_var("FLOWPROOF_MCP_EXE", env!("CARGO_BIN_EXE_flowproof"));
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-mcp-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// A fake model: replies `done` to every request, no tools. Enough to make
/// the cassette non-empty so the agent flow's model-boundary progress guard
/// is satisfied - the point of this suite is the MCP boundary, not the
/// model one.
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

/// A deterministic stdio MCP server. Answers `initialize` / `tools/list` /
/// `tools/call` line by line, and appends every request it RECEIVES to the
/// log path in argv[1] - so a test can prove which tools the real server was
/// (and was not) asked for.
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
        continue  # a notification has no response
    method = msg.get("method")
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05",
                  "serverInfo": {"name": "weather", "version": "1"},
                  "capabilities": {"tools": {}}}
    elif method == "tools/list":
        result = {"tools": [{"name": "get_weather"}, {"name": "send_alert"}]}
    elif method == "tools/call":
        name = msg["params"]["name"]
        result = {"content": [{"type": "text", "text": "REAL:" + name}], "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#;

/// A fake agent: one model call (so the cassette is non-empty), then it
/// spawns the MCP stand-in named by FLOWPROOF_MCP_SERVER_WEATHER and drives a
/// short JSON-RPC exchange: initialize, an initialized notification,
/// tools/list, tools/call get_weather. The weather city is read from
/// `city.txt` next to the script (so record and replay can differ without an
/// env race), and it calls send_alert too when `danger.txt` exists.
const FAKE_AGENT: &str = r#"
import json, os, shlex, subprocess, urllib.request

here = os.path.dirname(os.path.abspath(__file__))
base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]

payload = json.dumps({"model": "gpt-4o",
                      "messages": [{"role": "user", "content": prompt}]}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    reply = json.load(resp)["choices"][0]["message"].get("content", "")

cmd = os.environ["FLOWPROOF_MCP_SERVER_WEATHER"]
proc = subprocess.Popen(shlex.split(cmd), stdin=subprocess.PIPE, stdout=subprocess.PIPE)

def rpc(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()
    return json.loads(proc.stdout.readline())

def notify(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()

rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2024-11-05",
                "clientInfo": {"name": "fake-agent", "version": "1"},
                "capabilities": {}}})
notify({"jsonrpc": "2.0", "method": "notifications/initialized"})
rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})

city_path = os.path.join(here, "city.txt")
city = open(city_path).read().strip() if os.path.exists(city_path) else "Nairobi"
weather = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "get_weather", "arguments": {"city": city}}})
print("WEATHER", json.dumps(weather))

if os.path.exists(os.path.join(here, "danger.txt")):
    alert = rpc({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                 "params": {"name": "send_alert", "arguments": {"msg": "evac"}}})
    print("ALERT", json.dumps(alert))

proc.stdin.close()
proc.wait()
print(reply)
"#;

/// A deterministic stdio MCP server that sends a server NOTIFICATION. On
/// `tools/list` it writes `notifications/tools/list_changed` BEFORE the
/// response, so the notification crosses while the client is still blocked on
/// the list response - anchoring it deterministically at the count of calls
/// issued so far (initialize + tools/list = 2).
const FAKE_MCP_SERVER_NOTIFY: &str = r#"
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
    if method == "tools/list":
        sys.stdout.write(json.dumps({"jsonrpc": "2.0",
            "method": "notifications/tools/list_changed", "params": {}}) + "\n")
        sys.stdout.flush()
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid,
            "result": {"tools": [{"name": "get_weather"}]}}) + "\n")
        sys.stdout.flush()
        continue
    if method == "initialize":
        result = {"protocolVersion": "2024-11-05",
                  "serverInfo": {"name": "weather", "version": "1"},
                  "capabilities": {"tools": {}}}
    elif method == "tools/call":
        name = msg["params"]["name"]
        result = {"content": [{"type": "text", "text": "REAL:" + name}], "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#;

/// A stdio MCP server that sends a server-initiated REQUEST (a message with
/// BOTH `method` and `id`) right after answering `initialize` - the v3.4
/// case v3.3 must FAIL LOUDLY on, not capture. It still answers every client
/// request so the agent does not hang.
const FAKE_MCP_SERVER_REQUEST: &str = r#"
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
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
        sys.stdout.flush()
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": 999,
            "method": "sampling/createMessage", "params": {}}) + "\n")
        sys.stdout.flush()
        continue
    if method == "tools/list":
        result = {"tools": [{"name": "get_weather"}]}
    elif method == "tools/call":
        name = msg["params"]["name"]
        result = {"content": [{"type": "text", "text": "REAL:" + name}], "isError": False}
    else:
        result = {}
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": mid, "result": result}) + "\n")
    sys.stdout.flush()
"#;

/// A notification-aware fake agent: like FAKE_AGENT, but it dispatches lines
/// by JSON-RPC shape - a server notification (a method, no id) is collected
/// rather than mistaken for a response - and writes the notifications it
/// received to `notifications.txt`, so a test can prove the agent got them.
const NOTIFY_AGENT: &str = r#"
import json, os, shlex, subprocess, urllib.request

here = os.path.dirname(os.path.abspath(__file__))
base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]

payload = json.dumps({"model": "gpt-4o",
                      "messages": [{"role": "user", "content": prompt}]}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    reply = json.load(resp)["choices"][0]["message"].get("content", "")

cmd = os.environ["FLOWPROOF_MCP_SERVER_WEATHER"]
proc = subprocess.Popen(shlex.split(cmd), stdin=subprocess.PIPE, stdout=subprocess.PIPE)

notifications = []

def read_message():
    while True:
        line = proc.stdout.readline()
        if not line:
            return None
        msg = json.loads(line)
        if "method" in msg and msg.get("id") is None:
            notifications.append(msg["method"])
            continue
        return msg

def rpc(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()
    return read_message()

def notify(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()

rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2024-11-05",
                "clientInfo": {"name": "fake-agent", "version": "1"},
                "capabilities": {}}})
notify({"jsonrpc": "2.0", "method": "notifications/initialized"})
rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
     "params": {"name": "get_weather", "arguments": {"city": "Nairobi"}}})

proc.stdin.close()
proc.wait()
open(os.path.join(here, "notifications.txt"), "w").write("\n".join(notifications))
print(reply)
"#;

/// A model-only fake agent: it makes its model call but NEVER spawns the MCP
/// stand-in - the mispointed-agent case the record wiring guard catches.
const MODEL_ONLY_AGENT: &str = r#"
import json, os, urllib.request
base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
payload = json.dumps({"model": "gpt-4o",
                      "messages": [{"role": "user", "content": prompt}]}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    print(json.load(resp)["choices"][0]["message"].get("content", ""))
"#;

fn write(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write file");
}

/// Write a spec whose agent uses one MCP server; `mocked` lists tools mocked
/// at the MCP boundary.
fn write_spec(
    dir: &Path,
    agent_py: &Path,
    server_py: &Path,
    log: &Path,
    mocked: &[&str],
) -> PathBuf {
    let spec = dir.join("weather.flow.yaml");
    let mocks = if mocked.is_empty() {
        String::new()
    } else {
        let mut s = String::from("    tools:\n");
        for name in mocked {
            s.push_str(&format!(
                "      - name: {name}\n        result: {{ delivered: true }}\n"
            ));
        }
        s
    };
    write(
        &spec,
        &format!(
            "name: MCP weather\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n\
             mcp:\n  - name: weather\n    command: python3 {server} {log}\n{mocks}\
             steps:\n\
             \x20 - prompt: What is the weather?\n\
             \x20 - assert: reply contains done\n",
            agent = agent_py.display(),
            server = server_py.display(),
            log = log.display(),
        ),
    );
    spec
}

/// Full record then replay: the trace captures the MCP lane, and replay
/// reproduces it with ZERO real-server spawns.
#[test]
fn records_and_replays_the_mcp_lane() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("basic");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, FAKE_AGENT);
    write(&server_py, FAKE_MCP_SERVER);
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &[]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording an MCP agent flow should succeed");

    let trace = dir.join("weather.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    // The MCP lane is captured under the server name, with the real tool
    // result recorded verbatim.
    assert!(
        contents.contains("\"mcp\""),
        "mcp section present: {contents}"
    );
    assert!(contents.contains("\"weather\""), "server named: {contents}");
    assert!(
        contents.contains("initialize"),
        "handshake captured: {contents}"
    );
    assert!(
        contents.contains("tools/list"),
        "listing captured: {contents}"
    );
    assert!(
        contents.contains("get_weather"),
        "call captured: {contents}"
    );
    assert!(
        contents.contains("REAL:get_weather"),
        "real result captured: {contents}"
    );

    // The real server WAS asked (record touches it once).
    let logged = std::fs::read_to_string(&log).expect("server log");
    assert!(
        logged.contains("get_weather"),
        "real server was asked: {logged}"
    );

    // REPLAY with the real server deleted from disk: proof that no external
    // process is spawned - the lane is served from the trace.
    std::fs::remove_file(&log).ok();
    std::fs::remove_file(&server_py).expect("remove the real server");
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay must reproduce the MCP lane offline");
    assert!(
        !log.exists(),
        "replay spawned no real server, so its log was never re-created"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A tool mocked at the MCP boundary is answered locally: the real server is
/// NEVER asked for it (record), and replay serves the mock.
#[test]
fn a_mocked_tool_is_intercepted_and_never_forwarded() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("mocked");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, FAKE_AGENT);
    write(&server_py, FAKE_MCP_SERVER);
    write(&dir.join("danger.txt"), "yes"); // the agent will call send_alert
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &["send_alert"]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording with a mocked MCP tool should succeed");

    // The real server saw the non-mocked calls but NEVER the dangerous one.
    let logged = std::fs::read_to_string(&log).expect("server log");
    assert!(
        logged.contains("get_weather"),
        "safe tool forwarded: {logged}"
    );
    assert!(
        !logged.contains("send_alert"),
        "the mocked dangerous tool must never reach the real server: {logged}"
    );

    // The trace pins the mock, not a real answer, for send_alert.
    let trace = dir.join("weather.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        contents.contains("send_alert"),
        "mocked call recorded: {contents}"
    );
    assert!(
        contents.contains("delivered"),
        "the mock is what got recorded: {contents}"
    );
    assert!(
        !contents.contains("REAL:send_alert"),
        "no real answer for the mocked tool: {contents}"
    );

    // REPLAY serves the mock with no real server on disk.
    std::fs::remove_file(&server_py).expect("remove real server");
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "replay serves the mocked tool offline");

    std::fs::remove_dir_all(&dir).ok();
}

/// A replay whose client sends a different `tools/call` argument gets the
/// JSON-RPC error, and the run fails naming the divergent path.
#[test]
fn a_divergent_argument_fails_replay_naming_the_path() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("divergence");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, FAKE_AGENT);
    write(&server_py, FAKE_MCP_SERVER);
    write(&dir.join("city.txt"), "Nairobi");
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &[]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording should succeed");

    // Replay the SAME spec, but the agent now asks for a different city.
    write(&dir.join("city.txt"), "Paris");
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 1, "a diverged MCP argument must fail the run");

    std::fs::remove_dir_all(&dir).ok();
}

/// The record wiring guard: a declared server the agent never contacts fails
/// the record with the named message.
#[test]
fn a_declared_server_the_agent_ignores_fails_the_record() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("guard");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, MODEL_ONLY_AGENT); // never spawns the stand-in
    write(&server_py, FAKE_MCP_SERVER);
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &[]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_ne!(code, 0, "an uncontacted MCP server must fail the record");
    assert!(
        !dir.join("weather.trace.jsonl").exists(),
        "no trace when a declared MCP server was never contacted"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A server NOTIFICATION is captured into the mcp lane at the right anchor at
/// record, and re-emitted to the agent at replay after the anchoring call -
/// with ZERO real-server spawns. The run passes both times: a notification is
/// an emission, not an assertion.
#[test]
fn records_and_replays_a_server_notification() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("notify");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, NOTIFY_AGENT);
    write(&server_py, FAKE_MCP_SERVER_NOTIFY);
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &[]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "recording a notifying MCP server should succeed");

    // The trace lane carries the notification as an event, anchored after the
    // two calls issued when it crossed (initialize + tools/list).
    let trace = dir.join("weather.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    let doc: serde_json::Value = serde_json::from_str(&contents).expect("trace is json");
    let event = &doc["mcp"]["weather"]["events"][0];
    assert_eq!(
        event["method"], "notifications/tools/list_changed",
        "notification captured: {contents}"
    );
    assert_eq!(event["after"], 2, "anchored after initialize + tools/list");

    // The agent received the notification at record too.
    let got = std::fs::read_to_string(dir.join("notifications.txt")).expect("notifications");
    assert!(
        got.contains("notifications/tools/list_changed"),
        "agent got the notification at record: {got}"
    );

    // REPLAY with the real server deleted: the notification is re-emitted from
    // the trace, not the server.
    std::fs::remove_file(&server_py).expect("remove the real server");
    std::fs::remove_file(dir.join("notifications.txt")).ok();
    std::env::remove_var("FLOWPROOF_AGENT_UPSTREAM");
    std::env::remove_var("OPENAI_BASE_URL");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(
        code, 0,
        "replay reproduces the lane and re-emits the notification"
    );

    let got = std::fs::read_to_string(dir.join("notifications.txt")).expect("notifications");
    assert!(
        got.contains("notifications/tools/list_changed"),
        "agent received the notification at replay too: {got}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// STDIO PARITY (v3.3): a server-initiated REQUEST (a `method` AND an `id`)
/// mid-record fails the record LOUDLY - like the HTTP transport - rather than
/// mis-parking the client's answer and corrupting the lane silently. No trace
/// is written.
#[test]
fn a_server_initiated_request_fails_the_record_loudly() {
    let _env = lock_env();
    use_real_flowproof_exe();
    let dir = work_dir("server-request");
    let agent_py = dir.join("agent.py");
    let server_py = dir.join("server.py");
    let log = dir.join("server.log");
    write(&agent_py, NOTIFY_AGENT);
    write(&server_py, FAKE_MCP_SERVER_REQUEST);
    let spec = write_spec(&dir, &agent_py, &server_py, &log, &[]);

    std::env::set_var("FLOWPROOF_AGENT_UPSTREAM", fake_model());
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_ne!(
        code, 0,
        "a server-initiated request mid-record must fail the record"
    );
    assert!(
        !dir.join("weather.trace.jsonl").exists(),
        "no trace is minted when a server-initiated request was seen"
    );

    std::fs::remove_dir_all(&dir).ok();
}
