//! `flowproof run` writes a persisted run record; `flowproof audit` READS it
//! (never re-replays); `--since` diffs two records. Driver-free and
//! platform-neutral: the run flows here are `app: api` gated by an unset env
//! var, so they skip without launching anything, and the audit assertions
//! read the record the skip wrote. A skip still records the flow, so its
//! control reads as capability-error (it never ran) - which also proves audit
//! is reading the record, because there is no trace to re-replay.

use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-audit-rec-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// A control-bearing `app: api` flow gated by an unset env var: `run` skips
/// it (the gate wins over the missing trace), and the skip is recorded.
fn write_gated_control_spec(dir: &Path, flag: &str, control_id: &str) -> PathBuf {
    let spec = dir.join("gated.flow.yaml");
    std::fs::write(
        &spec,
        format!(
            "name: Gated control\napp: api\nskip_unless_env: [{flag}]\n\
             control:\n  id: {control_id}\n  title: A gated control\n\
             steps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n\
             \x20     timeout_seconds: 1\n"
        ),
    )
    .expect("spec");
    spec
}

/// The single `report.json` written under `<dir>/.flowproof/runs`.
fn find_record(dir: &Path) -> Option<PathBuf> {
    let runs = dir.join(".flowproof").join("runs");
    let entries = std::fs::read_dir(&runs).ok()?;
    for entry in entries.filter_map(Result::ok) {
        let candidate = entry.path().join("report.json");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Write a run record fixture directly, bypassing `run`, so a diff test can
/// pin exact verdicts across two runs.
fn write_record_fixture(dir: &Path, run_id: &str, flows: serde_json::Value) {
    let run_dir = dir.join(".flowproof").join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).expect("run dir");
    let record = serde_json::json!({
        "run_id": run_id,
        "started_at": "2026-07-24T11:06:18Z",
        "flowproof_version": "0.4.1",
        "env": { "os": "linux" },
        "flows": flows,
    });
    std::fs::write(
        run_dir.join("report.json"),
        serde_json::to_string_pretty(&record).expect("record json"),
    )
    .expect("write record");
}

fn control_flow(flow: &str, id: &str, verdict: &str) -> serde_json::Value {
    serde_json::json!({
        "flow": flow,
        "status": "pass",
        "control": {
            "id": id,
            "verdict": verdict,
            "evidence": { "trace": format!("{flow}.trace.jsonl") },
        }
    })
}

/// `run` writes a record; `audit` renders the control map FROM that record,
/// with no trace present to re-replay.
#[test]
fn run_writes_a_record_and_audit_reads_it_without_replay() {
    let dir = work_dir("read");
    let spec = write_gated_control_spec(&dir, "AUDIT_REC_FLAG_UNSET", "audit.rec.gated");
    std::env::remove_var("AUDIT_REC_FLAG_UNSET");

    // A gated single-spec run skips the flow, and the skip is recorded.
    let code = flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]);
    assert_eq!(code, 0, "a gated run is a pass, not an error");

    let record = find_record(&dir).expect("run wrote a report.json");
    let body = std::fs::read_to_string(&record).expect("read record");
    assert!(
        body.contains("audit.rec.gated"),
        "the record folds the flow's control: {body}"
    );
    // There is deliberately NO trace on disk, so audit cannot be re-replaying.
    assert!(
        !flowproof_cli::default_trace_path(&spec).exists(),
        "no trace was ever recorded"
    );

    // Audit as YAML reads the record and renders the control map.
    let yaml = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8")])
        .output()
        .expect("audit yaml");
    assert!(yaml.status.success(), "audit exits clean");
    let out = String::from_utf8_lossy(&yaml.stdout);
    assert!(out.contains("audit.rec.gated"), "names the control: {out}");
    // A skipped flow never ran, so its control is capability-error - proof the
    // verdict came from the record, not a fresh (impossible) replay.
    assert!(
        out.contains("verdict: capability-error"),
        "a never-run control is capability-error: {out}"
    );

    // Audit as JSON is the same map.
    let json = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8"), "--json"])
        .output()
        .expect("audit json");
    assert!(json.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("audit --json is valid JSON");
    assert_eq!(value["controls"][0]["id"], "audit.rec.gated");
    assert_eq!(value["controls"][0]["verdict"], "capability-error");

    std::fs::remove_dir_all(&dir).ok();
}

/// With no run recorded, audit refuses with a clear error pointing at `run` -
/// it never silently re-replays.
#[test]
fn audit_without_a_record_tells_you_to_run_first() {
    let dir = work_dir("norecord");
    let out = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8")])
        .output()
        .expect("audit");
    assert!(!out.status.success(), "audit fails when nothing has run");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no run record") && stderr.contains("flowproof run"),
        "the error points at run: {stderr}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// `--since` diffs the latest record against an earlier one by control id:
/// added, removed, and verdict-changed controls, with the right exit code.
#[test]
fn audit_since_diffs_added_removed_and_verdict_changed() {
    let dir = work_dir("diff");
    // Older run: `keep.same` and `regressed` both pass, plus `dropped`.
    write_record_fixture(
        &dir,
        "2026-07-24T10-00-00Z-0001",
        serde_json::json!([
            control_flow("flows/a.flow.yaml", "keep.same", "pass"),
            control_flow("flows/b.flow.yaml", "regressed", "pass"),
            control_flow("flows/c.flow.yaml", "dropped", "pass"),
        ]),
    );
    // Newer run: `regressed` now fails, `dropped` is gone, `brand.new` appears.
    write_record_fixture(
        &dir,
        "2026-07-24T11-00-00Z-0002",
        serde_json::json!([
            control_flow("flows/a.flow.yaml", "keep.same", "pass"),
            control_flow("flows/b.flow.yaml", "regressed", "fail"),
            control_flow("flows/d.flow.yaml", "brand.new", "pass"),
        ]),
    );

    let out = std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "audit",
            dir.to_str().expect("utf8"),
            "--since",
            "2026-07-24T10-00-00Z-0001",
            "--json",
        ])
        .output()
        .expect("audit --since");
    // A removed control and a fail regression are both CI-failing.
    assert!(
        !out.status.success(),
        "a regression exits non-zero: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let diff: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("diff --json is valid JSON");
    assert_eq!(diff["base"], "2026-07-24T10-00-00Z-0001");
    assert_eq!(diff["head"], "2026-07-24T11-00-00Z-0002");
    assert_eq!(diff["added"][0]["id"], "brand.new");
    assert_eq!(diff["removed"][0]["id"], "dropped");
    assert_eq!(diff["changed"][0]["id"], "regressed");
    assert_eq!(diff["changed"][0]["old"], "pass");
    assert_eq!(diff["changed"][0]["new"], "fail");

    // The YAML form renders the same three sections.
    let yaml = std::process::Command::new(FLOWPROOF_BIN)
        .args([
            "audit",
            dir.to_str().expect("utf8"),
            "--since",
            "2026-07-24T10-00-00Z-0001",
        ])
        .output()
        .expect("audit --since yaml");
    let text = String::from_utf8_lossy(&yaml.stdout);
    assert!(text.contains("added:") && text.contains("removed:") && text.contains("changed:"));

    std::fs::remove_dir_all(&dir).ok();
}
