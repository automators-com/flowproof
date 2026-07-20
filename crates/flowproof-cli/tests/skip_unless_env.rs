//! `skip_unless_env`: first-class env-flag gating, visible as junit
//! `skipped` instead of an invisible bash guard. Unix-gated with the
//! other suite integration tests; semantics are platform-neutral.
#![cfg(unix)]

use std::path::PathBuf;

fn suite_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-skip-unless-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("suite dir");
    dir
}

fn write_gated_spec(dir: &std::path::Path, flag: &str) -> PathBuf {
    let spec = dir.join("gated.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Gated\napp: api\nskip_unless_env: [{flag}]\nsteps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n      timeout_seconds: 1\n"
        ),
    )
    .expect("spec");
    spec
}

#[test]
fn record_skips_gated_spec_without_a_trace() {
    let dir = suite_dir("record");
    let spec = write_gated_spec(&dir, "SUE_REC_FLAG");
    std::env::remove_var("SUE_REC_FLAG");

    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "a gated record is a pass, not an error");
    assert!(
        !flowproof_cli::default_trace_path(&spec).exists(),
        "nothing recorded"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn single_run_reports_skip_as_json_data() {
    let dir = suite_dir("run");
    let spec = write_gated_spec(&dir, "SUE_RUN_FLAG");
    std::env::remove_var("SUE_RUN_FLAG");

    // No trace exists; the gate must win before the missing-trace error.
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8"), "--json"]);
    assert_eq!(code, 0, "gated single run passes");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn suite_counts_gated_flow_as_junit_skipped_even_under_strict() {
    let dir = suite_dir("suite");
    write_gated_spec(&dir, "SUE_SUITE_FLAG");
    std::env::remove_var("SUE_SUITE_FLAG");

    // --strict would hard-error on the missing trace — the gate wins.
    let code = flowproof_cli::run_cli(["run", dir.to_str().expect("utf8"), "--strict"]);
    assert_eq!(code, 0, "gated flow skips even under --strict");
    let junit = std::fs::read_to_string(dir.join(".flowproof").join("suite-junit.xml"))
        .expect("suite junit written");
    assert!(
        junit.contains("<skipped message=\"required env not set: SUE_SUITE_FLAG\"/>"),
        "gate reason travels into junit: {junit}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn suite_env_can_satisfy_the_gate() {
    let dir = suite_dir("satisfied");
    // suite.yaml sets the flag — the gate is checked AFTER suite env.
    std::fs::write(dir.join("suite.yaml"), "env:\n  SUE_SAT_FLAG: \"1\"\n").expect("manifest");
    let spec = write_gated_spec(&dir, "SUE_SAT_FLAG");
    std::env::remove_var("SUE_SAT_FLAG");

    // Ungated now: record proceeds past the gate to its real (network)
    // failure — proving the suite env satisfied the gate.
    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 2, "fails on the unreachable host, not skipped");
    std::env::remove_var("SUE_SAT_FLAG");
    std::fs::remove_dir_all(&dir).ok();
}
