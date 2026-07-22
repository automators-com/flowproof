//! Field regression (cypress-realworld-app, flowproof 0.2.3): one flow's
//! driver fault aborted the ENTIRE suite run. Flow 1 passed, flow 2 hit a
//! dead CDP socket, and the remaining six flows never ran - no merged
//! junit, nothing for CI to ingest. A broken flow is one flow's problem.
//! Unix-only for the same reason as the other suite tests (sh-backed
//! helpers); the semantics under test are platform-neutral.
#![cfg(unix)]

use std::path::PathBuf;

fn suite_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-flow-isolation-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("suite dir");
    dir
}

/// An `app: api` flow that needs no browser: recordable and replayable on
/// any OS, so the suite semantics can be tested without a live app.
fn write_passing_flow(dir: &std::path::Path, stem: &str, base_var: &str) -> PathBuf {
    let spec = dir.join(format!("{stem}.flow.yaml"));
    std::fs::write(
        &spec,
        format!(
            "name: {stem}\napp: api\nsteps:\n  - assert_api:\n      request: GET ${{{base_var}}}/health\n      status: 200\n"
        ),
    )
    .expect("spec");
    spec
}

/// A local HTTP server answering 200 on any path, so an `app: api` flow
/// can actually record. Returns its base URL; the thread ends with the
/// process.
fn serve_ok() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn a_broken_flow_errors_alone_and_the_suite_still_writes_junit() {
    let dir = suite_dir("broken");

    // Flow A parses and is traceless (skipped). Flow B is unparseable:
    // before this fix, its parse error aborted the run before any junit
    // was written, taking flow C down with it.
    write_passing_flow(&dir, "a-first", "ISO_BASE");
    std::fs::write(
        dir.join("b-broken.flow.yaml"),
        "name: Broken\napp: api\nsteps:\n  - assert_api:\n      no_such_field: nope\n",
    )
    .expect("spec");
    write_passing_flow(&dir, "c-last", "ISO_BASE");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(code, 2, "a suite containing an errored flow exits 2");

    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("the suite still writes a merged junit when a flow errors");

    // The decisive assertion: the flow AFTER the broken one still ran.
    assert!(
        junit.contains("c-last"),
        "flows after the broken one must still run: {junit}"
    );
    // Errored is a third outcome, distinct from failed.
    assert!(junit.contains("<error message="), "junit: {junit}");
    assert!(junit.contains("errors=\"1\""), "junit: {junit}");
    // All three flows are represented.
    assert_eq!(
        junit.matches("<testsuite name=").count(),
        3,
        "junit: {junit}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_failing_before_each_hook_errors_that_flow_not_the_run() {
    let dir = suite_dir("hook");
    write_passing_flow(&dir, "a-first", "HOOK_BASE");
    write_passing_flow(&dir, "b-second", "HOOK_BASE");
    // The hook's own comment promised this; `run_hook(...)?` did the
    // opposite and aborted the whole suite.
    std::fs::write(dir.join("suite.yaml"), "before_each: exit 3\n").expect("manifest");

    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8"), "--record-missing"]);
    assert_eq!(code, 2, "hook failures error their flow");

    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("suite junit written despite the failing hook");
    assert_eq!(
        junit.matches("<testsuite name=").count(),
        2,
        "junit: {junit}"
    );
    assert!(junit.contains("errors=\"2\""), "junit: {junit}");

    std::fs::remove_dir_all(&dir).ok();
}

/// GAP-R, found in the field against released 0.3.0: running ONE spec did
/// not run the suite's `before_each`, so a second consecutive run failed
/// on state the first had left behind while the whole suite passed.
///
/// That is the worst shape for a bug: it appears exactly when someone
/// isolates a spec to debug it, and the CLI prints "using suite context
/// from ...suite.yaml" either way, which reads as a promise that the
/// manifest applies.
#[test]
fn a_single_spec_run_honors_the_suites_hooks() {
    let dir = suite_dir("single-hooks");
    let spec = write_passing_flow(&dir, "only", "HOOKS_BASE");
    std::env::set_var("HOOKS_BASE", serve_ok());
    let before = dir.join("before.marker");
    let after = dir.join("after.marker");
    std::fs::write(
        dir.join("suite.yaml"),
        format!(
            "before_each: touch {}\nafter_each: touch {}\n",
            before.display(),
            after.display()
        ),
    )
    .expect("manifest");

    // Record so there is a trace to replay, then run the single spec.
    flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    std::fs::remove_file(&before).ok();
    std::fs::remove_file(&after).ok();
    flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);

    assert!(
        before.exists(),
        "before_each must run for a single spec, not only for a suite"
    );
    assert!(after.exists(), "after_each must run too");

    std::fs::remove_dir_all(&dir).ok();
}

/// `after_each` runs even when the flow fails, for the reason it exists:
/// a left-behind fixture hurts most exactly when something went wrong.
#[test]
fn a_single_spec_run_cleans_up_after_a_failing_flow() {
    let dir = suite_dir("single-cleanup");
    let spec = write_passing_flow(&dir, "only", "CLEANUP_BASE");
    std::env::set_var("CLEANUP_BASE", serve_ok());
    let after = dir.join("after.marker");
    std::fs::write(
        dir.join("suite.yaml"),
        format!("after_each: touch {}\n", after.display()),
    )
    .expect("manifest");

    flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    std::fs::remove_file(&after).ok();
    // Point the flow at a port nothing answers so replay fails.
    std::env::set_var("CLEANUP_BASE", "http://127.0.0.1:9");
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_ne!(code, 0, "the flow must fail for this test to mean anything");
    assert!(
        after.exists(),
        "after_each must run even when the flow fails"
    );

    std::fs::remove_dir_all(&dir).ok();
}
