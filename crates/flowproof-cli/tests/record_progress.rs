use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::time::Duration;

#[test]
fn progress_arrives_before_recording_finishes_and_keeps_json_clean() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let dir = std::env::temp_dir().join(format!("flowproof-progress-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let spec = dir.join("health.flow.yaml");
    std::fs::write(&spec, format!("name: Health\napp: api\nsteps:\n  - assert_api:\n      request: GET http://{}/health\n      status: 200\n", server.server_addr())).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(["record", spec.to_str().unwrap(), "--json", "--no-repair"])
        .env("FLOWPROOF_PROGRESS", "1")
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stderr = BufReader::new(child.stderr.take().unwrap());
    let mut first = String::new();
    stderr.read_line(&mut first).unwrap();
    assert_eq!(first.trim(), "[PROGRESS] Preparing recording");
    assert!(
        child.try_wait().unwrap().is_none(),
        "progress precedes completion"
    );
    let request = server
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    request
        .respond(tiny_http::Response::from_string("ok"))
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output.status);
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["steps"], 1);
    let mut rest = String::new();
    stderr.read_to_string(&mut rest).unwrap();
    assert!(rest.contains("[PROGRESS] Recording steps"));
    std::fs::remove_dir_all(dir).unwrap();
}
