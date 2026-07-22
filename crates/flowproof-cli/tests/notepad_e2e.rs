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

/// #66 and #68 on real Windows: drive an ARBITRARY program through the
/// `app: {command, window_title}` mapping, with its window pinned to a
/// size. This is the merge gate for that work - the grammar, trace shape
/// and replay semantics are covered by mock-driver tests, but a Windows
/// feature verified only against a mock is not verified at all, so this
/// exercises the real UIA path against a real process.
#[test]
fn drives_an_arbitrary_app_through_the_mapping_form_with_pinned_geometry() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping windows mapping E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    kill_notepad();

    let dir = std::env::temp_dir().join("flowproof-windows-mapping-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    // `${VAR}` in both fields, so the test also proves references resolve
    // at launch and are stored raw rather than baked into the trace.
    std::env::set_var("FLOWPROOF_E2E_APP", "notepad.exe");
    std::env::set_var("FLOWPROOF_E2E_WINDOW", "Notepad");
    // `page shows` on purpose: it is the SHARED surface assertion, and an
    // app the spec has never described has no "document". `document
    // contains` belongs to the notepad rules, which can hardcode the editor
    // id; here the assertion reads the foreground window's whole subtree.
    let spec = FlowSpec::parse(
        "name: Arbitrary Windows app\n\
         app:\n  command: ${FLOWPROOF_E2E_APP}\n  window_title: ${FLOWPROOF_E2E_WINDOW}\n\
         window:\n  width: 900\n  height: 640\n\
         steps:\n  - Type flowproof drove this\n  - assert: page shows flowproof drove this\n",
    )
    .expect("spec parses");
    assert_eq!(spec.app.id(), "windows");
    let trace = dir.join("mapping.trace.jsonl");

    let mut driver = UiaAppDriver::new().expect("UIA client");
    flowproof_agent::record(&spec, &mut driver, &trace).expect("records against a real Notepad");
    drop(driver);

    // The trace keeps the REFERENCES, not the resolved values, and pins the
    // geometry that was actually applied.
    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        persisted.contains("${FLOWPROOF_E2E_APP}"),
        "the command must travel as a reference: {persisted}"
    );
    assert!(
        !persisted.contains("notepad.exe"),
        "the resolved command must not enter the trace: {persisted}"
    );
    assert!(persisted.contains("\"geometry\""), "{persisted}");

    // Replay reproduces the same window shape and re-drives the app.
    kill_notepad();
    let mut driver = UiaAppDriver::new().expect("UIA client");
    let (report, _run_dir) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "mapping-form replay must pass: {report:#?}");

    // The window really is the size the trace pinned. Windows may adjust
    // for DPI or minimum size, so allow a small tolerance rather than
    // asserting an exact match that would flake on a different runner.
    let window = flowproof_driver::window::find_window("Notepad")
        .expect("window lookup")
        .expect("notepad is open");
    let (_, _, width, height) = window.rect;
    assert!(
        width.abs_diff(900) <= 40 && height.abs_diff(640) <= 40,
        "window should be about 900x640, got {width}x{height}"
    );

    kill_notepad();
    std::env::remove_var("FLOWPROOF_E2E_APP");
    std::env::remove_var("FLOWPROOF_E2E_WINDOW");
    std::fs::remove_dir_all(&dir).ok();
}
