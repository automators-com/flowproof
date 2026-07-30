//! Falsifiability proof for the `capability-error` regression rule (#263).
//!
//! `is_regression()` used to fire only on a removed control or one that turned
//! `fail`. A control going `pass` -> `capability-error` exited ZERO, so a
//! control that silently stopped being certifiable — a runner without seccomp,
//! a driver that lost a capability, a flow that stopped running at all — passed
//! the gate clean.
//!
//! That is a false green in the only artifact the evidence positioning rests
//! on. `capability-error` exists precisely so that "we could not check this"
//! never reads as "this is fine"; the rest of the codebase is emphatic about it
//! (`assert_no_egress` fails as a capability error on a host that cannot
//! enforce it, rather than passing vacuously). At the diff layer that
//! distinction had been lost.
//!
//! The founder's decision (#263) makes it a regression. The accepted cost is
//! recorded on the issue and is not revisited here: an unchanged suite run on a
//! host that cannot enforce a control now fails the gate. The remedy is to fix
//! the host or narrow the control, never to soften the rule.
//!
//! Two layers, as everywhere in this suite: the diff CONTENT names the control
//! and its old and new verdicts, and the EXIT CODE is non-zero.

use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/audit-since")
        .join(name)
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-cap-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// Install a committed record fixture in the layout `flowproof run` writes,
/// with the directory named from the record's own `run_id` so the two cannot
/// drift apart.
fn install(dir: &Path, fixture_name: &str) -> String {
    let body = std::fs::read_to_string(fixture(fixture_name)).expect("read fixture");
    let value: serde_json::Value = serde_json::from_str(&body).expect("fixture is JSON");
    let run_id = value["run_id"].as_str().expect("run_id").to_string();
    let run_dir = dir.join(".flowproof").join("runs").join(&run_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(run_dir.join("report.json"), body).expect("install record");
    run_id
}

/// A control that stops being certifiable is a regression, not a pass.
#[test]
fn a_control_turning_capability_error_fails_the_gate() {
    let dir = work_dir("regress");
    let base = install(&dir, "base.report.json");
    install(&dir, "head-capability-error.report.json");

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "audit",
            dir.to_str().expect("utf8"),
            "--since",
            &base,
            "--json",
        ])
        .output()
        .expect("audit --since");

    // Layer 2: the exit code CI branches on.
    assert!(
        !out.status.success(),
        "a control that can no longer be certified must fail the gate; exiting \
         zero here would let coverage evaporate without anyone failing a build. \
         stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // A usage or parse error also exits non-zero, and would sail through the
    // check above while proving nothing -- the gate would never have run. This
    // happened while writing this test: an invalid fixture made `audit` exit 2
    // before evaluating anything, and the exit-code assertion passed for
    // entirely the wrong reason. So the regression exit must be distinguished
    // from an error exit explicitly.
    assert!(
        !out.stdout.is_empty(),
        "audit produced no diff, so it errored rather than judged -- a non-zero \
         exit here proves nothing about the rule under test. stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Layer 1: and the diff must say which control, and what it became --
    // a non-zero exit that did not name the control would be unactionable.
    let diff: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("audit --since --json is valid JSON");
    let changed = diff["changed"]
        .as_array()
        .expect("changed array")
        .iter()
        .find(|c| c["id"] == "falsifiability.since.beta")
        .unwrap_or_else(|| panic!("the diff names the control: {diff}"));
    assert_eq!(changed["old"], "pass", "it was passing: {diff}");
    assert_eq!(
        changed["new"], "capability-error",
        "and is now uncertifiable: {diff}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
