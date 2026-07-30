//! Falsifiability proof for the `audit --since` regression gate (issue #254).
//!
//! `audit_record_e2e.rs` proves the gate FIRES: a removed control and a
//! pass->fail change both exit non-zero. Nothing proves it can DECLINE to
//! fire. A gate wired to exit non-zero unconditionally would satisfy every
//! assertion in that file, and CI would go red on every clean run until
//! somebody stopped believing it.
//!
//! That is the shape of a false signal in a gate: not a missed regression, but
//! a gate that says "regression" so often it stops meaning anything. So the
//! property under proof here is the discriminating half --
//! `audit --since` exits ZERO when nothing regressed -- which is what makes
//! its non-zero exit worth acting on.
//!
//! Both layers, as everywhere in this suite:
//!   1. the diff CONTENT (`audit --since --json`), and
//!   2. the process EXIT CODE a CI run would branch on.
//!
//! A gate that reported an empty diff while still exiting non-zero would pass
//! a content-only check, and would still break every pipeline that uses it.

use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/audit-since")
        .join(name)
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-since-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// Install a committed record fixture as a real run record, in the layout
/// `flowproof run` would have written. The directory name is taken from the
/// record's own `run_id`, so the two can never drift apart.
fn install(dir: &Path, fixture_name: &str) -> String {
    let body = std::fs::read_to_string(fixture(fixture_name)).expect("read fixture");
    let value: serde_json::Value = serde_json::from_str(&body).expect("fixture is JSON");
    let run_id = value["run_id"].as_str().expect("run_id").to_string();
    let run_dir = dir.join(".flowproof").join("runs").join(&run_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    std::fs::write(run_dir.join("report.json"), body).expect("install record");
    run_id
}

fn audit_since(dir: &Path, base: &str) -> std::process::Output {
    std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "audit",
            dir.to_str().expect("utf8"),
            "--since",
            base,
            "--json",
        ])
        .output()
        .expect("audit --since")
}

/// Two runs with identical controls are not a regression. The gate must stay
/// silent, and it must exit zero.
#[test]
fn an_unchanged_control_set_is_not_a_regression() {
    let dir = work_dir("unchanged");
    let base = install(&dir, "base.report.json");
    install(&dir, "head-unchanged.report.json");

    let out = audit_since(&dir, &base);

    // Layer 2: the exit code CI branches on.
    assert!(
        out.status.success(),
        "an unchanged control set must exit zero, or the gate cries wolf on \
         every clean run; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Layer 1: and it must say nothing changed, rather than exiting zero while
    // reporting a diff -- which would be the same defect wearing the other face.
    let diff: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("audit --since --json is valid JSON");
    for section in ["added", "removed", "changed"] {
        assert_eq!(
            diff[section].as_array().map(Vec::len).unwrap_or(0),
            0,
            "{section} must be empty for an unchanged pair: {diff}"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Gaining a control is not a regression. It is reported, because a reviewer
/// wants to see it, but it must not fail the build: a suite that could never
/// grow without going red would teach its owners to stop adding controls.
#[test]
fn an_added_control_is_reported_but_does_not_fail_the_gate() {
    let dir = work_dir("added");
    let base = install(&dir, "base.report.json");
    install(&dir, "head-added.report.json");

    let out = audit_since(&dir, &base);

    assert!(
        out.status.success(),
        "adding a control must not fail the gate; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let diff: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("audit --since --json is valid JSON");
    assert_eq!(
        diff["added"][0]["id"], "falsifiability.since.gamma",
        "the addition is still reported: {diff}"
    );
    assert_eq!(
        diff["removed"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "nothing was removed: {diff}"
    );
    assert_eq!(
        diff["changed"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "nothing changed verdict: {diff}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
