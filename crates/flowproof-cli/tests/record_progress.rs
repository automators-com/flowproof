use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn progress_arrives_before_recording_finishes_and_keeps_json_clean() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("bind test server");
    let dir = std::env::temp_dir().join(format!("flowproof-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create test directory");
    let spec = dir.join("health.flow.yaml");
    std::fs::write(&spec, format!("name: Health\napp: api\nsteps:\n  - assert_api:\n      request: GET http://{}/health\n      status: 200\n", server.server_addr())).expect("write test flow");
    let mut child = Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args([
            "record",
            spec.to_str().expect("UTF-8 temporary path"),
            "--json",
            "--no-repair",
        ])
        .env("FLOWPROOF_PROGRESS", "1")
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start recording");
    let mut stderr = BufReader::new(child.stderr.take().expect("capture stderr"));
    let mut first = String::new();
    stderr
        .read_line(&mut first)
        .expect("read first progress event");
    assert_eq!(first.trim(), "[PROGRESS] Preparing recording");
    assert!(
        child.try_wait().expect("check recording status").is_none(),
        "progress precedes completion"
    );
    let request = server
        .recv_timeout(Duration::from_secs(10))
        .expect("receive test request")
        .expect("request arrives before timeout");
    request
        .respond(tiny_http::Response::from_string("ok"))
        .expect("respond to request");
    let output = child.wait_with_output().expect("wait for recording");
    assert!(output.status.success(), "{:?}", output.status);
    let result: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse JSON result");
    assert_eq!(result["steps"], 1);
    let mut rest = String::new();
    stderr
        .read_to_string(&mut rest)
        .expect("read remaining progress");
    assert!(rest.contains("[PROGRESS] Recording steps"));
    std::fs::remove_dir_all(dir).expect("remove test directory");
}
