//! Selected business workflows must not pick up candidate flows or execute
//! dependent operations after a prerequisite failed or never ran.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};

fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flowproof-suite-policy-{name}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("fixture operation succeeds");
    dir
}
fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(args)
        .current_dir(dir)
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .output()
        .expect("fixture operation succeeds")
}
fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn allowlist_omits_candidates_and_failed_prerequisite_blocks_only_dependents() {
    let dir = fixture("dependency");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server = tiny_http::Server::http("127.0.0.1:0").expect("fixture operation succeeds");
    let base = format!("http://{}", server.server_addr());
    let observed = Arc::clone(&requests);
    std::thread::spawn(move || {
        for request in server.incoming_requests() {
            observed
                .lock()
                .expect("fixture state lock")
                .push(request.url().to_owned());
            let status = if request.url() == "/fail" { 409 } else { 200 };
            request
                .respond(tiny_http::Response::empty(status))
                .expect("fixture operation succeeds");
        }
    });
    for flow in ["prepare", "post", "independent", "candidate"] {
        let spec = format!("{flow}.flow.yaml");
        std::fs::write(dir.join(&spec), format!("name: {flow}\napp: api\nsteps:\n  - assert_api:\n      request: GET ${{POLICY_BASE}}/{flow}\n      status: 200\n")).expect("fixture operation succeeds");
        let recorded = Command::new(env!("CARGO_BIN_EXE_flowproof"))
            .args(["record", &spec, "--author", "rules", "--no-repair"])
            .current_dir(&dir)
            .env("POLICY_BASE", &base)
            .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
            .output()
            .expect("fixture operation succeeds");
        assert!(
            recorded.status.success(),
            "{}",
            String::from_utf8_lossy(&recorded.stderr)
        );
    }
    // Replay a real trace against a failed assertion response.
    let failure = tiny_http::Server::http("127.0.0.1:0").expect("fixture operation succeeds");
    let failure_base = format!("http://{}", failure.server_addr());
    std::thread::spawn(move || {
        for request in failure.incoming_requests() {
            request
                .respond(tiny_http::Response::empty(409))
                .expect("fixture operation succeeds");
        }
    });
    std::fs::write(
        dir.join("prepare.values.yaml"),
        format!("POLICY_BASE: {failure_base}\n"),
    )
    .expect("fixture operation succeeds");
    std::fs::write(dir.join("suite.yaml"), format!("env:\n  POLICY_BASE: {base}\nflows: [prepare.flow.yaml, post.flow.yaml, independent.flow.yaml]\ndepends_on:\n  post.flow.yaml: [prepare.flow.yaml]\n")).expect("fixture operation succeeds");
    requests.lock().expect("fixture state lock").clear();
    let output = run(&dir, &["run", ".", "--json", "--strict"]);
    assert_eq!(output.status.code(), Some(1));
    let result = json(&output);
    assert_eq!(result["flows"].as_array().expect("flows array").len(), 3);
    assert_eq!(result["blocked"], 1);
    assert_eq!(result["flows"][1]["report"]["trace_id"], "skipped");
    assert_eq!(result["flows"][2]["report"]["passed"], true);
    assert_eq!(
        *requests.lock().expect("fixture state lock"),
        vec!["/independent"]
    );

    let manifest =
        std::fs::read_to_string(dir.join("suite.yaml")).expect("fixture operation succeeds");
    std::fs::write(
        dir.join("suite.yaml"),
        format!("{manifest}stop_on_failure: true\n"),
    )
    .expect("fixture operation succeeds");
    requests.lock().expect("fixture state lock").clear();
    let output = run(&dir, &["run", ".", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)["blocked"], 2);
    assert!(requests.lock().expect("fixture state lock").is_empty());
    std::fs::remove_dir_all(dir).expect("fixture operation succeeds");
}

#[test]
fn invalid_selection_and_dependency_graph_fail_before_hooks_or_recording() {
    let dir = fixture("validation");
    for flow in ["a", "b"] {
        std::fs::write(dir.join(format!("{flow}.flow.yaml")), format!("name: {flow}\napp: api\nsteps:\n  - assert_api:\n      request: GET http://127.0.0.1:9\n      status: 200\n")).expect("fixture operation succeeds");
    }
    for manifest in [
        "flows: []",
        "flows: [a.flow.yaml, a.flow.yaml]",
        "flows: [../a.flow.yaml]",
        "flows: [absent.flow.yaml]",
        "flows: [a.flow.yaml]\norder: [a.flow.yaml]",
        "flows: [a.flow.yaml]\ndepends_on:\n  b.flow.yaml: [a.flow.yaml]",
        "flows: [a.flow.yaml]\ndepends_on:\n  a.flow.yaml: [b.flow.yaml]",
        "flows: [a.flow.yaml, b.flow.yaml]\ndepends_on:\n  a.flow.yaml: [b.flow.yaml]\n  b.flow.yaml: [a.flow.yaml]",
    ] {
        std::fs::write(dir.join("suite.yaml"), format!("{manifest}\nenv_from: echo SHOULD_NOT_RUN\n")).expect("fixture operation succeeds");
        let output = run(&dir, &["run", ".", "--record-missing"]);
        assert_eq!(output.status.code(), Some(2), "{manifest}");
        assert!(!String::from_utf8_lossy(&output.stderr).contains("env_from output"), "validation must precede the data command");
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("a.flow.yaml", dir.join("alias.flow.yaml"))
            .expect("alias fixture");
        std::fs::write(
            dir.join("suite.yaml"),
            "flows: [a.flow.yaml, alias.flow.yaml]\n",
        )
        .expect("manifest");
        let output = run(&dir, &["run", "."]);
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("more than once"));
    }
    // A missing prerequisite is not a pass even though legacy skip reports
    // themselves are non-failing: its dependent must never execute.
    std::fs::write(
        dir.join("suite.yaml"),
        "flows: [a.flow.yaml, b.flow.yaml]\ndepends_on:\n  b.flow.yaml: [a.flow.yaml]\n",
    )
    .expect("fixture operation succeeds");
    let output = run(&dir, &["run", ".", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(json(&output)["blocked"], 1);
    std::fs::remove_dir_all(dir).expect("fixture operation succeeds");
}
