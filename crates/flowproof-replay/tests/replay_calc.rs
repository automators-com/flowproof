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
    let (report, run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(report.passed, "report: {report:?}");
    assert_eq!(report.steps.len(), 5);
    assert!(report.steps.iter().all(|s| s.status == StepStatus::Passed));
    // The four button presses were actually invoked, in order.
    assert_eq!(
        driver.invoked,
        vec!["num5Button", "plusButton", "num3Button", "equalButton"]
    );

    let result_path = report.write_into(&run_dir).expect("artifact written");
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
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

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
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
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
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The app renamed an automation id since recording: replay must degrade
/// down the selector ladder (structural rung: control type + accessible
/// name), still pass, and report the drift instead of hiding it.
#[test]
fn renamed_automation_id_degrades_to_structural_rung_and_reports_it() {
    let dir = std::env::temp_dir().join("flowproof-replay-degraded");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_calc_trace(&dir);

    // The recorded primary selector is dead; the button still exists under
    // its accessible name (the mock matches names like UIA find-by-name).
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    std::fs::write(&trace, contents.replace("plusButton", "renamedPlusButton"))
        .expect("trace rewritten");
    let mut driver = MockAppDriver::new(&[
        "num5Button",
        "num3Button",
        "Plus",
        "equalButton",
        "CalculatorResults",
    ])
    .with_text("CalculatorResults", "Display is 8");

    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "fallback must keep the run green: {report:?}"
    );
    assert!(report.degraded, "drift must be reported");

    let plus = report
        .steps
        .iter()
        .find(|s| s.intent == "Press plus")
        .expect("plus step present");
    assert!(plus.degraded);
    assert_eq!(plus.selector_tier.as_deref(), Some("structural"));
    // The button was really pressed — via its name, not the dead id.
    assert!(driver.invoked.contains(&"Plus".to_string()));

    // Undrifted steps stay on the primary rung and unflagged.
    let five = report
        .steps
        .iter()
        .find(|s| s.intent == "Type 5")
        .expect("type step present");
    assert!(!five.degraded);
    assert_eq!(five.selector_tier.as_deref(), Some("native_id"));

    std::fs::remove_dir_all(&dir).ok();
}

/// With the structural rung gone too, the text-anchor rung is the last
/// deterministic resort — and is reported as such.
#[test]
fn text_anchor_rung_is_the_last_deterministic_resort() {
    let dir = std::env::temp_dir().join("flowproof-replay-text-anchor");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_calc_trace(&dir);

    // Strip structural rungs from the trace and kill the primary id, so
    // only the text-anchor rung can match.
    let contents = std::fs::read_to_string(&trace).expect("trace readable");
    let rewritten: Vec<String> = contents
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut value: serde_json::Value = serde_json::from_str(line).expect("trace line");
            if let Some(selectors) = value.get_mut("selectors").and_then(|s| s.as_array_mut()) {
                selectors.retain(|s| s["tier"] != "structural");
            }
            value.to_string()
        })
        .collect();
    std::fs::write(
        &trace,
        rewritten
            .join("\n")
            .replace("plusButton", "renamedPlusButton"),
    )
    .expect("trace rewritten");

    let mut driver = MockAppDriver::new(&[
        "num5Button",
        "num3Button",
        "Plus",
        "equalButton",
        "CalculatorResults",
    ])
    .with_text("CalculatorResults", "Display is 8");
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(report.passed, "report: {report:?}");
    let plus = report
        .steps
        .iter()
        .find(|s| s.intent == "Press plus")
        .expect("plus step present");
    assert!(plus.degraded);
    assert_eq!(plus.selector_tier.as_deref(), Some("text_anchor"));

    std::fs::remove_dir_all(&dir).ok();
}

/// Secret indirection: `${VAR}` in a spec resolves from the environment at
/// the moment of use — recording AND replay — while the persisted trace
/// only ever contains the reference, never the value.
#[test]
fn secret_values_never_reach_the_persisted_trace() {
    std::env::set_var("FLOWPROOF_RT_SECRET", "hunter2-super-secret");
    let dir = std::env::temp_dir().join("flowproof-replay-secret");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(
        "name: Login note\napp: notepad\nsteps:\n  - Type ${FLOWPROOF_RT_SECRET}\n  - assert: document contains ${FLOWPROOF_RT_SECRET}\n",
    )
    .expect("spec parses");
    let trace = dir.join("secret.trace.jsonl");

    let mut rec = MockAppDriver::new(&["15"]);
    record(&spec, &mut rec, &trace).expect("recording succeeds");
    // The app really received the resolved value...
    assert_eq!(
        rec.typed,
        vec![("15".to_string(), "hunter2-super-secret".to_string())]
    );
    // ...but the persisted trace only carries the reference.
    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(persisted.contains("${FLOWPROOF_RT_SECRET}"));
    assert!(
        !persisted.contains("hunter2-super-secret"),
        "secret value must never be persisted"
    );

    // Replay resolves the reference again, against a fresh app instance.
    let mut driver = MockAppDriver::new(&["15"]);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");
    assert_eq!(
        driver.typed,
        vec![("15".to_string(), "hunter2-super-secret".to_string())]
    );

    // The run report is persisted too — it must stay value-free.
    let report_json = serde_json::to_string(&report).expect("serializes");
    assert!(!report_json.contains("hunter2-super-secret"));

    std::fs::remove_dir_all(&dir).ok();
    std::env::remove_var("FLOWPROOF_RT_SECRET");
}

/// A missing secret is a hard, clearly-named error — never typing the
/// literal reference, and never leaking live app text in the failure.
#[test]
fn missing_secret_fails_closed_with_a_clear_error() {
    std::env::set_var("FLOWPROOF_RT_SECRET2", "tmp");
    let dir = std::env::temp_dir().join("flowproof-replay-secret-missing");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(
        "name: Login note\napp: notepad\nsteps:\n  - Type ${FLOWPROOF_RT_SECRET2}\n",
    )
    .expect("spec parses");
    let trace = dir.join("secret.trace.jsonl");
    record(&spec, &mut MockAppDriver::new(&["15"]), &trace).expect("recording succeeds");
    std::env::remove_var("FLOWPROOF_RT_SECRET2");

    // Replay without the variable: the step fails, nothing is typed.
    let mut driver = MockAppDriver::new(&["15"]);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(!report.passed);
    let detail = report.steps[0].detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("${FLOWPROOF_RT_SECRET2}") && detail.contains("not set"),
        "detail: {detail}"
    );
    assert!(driver.typed.is_empty(), "no literal reference typed");

    // Recording without the variable is refused outright.
    let err = record(&spec, &mut MockAppDriver::new(&["15"]), &trace)
        .expect_err("record must fail without the secret");
    assert!(err.to_string().contains("${FLOWPROOF_RT_SECRET2}"));

    std::fs::remove_dir_all(&dir).ok();
}

/// Auto-waiting assertions: a slow UI whose text only becomes right after
/// several polls still records and replays green — and the timeout travels
/// in the trace, so replay waits exactly as long as authoring allowed.
#[test]
fn assertions_wait_for_slow_uis_and_time_out_deterministically() {
    let dir = std::env::temp_dir().join("flowproof-replay-autowait");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(
        "name: Slow op\napp: web\nurl: x\nsteps:\n  - Wait until page shows Done within 5s\n",
    )
    .expect("spec parses");
    let spec = FlowSpec {
        url: Some("https://example.test/slow".into()),
        ..spec
    };
    let trace = dir.join("slow.trace.jsonl");

    // Recording: the page shows "Working…" for the first three reads.
    let mut rec = MockAppDriver::new(&["body"]).with_text("body", "Done");
    rec.text_sequence.insert(
        "body".into(),
        ["Working…", "Working…", "Working…"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    record(&spec, &mut rec, &trace).expect("recording waits out the slow op");

    // The recorded step carries the wait bound.
    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(persisted.contains("\"timeout_ms\":5000"));

    // Replay: same slow behavior, still passes.
    let mut driver = MockAppDriver::new(&["body"]).with_text("body", "Done");
    driver.text_sequence.insert(
        "body".into(),
        ["Working…", "Working…"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");

    // A page that NEVER shows the text fails after the bounded wait —
    // deterministically, with the real text in the failure detail.
    let mut driver = MockAppDriver::new(&["body"]).with_text("body", "Working…");
    let started = std::time::Instant::now();
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(!report.passed);
    assert!(started.elapsed() >= std::time::Duration::from_secs(5));
    assert!(report.steps[0]
        .detail
        .as_deref()
        .unwrap_or_default()
        .contains("Working…"));

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
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    assert_eq!(report.steps[0].status, StepStatus::Passed); // Type 5
    assert_eq!(report.steps[1].status, StepStatus::Failed); // Press plus
    assert!(report.steps[2..]
        .iter()
        .all(|s| s.status == StepStatus::Skipped));

    std::fs::remove_dir_all(&dir).ok();
}
