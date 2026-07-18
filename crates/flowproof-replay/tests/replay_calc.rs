//! Full record→replay round trip against the mock driver: proves the
//! deterministic spine end-to-end without a Windows session.

use flowproof_agent::{record, FlowSpec};
use flowproof_driver::mock::MockAppDriver;
use flowproof_replay::{run_trace, StepStatus};

const CALC_SPEC: &str = "\
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
";

const CALC_ELEMENTS: [&str; 5] = [
    "num5Button",
    "num3Button",
    "plusButton",
    "equalButton",
    "CalculatorResults",
];

fn record_calc_trace(dir: &std::path::Path) -> std::path::PathBuf {
    let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
    let mut driver =
        MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
    let out = dir.join("calc.trace.jsonl");
    record(&spec, &mut driver, &out).expect("recording succeeds");
    out
}

#[test]
fn replay_passes_when_display_matches() {
    let dir = std::env::temp_dir().join("flowproof-replay-pass");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_calc_trace(&dir);

    let mut driver =
        MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
    let report = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(report.passed, "report: {report:?}");
    assert_eq!(report.steps.len(), 5);
    assert!(report.steps.iter().all(|s| s.status == StepStatus::Passed));
    // The four button presses were actually invoked, in order.
    assert_eq!(
        driver.invoked,
        vec!["num5Button", "plusButton", "num3Button", "equalButton"]
    );

    let result_path = report.write(&dir).expect("artifact written");
    let json: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(result_path).expect("read artifact"))
            .expect("valid JSON artifact");
    assert_eq!(json["passed"], serde_json::Value::Bool(true));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replay_fails_when_display_differs() {
    let dir = std::env::temp_dir().join("flowproof-replay-fail");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_calc_trace(&dir);

    let mut driver =
        MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 9");
    let report = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    let last = report.steps.last().expect("has steps");
    assert_eq!(last.status, StepStatus::Failed);
    assert!(
        last.detail
            .as_deref()
            .unwrap_or("")
            .contains("expected element text '8'"),
        "detail: {:?}",
        last.detail
    );

    std::fs::remove_dir_all(&dir).ok();
}

const NOTEPAD_SPEC: &str = "\
name: Write a note
app: notepad
steps:
  - Type hello from flowproof
  - assert: document contains hello
";

#[test]
fn notepad_round_trip_types_and_checks_contains() {
    let dir = std::env::temp_dir().join("flowproof-replay-notepad");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(NOTEPAD_SPEC).expect("spec parses");
    let trace = dir.join("notepad.trace.jsonl");
    let mut recorder_driver = MockAppDriver::new(&["15"]);
    record(&spec, &mut recorder_driver, &trace).expect("recording succeeds");

    // Fresh app instance: replay re-types the text, then the contains
    // assert reads what was typed.
    let mut driver = MockAppDriver::new(&["15"]);
    let report = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");
    assert_eq!(
        driver.typed,
        vec![("15".to_string(), "hello from flowproof".to_string())]
    );

    // Recording a spec whose assert can't hold is caught at record time.
    let spec_bad = FlowSpec::parse(
        "name: x\napp: notepad\nsteps:\n  - Type abc\n  - assert: document contains xyz\n",
    )
    .expect("parses");
    let trace_bad = dir.join("notepad-bad.trace.jsonl");
    let mut rec = MockAppDriver::new(&["15"]);
    let err = record(&spec_bad, &mut rec, &trace_bad).expect_err("record catches bad assert");
    assert!(err.to_string().contains("does not hold"));

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn web_round_trip_via_mock_uses_css_selectors() {
    let dir = std::env::temp_dir().join("flowproof-replay-web");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec {
        url: Some("https://example.test/greeter".into()),
        ..FlowSpec::parse(
            "name: Greet\napp: web\nurl: x\nsteps:\n  - Type Ada into the name field\n  - Press the greet button\n  - assert: page shows Hello, Ada\n",
        )
        .expect("spec parses")
    };
    let trace = dir.join("web.trace.jsonl");

    // Mock "page": elements keyed by css; body already shows the greeting so
    // the record-time assert holds.
    let mut rec =
        MockAppDriver::new(&["#name", "#greet", "body"]).with_text("body", "Greeter Hello, Ada!");
    record(&spec, &mut rec, &trace).expect("recording succeeds");
    assert_eq!(
        rec.launched.as_ref().map(|l| l.0.as_str()),
        Some("https://example.test/greeter")
    );
    assert_eq!(rec.typed, vec![("#name".to_string(), "Ada".to_string())]);
    assert_eq!(rec.invoked, vec!["#greet"]);

    let mut driver =
        MockAppDriver::new(&["#name", "#greet", "body"]).with_text("body", "Greeter Hello, Ada!");
    let report = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn replay_skips_remaining_steps_after_a_missing_element() {
    let dir = std::env::temp_dir().join("flowproof-replay-skip");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_calc_trace(&dir);

    // plusButton disappeared from the app since recording.
    let mut driver = MockAppDriver::new(&[
        "num5Button",
        "num3Button",
        "equalButton",
        "CalculatorResults",
    ]);
    let report = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    assert_eq!(report.steps[0].status, StepStatus::Passed); // Type 5
    assert_eq!(report.steps[1].status, StepStatus::Failed); // Press plus
    assert!(report.steps[2..]
        .iter()
        .all(|s| s.status == StepStatus::Skipped));

    std::fs::remove_dir_all(&dir).ok();
}
