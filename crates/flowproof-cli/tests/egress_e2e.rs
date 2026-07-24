//! End to end for egress containment, Linux only: a real seccomp filter, a
//! fake model (loopback, exempt), a real non-loopback listener the flow
//! DECLARES, and a fake agent that connects to declared and undeclared
//! destinations. This is the ONLY place the seccomp RUNTIME is proven - it
//! cannot run on a non-Linux dev host - so CI runs it on the Linux runner
//! with `RUN_EGRESS_E2E=1`.
//!
//! Three flows exercise the whole verdict:
//! - declared-only + `assert_no_egress` -> record PASSES, a trace is minted;
//! - undeclared + `assert_no_egress` -> record FAILS naming the destination,
//!   NO trace is minted;
//! - undeclared with NO assertion -> record PASSES (containment denied the
//!   attempt), and the trace's `blocked` lane names the destination.
#![cfg(target_os = "linux")]

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::Command;

/// A hard per-test watchdog: if the guard is not dropped within `secs`, print
/// a diagnostic and `abort()` the whole test process. This exists because the
/// seccomp path once DEADLOCKED (the notify-fd handoff used a syscall the
/// filter traps), wedging CI for hours. With a watchdog a future deadlock
/// FAILS RED in ~1 minute instead of hanging. Each seccomp E2E arms one before
/// it does anything that could block; a normal completion drops the guard,
/// disarming it.
struct Watchdog {
    disarmed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Watchdog {
    fn arm(label: &'static str, secs: u64) -> Self {
        use std::sync::atomic::Ordering;
        let disarmed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&disarmed);
        std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
            while std::time::Instant::now() < deadline {
                if flag.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            if flag.load(Ordering::Relaxed) {
                return;
            }
            eprintln!(
                "egress E2E watchdog: `{label}` exceeded {secs}s - assuming a deadlock \
                 (e.g. the notify-fd handoff) and aborting so CI fails red instead of hanging"
            );
            std::process::abort();
        });
        Watchdog { disarmed }
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.disarmed
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Is this run allowed to exercise the (kernel-dependent) seccomp E2E?
fn enabled() -> bool {
    std::env::var("RUN_EGRESS_E2E")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-egress-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// This host's primary non-loopback IPv4: the UDP-connect trick picks the
/// egress interface's local address without sending a packet. The declared
/// listener binds here so the agent's connect to it is genuinely non-loopback
/// (loopback is exempt wholesale and would not exercise the allow-set).
fn host_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

/// A listener on `ip` that accepts and immediately closes, so the
/// supervisor's performed connect to a DECLARED destination succeeds. Returns
/// the bound `ip:port`.
fn declared_listener(ip: Ipv4Addr) -> String {
    let listener = TcpListener::bind((ip, 0)).expect("bind declared listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming().take(16).flatten() {
            drop(stream);
        }
    });
    addr.to_string()
}

/// A fake model: scripted two-turn trajectory (ask for `get_weather`, then
/// reply once a tool result arrives). Loopback, so it is exempt.
fn fake_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(8) {
            let Ok(mut stream) = stream else { continue };
            let req = read_http_request(&mut stream);
            let reply = if req.contains("\"role\":\"tool\"") || req.contains("\"role\": \"tool\"") {
                serde_json::json!({
                    "choices": [{"index": 0, "finish_reason": "stop",
                        "message": {"role": "assistant", "content": "It is sunny in Nairobi."}}]
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

/// A fake agent: it FIRST connects to the declared destination (which the
/// supervisor performs), THEN optionally attempts an undeclared one (which
/// the supervisor refuses - the agent swallows the error, as a resilient
/// client would), then drives the model to a reply.
const FAKE_AGENT: &str = r#"
import json, os, socket, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]

def dial(addr):
    host, port = addr.rsplit(":", 1)
    s = socket.socket()
    s.settimeout(5)
    try:
        s.connect((host, int(port)))
        return True
    except OSError:
        return False
    finally:
        s.close()

allowed = os.environ.get("ALLOWED_ADDR")
if allowed and not dial(allowed):
    # A declared destination MUST be reachable under containment.
    raise SystemExit("declared destination was refused")

undeclared = os.environ.get("UNDECLARED_ADDR")
if undeclared:
    dial(undeclared)  # refused by the supervisor; ignore and continue

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
            messages.append({"role": "tool", "tool_call_id": call["id"],
                             "content": json.dumps({"sky": "clear"})})
        continue
    print(msg.get("content", ""))
    break
"#;

/// Run `flowproof record` as a subprocess so its output can be inspected.
/// Returns (exit_ok, combined stdout+stderr).
fn record(spec: &Path, model: &str) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(["record", spec.to_str().expect("utf8")])
        .env("FLOWPROOF_AGENT_UPSTREAM", model)
        .output()
        .expect("run flowproof record");
    let mut combined = String::from_utf8_lossy(&out.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), combined)
}

fn write_spec(
    dir: &Path,
    agent_py: &Path,
    allow: &str,
    extra_env: &str,
    assert_egress: bool,
) -> PathBuf {
    let spec = dir.join("flow.flow.yaml");
    let assert_line = if assert_egress {
        "  - assert_no_egress\n"
    } else {
        ""
    };
    std::fs::write(
        &spec,
        format!(
            "name: Contained weather\n\
             app: agent\n\
             agent:\n  command: python3 {agent}\n  allow_egress:\n    - {allow}\n{extra_env}\
             tools:\n  - name: get_weather\n    result: {{ sky: clear, temp: 25 }}\n\
             steps:\n\
             \x20 - prompt: What is the weather in Nairobi?\n\
             \x20 - assert: reply contains sunny\n\
             {assert_line}",
            agent = agent_py.display()
        ),
    )
    .expect("spec");
    spec
}

/// The declared destination is reachable and, with `assert_no_egress`, the
/// run passes and mints a trace - the report says containment is enforced.
#[test]
fn a_declared_flow_passes_and_mints_a_trace() {
    if !enabled() {
        eprintln!("RUN_EGRESS_E2E not set; skipping the seccomp E2E");
        return;
    }
    let Some(host) = host_ipv4() else {
        eprintln!("no non-loopback IPv4 on this host; skipping");
        return;
    };
    let _watchdog = Watchdog::arm("a_declared_flow_passes_and_mints_a_trace", 60);
    let dir = work_dir("declared");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let allowed = declared_listener(host);
    let env = format!("  env:\n    ALLOWED_ADDR: {allowed}\n");
    let spec = write_spec(&dir, &agent_py, &allowed, &env, true);

    let (ok, output) = record(&spec, &fake_model());
    assert!(ok, "declared flow must record: {output}");
    assert!(
        output.contains("enforced (linux seccomp)"),
        "the tier line must report enforcement: {output}"
    );
    let trace = dir.join("flow.trace.jsonl");
    assert!(trace.exists(), "a declared flow mints a trace");
    let contents = std::fs::read_to_string(&trace).expect("trace");
    // The audit lane records the containment tier and the UNRESOLVED allow.
    assert!(
        contents.contains("egress"),
        "the egress lane is written: {contents}"
    );
    assert!(
        contents.contains(&allowed),
        "the allow-list is audited: {contents}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// An undeclared destination with `assert_no_egress` FAILS the record naming
/// the destination, and mints NO trace.
#[test]
fn an_undeclared_flow_fails_assert_no_egress_and_mints_no_trace() {
    if !enabled() {
        eprintln!("RUN_EGRESS_E2E not set; skipping the seccomp E2E");
        return;
    }
    let Some(host) = host_ipv4() else {
        eprintln!("no non-loopback IPv4 on this host; skipping");
        return;
    };
    let _watchdog = Watchdog::arm(
        "an_undeclared_flow_fails_assert_no_egress_and_mints_no_trace",
        60,
    );
    let dir = work_dir("undeclared");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let allowed = declared_listener(host);
    // 198.51.100.9 (TEST-NET-2) is undeclared and never reached: the
    // supervisor refuses it before any packet leaves.
    let env =
        format!("  env:\n    ALLOWED_ADDR: {allowed}\n    UNDECLARED_ADDR: 198.51.100.9:443\n");
    let spec = write_spec(&dir, &agent_py, &allowed, &env, true);

    let (ok, output) = record(&spec, &fake_model());
    assert!(
        !ok,
        "an undeclared attempt must fail assert_no_egress: {output}"
    );
    assert!(
        output.contains("undeclared egress attempted"),
        "the failure names the violation: {output}"
    );
    assert!(
        output.contains("198.51.100.9:443"),
        "the failure names the destination: {output}"
    );
    assert!(
        !dir.join("flow.trace.jsonl").exists(),
        "no trace for a flow that failed assert_no_egress"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Without the assertion, an undeclared attempt is DENIED (the agent gets
/// ECONNREFUSED and carries on) and RECORDED: the run passes and the trace's
/// blocked lane names the destination it never reached.
#[test]
fn an_undeclared_attempt_is_denied_and_recorded_in_the_blocked_lane() {
    if !enabled() {
        eprintln!("RUN_EGRESS_E2E not set; skipping the seccomp E2E");
        return;
    }
    let Some(host) = host_ipv4() else {
        eprintln!("no non-loopback IPv4 on this host; skipping");
        return;
    };
    let _watchdog = Watchdog::arm(
        "an_undeclared_attempt_is_denied_and_recorded_in_the_blocked_lane",
        60,
    );
    let dir = work_dir("recorded");
    let agent_py = dir.join("agent.py");
    std::fs::write(&agent_py, FAKE_AGENT).expect("agent");
    let allowed = declared_listener(host);
    let env =
        format!("  env:\n    ALLOWED_ADDR: {allowed}\n    UNDECLARED_ADDR: 198.51.100.9:443\n");
    // No `assert_no_egress`: the run is not asked to certify, so a denied
    // attempt is contained and logged, not a failure.
    let spec = write_spec(&dir, &agent_py, &allowed, &env, false);

    let (ok, output) = record(&spec, &fake_model());
    assert!(ok, "a denied-but-not-asserted flow still records: {output}");
    let trace = dir.join("flow.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace");
    assert!(
        contents.contains("198.51.100.9:443"),
        "the blocked lane names the denied destination: {contents}"
    );
    assert!(
        contents.contains("blocked"),
        "a blocked lane exists: {contents}"
    );
    std::fs::remove_dir_all(&dir).ok();
}
