//! Traceless specs in a suite run: skipped-with-reason by default (one
//! partial commit must not hard-fail everyone), `--strict` restores the
//! hard error, `--record-missing` records in place. Unix-only for the
//! same reason as suite_env_from.rs (sh-backed helpers elsewhere in the
//! suite machinery); the semantics under test are platform-neutral.
#![cfg(unix)]

use std::path::PathBuf;

fn suite_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-missing-trace-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("suite dir");
    dir
}

/// An `app: api` spec + tiny_http responder: recordable on any OS.
fn write_api_spec(dir: &std::path::Path, var: &str) -> PathBuf {
    let spec = dir.join("health.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Health\napp: api\nsteps:\n  - assert_api:\n      request: GET ${{{var}}}/health\n      status: 200\n"
        ),
    )
    .expect("spec");
    spec
}

#[test]
fn traceless_spec_is_skipped_by_default_and_junit_counts_it() {
    let dir = suite_dir("skip");
    // No server needed: the spec is never executed, only parsed for a name.
    write_api_spec(&dir, "MT_SKIP_BASE");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(code, 0, "a skip-only suite passes");

    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("suite junit written");
    assert!(junit.contains("skipped=\"1\""), "junit counts it: {junit}");
    assert!(
        junit.contains("<skipped message=\"no trace recorded"),
        "reason travels into junit: {junit}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn strict_mode_restores_the_hard_error() {
    let dir = suite_dir("strict");
    write_api_spec(&dir, "MT_STRICT_BASE");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8"), "--strict"]);
    assert_eq!(code, 2, "--strict makes a missing trace a hard error");
    // Behavior change (round-3 field ruling): --strict still exits 2, but
    // the missing trace is now ONE errored flow rather than a run abort,
    // so the merged junit is still written. `--strict` exists to stop
    // coverage shrinking silently; suppressing the CI report was never
    // part of that job, and it is exactly what hid the field failure.
    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("strict still reports to CI");
    assert!(junit.contains("<error message="), "junit: {junit}");
    assert!(
        junit.contains("not found"),
        "junit names the missing trace: {junit}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn record_missing_records_then_replays() {
    let dir = suite_dir("record");
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    std::env::set_var("MT_RECORD_BASE", &base);
    // record probes once, replay probes once.
    let server_thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok(request) = server.recv() else { break };
            let code = if request.url() == "/health" { 200 } else { 404 };
            request
                .respond(tiny_http::Response::from_string("ok").with_status_code(code))
                .ok();
        }
    });
    let spec = write_api_spec(&dir, "MT_RECORD_BASE");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8"), "--record-missing"]);
    assert_eq!(code, 0, "records the missing trace, then passes");
    assert!(
        flowproof_cli::default_trace_path(&spec).exists(),
        "trace was recorded in place"
    );
    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("suite junit written");
    assert!(!junit.contains("<skipped/>"), "nothing skipped: {junit}");

    server_thread.join().ok();
    std::env::remove_var("MT_RECORD_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn min_version_gate_refuses_older_engine() {
    let dir = suite_dir("minver");
    std::fs::write(dir.join("suite.yaml"), "min_version: \"999.0.0\"\n").expect("manifest");
    write_api_spec(&dir, "MT_MINVER_BASE");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(code, 2, "suite demanding a future flowproof must refuse");
    std::fs::remove_dir_all(&dir).ok();
}
