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

/// Record a one-press web trace and shrink its recorded existence timeout
/// so gate-failure tests spend milliseconds, not the 5s default.
fn record_press_trace(dir: &std::path::Path, label: &str, timeout_ms: u64) -> std::path::PathBuf {
    let spec = FlowSpec::parse(&format!(
        "name: Press flow\napp: web\nurl: https://e.test/x\nsteps:\n  - Press the \"{label}\" button\n",
    ))
    .expect("spec parses");
    let trace = dir.join("press.trace.jsonl");
    let mut rec = MockAppDriver::new(&[label]);
    record(&spec, &mut rec, &trace).expect("records");
    let shrunk = std::fs::read_to_string(&trace)
        .expect("trace readable")
        .replace(
            "\"timeout_ms\":5000",
            &format!("\"timeout_ms\":{timeout_ms}"),
        );
    std::fs::write(&trace, shrunk).expect("trace rewritten");
    trace
}

/// Mock rules travel spec → trace header → replay staging: what was
/// mocked at record is mocked at replay, with one shared conversion.
#[test]
fn mock_rules_travel_from_spec_through_trace_to_replay_staging() {
    let dir = std::env::temp_dir().join("flowproof-replay-mocks");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(
        "name: Mocked\napp: web\nurl: https://e.test/x\nmock:\n  - url_contains: /api/rates\n    method: get\n    status: 200\n    body:\n      rate: 1.23\nsteps:\n  - Press the \"Go\" button\n",
    )
    .expect("spec parses");
    let trace = dir.join("mocked.trace.jsonl");
    let mut rec = MockAppDriver::new(&["Go"]);
    record(&spec, &mut rec, &trace).expect("records");
    // Record-time staging happened...
    assert_eq!(rec.staged_mocks.len(), 1);
    assert_eq!(rec.staged_mocks[0].url_contains, "/api/rates");
    assert_eq!(rec.staged_mocks[0].content_type, "application/json");
    // ...and the rules landed in the header.
    let header_line = std::fs::read_to_string(&trace)
        .expect("trace readable")
        .lines()
        .next()
        .map(str::to_string)
        .expect("header");
    assert!(header_line.contains("\"url_contains\":\"/api/rates\""));

    // Replay stages the same mocks from the header alone.
    let mut driver = MockAppDriver::new(&["Go"]);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "{report:#?}");
    assert_eq!(driver.staged_mocks.len(), 1);
    assert_eq!(driver.staged_mocks[0].body, br#"{"rate":1.23}"#);
    std::fs::remove_dir_all(&dir).ok();
}

/// Round-2 input capabilities replay deterministically: upload sets the
/// file (skipping actionability — file inputs are conventionally hidden),
/// right-click opens the context menu through the actionability gate, and
/// a portable `Mod` chord resolves per-OS at press time.
#[test]
fn upload_right_click_and_portable_modifier_replay() {
    let dir = std::env::temp_dir().join("flowproof-replay-upload");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec = FlowSpec::parse(
        "name: Import\napp: web\nurl: https://e.test/x\nsteps:\n  - Upload fixtures/data.qif into the \"Import file\" field\n  - Right-click \"Accounts\"\n  - Press Mod+K\n",
    )
    .expect("spec parses");
    let trace = dir.join("import.trace.jsonl");
    let mut rec = MockAppDriver::new(&["Import file", "Accounts"]);
    record(&spec, &mut rec, &trace).expect("records");

    let mut driver = MockAppDriver::new(&["Import file", "Accounts"]);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "{report:#?}");
    assert_eq!(
        driver.uploads,
        vec![("Import file".to_string(), "fixtures/data.qif".to_string())]
    );
    assert_eq!(driver.context_clicked, vec!["Accounts"]);
    let expected_chord = if cfg!(target_os = "macos") {
        "Meta+k"
    } else {
        "Ctrl+k"
    };
    assert_eq!(driver.keys_pressed, vec![expected_chord]);
    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #42 gate 1: an element that exists but is disabled must not be
/// clicked — and the failure must NAME the gate.
#[test]
fn disabled_element_blocks_the_click_and_names_the_gate() {
    let dir = std::env::temp_dir().join("flowproof-actionable-disabled");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_press_trace(&dir, "Save", 400);

    let mut driver = MockAppDriver::new(&["Save"]);
    driver.disabled = vec!["Save".into()];
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    let detail = report.steps[0].detail.clone().unwrap_or_default();
    assert!(
        detail.contains("exists but is disabled after 400ms"),
        "gate named: {detail}"
    );
    assert!(driver.invoked.is_empty(), "the click must not have fired");
    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #42 gate 2: a mid-animation element is waited out — the click
/// lands once the bounding box settles.
#[test]
fn animation_settles_then_the_click_lands() {
    let dir = std::env::temp_dir().join("flowproof-actionable-moving");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_press_trace(&dir, "Save", 5000);

    let mut driver = MockAppDriver::new(&["Save"]);
    driver.moving.insert("Save".into(), 3);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(
        report.passed,
        "settled animation must not fail: {report:#?}"
    );
    assert_eq!(driver.invoked, vec!["Save"]);
    std::fs::remove_dir_all(&dir).ok();
}

/// Issue #42 gate 3: an element whose center a click would not reach
/// (toast/overlay) is not clicked blind.
#[test]
fn obscured_element_blocks_the_click_and_names_the_gate() {
    let dir = std::env::temp_dir().join("flowproof-actionable-obscured");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace = record_press_trace(&dir, "Save", 400);

    let mut driver = MockAppDriver::new(&["Save"]);
    driver.obscured = vec!["Save".into()];
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    let detail = report.steps[0].detail.clone().unwrap_or_default();
    assert!(
        detail.contains("exists but is obscured"),
        "gate named: {detail}"
    );
    assert!(driver.invoked.is_empty(), "the click must not have fired");
    std::fs::remove_dir_all(&dir).ok();
}

/// A failing step captures the debug bundle into the run dir and suggests
/// the nearest live text anchor — the two questions a human asks first,
/// answered without a re-run.
#[test]
fn failure_writes_debug_bundle_and_suggests_nearest_anchor() {
    let dir = std::env::temp_dir().join("flowproof-replay-debug-bundle");
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Record a web flow pressing "Save change" against a mock that has it.
    let spec = FlowSpec::parse(
        "name: Save flow\napp: web\nurl: https://e.test/x\nsteps:\n  - Press the \"Save change\" button\n",
    )
    .expect("spec parses");
    let trace = dir.join("save.trace.jsonl");
    let mut rec = MockAppDriver::new(&["Save change"]);
    record(&spec, &mut rec, &trace).expect("records");

    // Replay against a drifted app: the button is now "Save changes".
    let mut driver = MockAppDriver::new(&["other"]);
    driver.scene = Some(
        r#"[{"target":"css:#s","tag":"button","label":"Save changes"},
            {"target":"css:#d","tag":"button","label":"Delete"}]"#
            .into(),
    );
    driver.debug = Some(flowproof_driver::DebugBundle {
        dom_html: Some("<html><body>drifted</body></html>".into()),
        console: vec!["[error] boom".into()],
    });
    let (report, run_dir) = run_trace(&trace, &mut driver).expect("replay runs");

    assert!(!report.passed);
    let detail = report.steps[0].detail.clone().unwrap_or_default();
    assert!(
        detail.contains("did you mean 'Save changes'"),
        "nearest-anchor hint present: {detail}"
    );
    assert!(
        detail.contains("debug/dom.html") && detail.contains("debug/console.log"),
        "capture note present: {detail}"
    );
    let dom = std::fs::read_to_string(run_dir.join("debug/dom.html")).expect("dom written");
    assert!(dom.contains("drifted"));
    let console = std::fs::read_to_string(run_dir.join("debug/console.log")).expect("log written");
    assert!(console.contains("[error] boom"));

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
    let mut rec = MockAppDriver::new(&["name", "greet"]).with_surface_text("Greeter Hello, Ada!");
    record(&spec, &mut rec, &trace).expect("recording succeeds");
    assert_eq!(
        rec.launched.as_ref().map(|l| l.0.as_str()),
        Some("https://example.test/greeter")
    );
    assert_eq!(rec.typed, vec![("name".to_string(), "Ada".to_string())]);
    assert_eq!(rec.invoked, vec!["greet"]);

    let mut driver =
        MockAppDriver::new(&["name", "greet"]).with_surface_text("Greeter Hello, Ada!");
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
    let mut rec = MockAppDriver::new(&[]).with_surface_text("Done");
    rec.text_sequence.insert(
        MockAppDriver::SURFACE.into(),
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
    let mut driver = MockAppDriver::new(&[]).with_surface_text("Done");
    driver.text_sequence.insert(
        MockAppDriver::SURFACE.into(),
        ["Working…", "Working…"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:?}");

    // A page that NEVER shows the text fails after the bounded wait —
    // deterministically, with the real text in the failure detail.
    let mut driver = MockAppDriver::new(&[]).with_surface_text("Working…");
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

#[test]
fn keyboard_clear_and_focused_typing_replay_deterministically() {
    let dir = std::env::temp_dir().join("flowproof-replay-keyboard");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let spec = FlowSpec::parse(
        "name: Keyboard flow
app: web
url: https://e.test/x
steps:
  - Type old into the \"Template name\" field
  - Clear the \"Template name\" field
  - Type fresh
  - Press Enter
  - Press Alt+Shift+Backspace
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&["Template name"]);
    let trace = dir.join("keyboard.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("recording succeeds");

    let mut driver = MockAppDriver::new(&["Template name"]);
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "keyboard flow replays: {report:?}");
    assert!(!report.degraded);
    // Replay re-performed each action through the driver surface.
    assert_eq!(driver.cleared, vec!["Template name"]);
    assert_eq!(driver.typed_focused, vec!["fresh"]);
    assert_eq!(
        driver.keys_pressed,
        vec!["Enter", "Alt+Shift+Backspace"],
        "chords replayed with their modifiers"
    );
    // The clear left the field empty (replace semantics, not append).
    assert_eq!(
        driver.texts.get("Template name").map(String::as_str),
        Some("")
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn negative_count_value_and_presence_asserts_replay() {
    let dir = std::env::temp_dir().join("flowproof-replay-assertions");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let spec = FlowSpec::parse(
        "name: Assertion forms
app: web
url: https://e.test/x
steps:
  - Type Street into the \"Field Name\" field
  - assert: the \"Field Name\" field contains Street
  - assert: page does not show Deleted item
  - assert: page shows row 2 times
  - assert: the \"Field Name\" is visible
  - assert: the \"Ghost element\" is not visible within 1s
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&["Field Name"]).with_surface_text("row one, row two");
    let trace = dir.join("assertions.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("recording succeeds");

    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(persisted.contains("\"value_not_contains\":\"Deleted item\""));
    assert!(persisted.contains("\"count\":2"));
    assert!(persisted.contains("\"element_present\":true"));
    assert!(persisted.contains("\"element_present\":false"));

    let mut driver = MockAppDriver::new(&["Field Name"]).with_surface_text("row one, row two");
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "assertion forms replay: {report:?}");

    // A count mismatch fails deterministically after its timeout.
    let spec = FlowSpec::parse(
        "name: Count mismatch
app: web
url: https://e.test/x
steps:
  - assert: page shows row 3 times within 1s
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]).with_surface_text("row one, row two");
    let trace2 = dir.join("count-mismatch.trace.jsonl");
    let err = record(&spec, &mut driver, &trace2).expect_err("count mismatch must fail");
    assert!(err.to_string().contains("row"), "err: {err}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn negative_assert_waits_for_text_to_disappear() {
    let dir = std::env::temp_dir().join("flowproof-replay-negative-wait");
    std::fs::create_dir_all(&dir).expect("temp dir");

    // The page still shows the row right after the delete click; it
    // disappears two polls later — the negative assert must wait it out.
    let spec = FlowSpec::parse(
        "name: Delete waits
app: web
url: https://e.test/x
steps:
  - assert: page does not show TestConnection within 5s
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]).with_surface_text("list");
    driver.text_sequence.insert(
        MockAppDriver::SURFACE.into(),
        ["TestConnection", "TestConnection", "list"]
            .into_iter()
            .map(String::from)
            .collect(),
    );
    let trace = dir.join("negative.trace.jsonl");
    let started = std::time::Instant::now();
    record(&spec, &mut driver, &trace).expect("recording succeeds after the text disappears");
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(400),
        "the assert polled at least twice"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn session_and_navigation_record_and_replay() {
    let dir = std::env::temp_dir().join("flowproof-replay-session-nav");
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::env::set_var("FLOWPROOF_TEST_SESSION_JWT", "jwt-value-e2e");
    std::env::set_var("FLOWPROOF_TEST_BASE", "https://app.test");

    let spec = FlowSpec::parse(
        "name: Authenticated flow
app: web
url: ${FLOWPROOF_TEST_BASE}/templates
session:
  cookies:
    - name: automators.session
      value: ${FLOWPROOF_TEST_SESSION_JWT}
  local_storage:
    projectId: p-123
steps:
  - Go to /settings
  - Reload the page
  - assert: page shows Settings
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]).with_surface_text("Settings");
    let trace = dir.join("session.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("recording succeeds");

    // The driver received RESOLVED session values and navigations…
    let staged = driver.staged_session.as_ref().expect("session staged");
    assert_eq!(
        staged.cookies,
        vec![(
            "automators.session".to_string(),
            "jwt-value-e2e".to_string(),
            None
        )]
    );
    assert_eq!(
        staged.local_storage,
        vec![("projectId".to_string(), "p-123".to_string())]
    );
    assert_eq!(
        driver.launched.as_ref().map(|l| l.0.as_str()),
        Some("https://app.test/templates"),
        "launch URL resolved from the environment"
    );
    assert_eq!(driver.navigations, vec!["https://app.test/settings"]);
    assert_eq!(driver.reloads, 1);

    // …while the trace keeps the references, never the values.
    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(persisted.contains("${FLOWPROOF_TEST_SESSION_JWT}"));
    assert!(!persisted.contains("jwt-value-e2e"));
    assert!(persisted.contains("${FLOWPROOF_TEST_BASE}"));

    // Replay stages the same session and re-navigates.
    let mut driver = MockAppDriver::new(&[]).with_surface_text("Settings");
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "session flow replays: {report:?}");
    let staged = driver
        .staged_session
        .as_ref()
        .expect("session staged at replay");
    assert_eq!(staged.cookies[0].1, "jwt-value-e2e");
    assert_eq!(driver.navigations, vec!["https://app.test/settings"]);
    assert_eq!(driver.reloads, 1);

    std::env::remove_var("FLOWPROOF_TEST_SESSION_JWT");
    std::env::remove_var("FLOWPROOF_TEST_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn notepad_shares_the_provenance_agnostic_assertion_grammar() {
    let dir = std::env::temp_dir().join("flowproof-replay-uia-asserts");
    std::fs::create_dir_all(&dir).expect("temp dir");

    // A desktop (UIA-profile) app using the SHARED assertion grammar:
    // surface negatives/counts and native-id field values — no web anywhere.
    let spec = FlowSpec::parse(
        "name: Desktop assertions
app: notepad
steps:
  - Type status report
  - assert: document contains status report
  - assert: page shows saved 2 times
  - assert: page does not show error
  - assert: the 15 field contains status report
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&["15"]).with_surface_text("draft saved — autosave saved");
    let trace = dir.join("desktop.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("recording succeeds");

    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    // Surface asserts carry the neutral scope key and NO selector ladder…
    assert!(persisted.contains("\"scope\":\"surface\""));
    assert!(
        !persisted.contains("\"css\":\"body\""),
        "no web-ism in a desktop trace"
    );
    // …and the field assert anchors on the native automation id.
    assert!(persisted.contains("\"automation_id\":\"15\""));

    let mut driver = MockAppDriver::new(&["15"]).with_surface_text("draft saved — autosave saved");
    driver.texts.insert("15".into(), "status report".into());
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "desktop assertions replay: {report:?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn api_assertions_probe_a_real_http_server_out_of_band() {
    let dir = std::env::temp_dir().join("flowproof-replay-oob-api");
    std::fs::create_dir_all(&dir).expect("temp dir");

    // A real (local) HTTP server: /templates answers 200 with a JSON body,
    // anything else 404. The probe goes out of band — no UI involved.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let body = r#"[{"name":"playwrightTemplate"},{"name":"playwrightTemplateRoot"}]"#;
            let response = if request.starts_with("GET /templates") {
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
            } else {
                "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".into()
            };
            let _ = stream.write_all(response.as_bytes());
        }
    });
    std::env::set_var("FLOWPROOF_TEST_API", format!("http://127.0.0.1:{port}"));

    let spec = FlowSpec::parse(
        "name: Business data flow
app: web
url: https://e.test/x
steps:
  - assert: page shows Templates
  - assert_api:
      request: GET ${FLOWPROOF_TEST_API}/templates
      status: 200
      body_contains: playwrightTemplateRoot
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]).with_surface_text("Templates");
    let trace = dir.join("oob.trace.jsonl");
    record(&spec, &mut driver, &trace).expect("recording succeeds (probe really ran)");

    // The trace stores the RAW url reference and the api kind — no resolved
    // host, no credentials.
    let persisted = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(persisted.contains("\"kind\":\"api\""));
    assert!(persisted.contains("${FLOWPROOF_TEST_API}"));
    assert!(!persisted.contains(&format!("127.0.0.1:{port}")));

    let mut driver = MockAppDriver::new(&[]).with_surface_text("Templates");
    let (report, _run_dir) = run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "oob assert replays: {report:?}");

    // A wrong expectation fails after its bound with the live reason.
    let spec = FlowSpec::parse(
        "name: Business data mismatch
app: web
url: https://e.test/x
steps:
  - assert_api:
      request: GET ${FLOWPROOF_TEST_API}/missing
      status: 200
      timeout_seconds: 1
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]);
    let trace2 = dir.join("oob-miss.trace.jsonl");
    let err = record(&spec, &mut driver, &trace2).expect_err("404 must fail the assert");
    assert!(err.to_string().contains("404"), "err: {err}");

    std::env::remove_var("FLOWPROOF_TEST_API");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn sql_assertions_fail_closed_without_a_configured_connection() {
    let dir = std::env::temp_dir().join("flowproof-replay-oob-sql");
    std::fs::create_dir_all(&dir).expect("temp dir");

    let spec = FlowSpec::parse(
        "name: Posted record
app: web
url: https://e.test/x
steps:
  - assert_sql:
      connection: reporting-e2e-unset
      query: SELECT count(*) FROM templates
      equals: \"2\"
",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&[]);
    let trace = dir.join("sql.trace.jsonl");
    // No FLOWPROOF_SQL_REPORTING_E2E_UNSET in the environment: recording
    // refuses immediately with an error naming the variable — never a
    // silent pass, never a poll loop against nothing.
    let err = record(&spec, &mut driver, &trace).expect_err("must fail closed");
    assert!(
        err.to_string()
            .contains("FLOWPROOF_SQL_REPORTING_E2E_UNSET"),
        "err: {err}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
