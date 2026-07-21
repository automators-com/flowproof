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
