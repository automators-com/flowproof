//! End-to-end cover for the `agent.url` HTTP-target driver (#211).
//!
//! `docs/agent-testing.md` admits the gap in its own coverage table:
//! "http-target (`agent.url`) | replay covered; the RECORD path has no test".
//! `proxy_port` appears nowhere in the test tree — only in `src/` — so the
//! driver flowproof offers for services it did not start had never been driven
//! through `record` at all.
//!
//! That is the half where the driver's distinctive risk lives. flowproof cannot
//! inject environment into a process it did not spawn, so a `url:` service must
//! ALREADY be pointed at the proxy by whoever started it. Getting that wrong is
//! the documented commonest failure, and it is a record-time failure: a replay
//! test can never exercise it, because by then the cassette exists.
//!
//! So this drives the real thing: a service started independently, pointed at
//! the fixed `proxy_port`, POSTed by flowproof, recorded, and then replayed with
//! no model reachable at all.
//!
//! Nothing here needs a model credential. The upstream is a loopback fake, and
//! replay makes zero model calls by invariant 1.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/url-target-service.py")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-url-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// A port free at this instant: bound and dropped, rather than assumed.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("probe binds");
    let p = l.local_addr().expect("addr").port();
    drop(l);
    p
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

/// The upstream model for the RECORD leg only. Replay never reaches it.
fn fake_model() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else { continue };
            let _ = read_http_request(&mut stream);
            let reply = serde_json::json!({
                "choices": [{"index": 0, "finish_reason": "stop",
                    "message": {"role": "assistant", "content": "handled"}}]
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

/// Wait until `port` accepts a connection, or give up.
fn wait_for_port(port: u16) -> bool {
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

/// Kill the service on drop, so a failing assertion cannot leak a process.
struct Service(std::process::Child);
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Record an `agent.url` flow against a service flowproof did not start, then
/// replay it with no model reachable.
#[test]
fn an_agent_url_service_records_and_replays() {
    let dir = work_dir("record");
    let service_py = dir.join("service.py");
    std::fs::copy(fixture(), &service_py).expect("stage service");

    let proxy_port = free_port();
    let service_port = free_port();

    // The service is started by US, pointed at the proxy port the spec fixes --
    // the one-variable cooperation the url driver asks of whoever runs it.
    let service = Service(
        std::process::Command::new("python3")
            .arg(&service_py)
            .arg(service_port.to_string())
            .env(
                "OPENAI_BASE_URL",
                format!("http://127.0.0.1:{proxy_port}/v1"),
            )
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("start the url service"),
    );
    assert!(
        wait_for_port(service_port),
        "the service must be listening before flowproof triggers it"
    );

    let spec = dir.join("service.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: A running service handles an incident\n\
             app: agent\n\
             agent:\n  url: http://127.0.0.1:{service_port}/task\n  \
             proxy_port: {proxy_port}\n\
             steps:\n\
             \x20 - prompt: Handle the incident.\n\
             \x20 - assert: reply contains handled\n",
        ),
    )
    .expect("spec");

    // --- RECORD: the leg that had no test. ---
    let rec = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("FLOWPROOF_AGENT_UPSTREAM", fake_model())
        .env("FLOWPROOF_AGENT_KEY", "not-a-real-key")
        .output()
        .expect("record the url-driven flow");
    assert!(
        rec.status.success(),
        "recording an agent.url flow must succeed; stdout={} stderr={}",
        String::from_utf8_lossy(&rec.stdout),
        String::from_utf8_lossy(&rec.stderr)
    );

    // A trace with a real turn in it -- not merely a file. A record that
    // triggered the service but captured nothing would still exit 0 on a
    // driver whose verdict came from the trigger's HTTP status.
    let trace = dir.join("service.trace.jsonl");
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        contents.contains("Handle the incident"),
        "the prompt reached the model through the service: {contents}"
    );
    assert!(
        contents.contains("handled"),
        "and the reply was captured at the model boundary: {contents}"
    );

    // --- REPLAY: no model reachable at all. ---
    let run = std::process::Command::new(FLOWPROOF_BIN)
        .arg("run")
        .arg(&spec)
        .current_dir(&dir)
        .output()
        .expect("replay the url-driven flow");
    assert!(
        run.status.success(),
        "replay must reproduce the trajectory with zero model calls; \
         stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    drop(service);
    std::fs::remove_dir_all(&dir).ok();
}
