//! End-to-end: actually drives Notepad through record and replay.
//! Windows-only and opt-in via FLOWPROOF_E2E=1. Unlike the Calculator E2E,
//! this one RUNS IN CI: notepad.exe ships on the Windows Server images that
//! GitHub's windows-latest runners use.

#![cfg(windows)]

use flowproof_agent::FlowSpec;
use flowproof_driver::UiaAppDriver;

const NOTEPAD_SPEC: &str = include_str!("../../../examples/notepad.flow.yaml");

/// Kill any notepad instance so each phase starts from a fresh, empty
/// document and unsaved-changes prompts never appear.
fn kill_notepad() {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "notepad.exe"])
        .output();
    std::thread::sleep(std::time::Duration::from_millis(500));
}

#[test]
fn records_and_replays_notepad() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping notepad E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-notepad-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("notepad.trace.jsonl");

    let spec = FlowSpec::parse(NOTEPAD_SPEC).expect("example spec parses");

    kill_notepad();
    let record_result = (|| {
        let mut driver = UiaAppDriver::new()?;
        flowproof_agent::record(&spec, &mut driver, &trace_path)
            .map_err(|e| flowproof_driver::DriverError::Uia(format!("record failed: {e}")))
    })();
    kill_notepad();
    let summary = record_result.expect("recording succeeds");
    assert_eq!(summary.steps, 2);

    let replay_result = (|| {
        let mut driver = UiaAppDriver::new()?;
        flowproof_replay::run_trace(&trace_path, &mut driver)
            .map(|(report, _)| report)
            .map_err(|e| flowproof_driver::DriverError::Uia(format!("replay failed: {e}")))
    })();
    kill_notepad();
    let report = replay_result.expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "notepad flow must pass: {report:#?}");
    assert!(
        !report.degraded,
        "primary selectors must match: {report:#?}"
    );
    // GDI keyframe capture on the runner: the run must carry its recording.
    let recording = report.recording.as_ref().expect("run is recorded via GDI");
    assert_eq!(recording.steps.len(), report.steps.len());

    // Ladder fallback against REAL UIA: kill the recorded automation id (as
    // if the app renamed its control) — replay must still pass by matching
    // the editor's structural rung (control type + name), and report the
    // drift as degraded.
    let contents = std::fs::read_to_string(&trace_path).expect("trace readable");
    std::fs::write(
        &trace_path,
        contents.replace("\"automation_id\":\"15\"", "\"automation_id\":\"99999\""),
    )
    .expect("trace rewritten");
    let degraded_result = (|| {
        let mut driver = UiaAppDriver::new()?;
        flowproof_replay::run_trace(&trace_path, &mut driver)
            .map(|(report, _)| report)
            .map_err(|e| flowproof_driver::DriverError::Uia(format!("replay failed: {e}")))
    })();
    kill_notepad();
    let report = degraded_result.expect("degraded replay runs");
    assert!(
        report.passed,
        "fallback must keep the run green: {report:#?}"
    );
    assert!(report.degraded, "drift must be reported: {report:#?}");
    let typed = report
        .steps
        .iter()
        .find(|s| s.intent.starts_with("Type"))
        .expect("type step present");
    assert_eq!(typed.selector_tier.as_deref(), Some("structural"));

    std::fs::remove_dir_all(&dir).ok();
}
