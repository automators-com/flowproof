//! Rule-based recording: perform each resolved step's existence check
//! against the live application and write a v1 trace.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use flowproof_driver::{resolve_app, AppDriver, UiaSelector};
use flowproof_trace::format::{
    Action, AppInfo, Artifacts, Assertion, Condition, EnvInfo, Header, Selector, Step, Sync,
    TypeTextParams,
};
use flowproof_trace::{SelectorTier, FORMAT_NAME, FORMAT_VERSION};

use crate::rules::{resolve_step, ResolvedAction, NOTEPAD_EDITOR_ID};
use crate::spec::FlowSpec;

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const STEP_TIMEOUT_MS: u64 = 5000;

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error(transparent)]
    Rules(#[from] crate::rules::RulesError),
    #[error("unknown app '{0}' (this slice supports: calc, notepad)")]
    UnknownApp(String),
    #[error("element for step '{intent}' not found: [{selector}]")]
    ElementNotFound { intent: String, selector: String },
    #[error(
        "assertion '{intent}' does not hold while recording: expected '{expected}', element shows '{actual}'"
    )]
    AssertMismatch {
        intent: String,
        expected: String,
        actual: String,
    },
    #[error("driver error: {0}")]
    Driver(#[from] flowproof_driver::DriverError),
    #[error("cannot write trace {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Outcome of a recording session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    pub trace_path: std::path::PathBuf,
    pub steps: usize,
}

fn native_selector(payload: serde_json::Map<String, serde_json::Value>) -> Selector {
    Selector {
        tier: SelectorTier::NativeId,
        provenance: flowproof_trace::format::Adapter::Uia,
        confidence: Some(1.0),
        payload,
    }
}

/// The recorded selector ladder for an action target. Notepad's editor gets
/// a second rung (control type + name) because the Win32 control id `15`
/// varies across Notepad generations — the first real ladder fallback.
fn selectors_for(app: &str, automation_id: &str, label: Option<&str>) -> Vec<Selector> {
    let mut payload = serde_json::Map::new();
    payload.insert("automation_id".into(), automation_id.into());
    if let Some(label) = label {
        payload.insert("name".into(), label.into());
    }
    let mut ladder = vec![native_selector(payload)];
    if app == "notepad" && automation_id == NOTEPAD_EDITOR_ID {
        let mut fallback = serde_json::Map::new();
        fallback.insert("control_type".into(), "Edit".into());
        fallback.insert("name".into(), "Text Editor".into());
        ladder.push(native_selector(fallback));
    }
    ladder
}

fn step_for(id: usize, intent: &str, app: &str, action: &ResolvedAction) -> Step {
    let (selectors, trace_action) = match action {
        ResolvedAction::Press {
            automation_id,
            label,
        } => (
            selectors_for(app, automation_id, Some(label)),
            Action::Click(serde_json::Map::new()),
        ),
        ResolvedAction::TypeText {
            automation_id,
            text,
        } => (
            selectors_for(app, automation_id, None),
            Action::TypeText(TypeTextParams {
                text: text.clone(),
                submit: None,
                extra: serde_json::Map::new(),
            }),
        ),
        ResolvedAction::AssertText {
            automation_id,
            expected,
            contains,
            numeric,
        } => {
            let expect = if *contains {
                serde_json::json!({ "value_contains": expected })
            } else if *numeric {
                serde_json::json!({ "value_equals": expected, "normalize": "numeric" })
            } else {
                serde_json::json!({ "value_equals": expected })
            };
            (
                selectors_for(app, automation_id, None),
                Action::Assert(Assertion::ElementState {
                    expect,
                    selector_ref: Some(0),
                }),
            )
        }
    };
    Step {
        id: format!("s{id:04}"),
        intent: intent.to_string(),
        action: trace_action,
        selectors,
        sync: Sync {
            pre: vec![Condition::ElementExists {
                timeout_ms: STEP_TIMEOUT_MS,
                selector_ref: None, // any rung of the ladder satisfies it
            }],
            post: vec![],
        },
        artifacts: Artifacts::default(),
    }
}

fn action_selector(action: &ResolvedAction) -> UiaSelector {
    match action {
        ResolvedAction::Press { automation_id, .. }
        | ResolvedAction::TypeText { automation_id, .. }
        | ResolvedAction::AssertText { automation_id, .. } => {
            UiaSelector::automation_id(automation_id.clone())
        }
    }
}

fn assert_holds(actual: &str, expected: &str, contains: bool, numeric: bool) -> bool {
    if contains {
        actual.contains(expected)
    } else if numeric {
        matches!(
            (flowproof_driver::numeric_value(actual), expected.parse::<f64>()),
            (Some(a), Ok(e)) if a == e
        )
    } else {
        actual == expected
    }
}

/// Record `spec` against the live app via `driver`, writing the trace to
/// `out`. Every planned action's target element must exist before it is
/// written — recording is a verification pass, not a transcription.
pub fn record<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
) -> Result<RecordSummary, RecordError> {
    let target = resolve_app(&spec.app).ok_or_else(|| RecordError::UnknownApp(spec.app.clone()))?;
    driver.launch(target.command, target.window_name, LAUNCH_TIMEOUT)?;
    let (width, height) = driver.screen_size()?;

    // Recording PERFORMS the flow once: buttons are really pressed and the
    // assert is checked against the live display, so a trace is only ever
    // written for a flow that actually worked.
    let mut steps = Vec::new();
    for spec_step in &spec.steps {
        for action in resolve_step(&spec.app, spec_step)? {
            let selector = action_selector(&action);
            if !driver.element_exists(&selector)? {
                return Err(RecordError::ElementNotFound {
                    intent: spec_step.intent().to_string(),
                    selector: selector.to_string(),
                });
            }
            match &action {
                ResolvedAction::Press { .. } => driver.invoke(&selector)?,
                ResolvedAction::TypeText { text, .. } => driver.type_text(&selector, text)?,
                ResolvedAction::AssertText {
                    expected,
                    contains,
                    numeric,
                    ..
                } => {
                    let actual = driver.read_text(&selector)?;
                    if !assert_holds(&actual, expected, *contains, *numeric) {
                        return Err(RecordError::AssertMismatch {
                            intent: spec_step.intent().to_string(),
                            expected: expected.clone(),
                            actual,
                        });
                    }
                }
            }
            steps.push(step_for(
                steps.len() + 1,
                spec_step.intent(),
                &spec.app,
                &action,
            ));
        }
    }

    let header = Header {
        format: FORMAT_NAME.to_string(),
        version: FORMAT_VERSION,
        trace_id: uuid::Uuid::new_v4().to_string(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        spec: Some(flowproof_trace::format::SpecRef {
            name: spec.name.clone(),
            path: None,
            hash: None,
        }),
        app: AppInfo {
            name: spec.app.clone(),
            adapter: flowproof_trace::format::Adapter::Uia,
            window_title: Some(target.window_name.to_string()),
            version: None,
        },
        agent: None, // rule-based recording: no model involved
        env: EnvInfo {
            os: std::env::consts::OS.to_string(),
            resolution: (width.max(1), height.max(1)),
            dpi_scale: None,
            locale: None,
        },
    };

    let io_err = |source: std::io::Error| RecordError::Io {
        path: out.display().to_string(),
        source,
    };
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let mut file = std::fs::File::create(out).map_err(io_err)?;
    writeln!(file, "{}", serde_json::to_string(&header)?).map_err(io_err)?;
    for step in &steps {
        writeln!(file, "{}", serde_json::to_string(step)?).map_err(io_err)?;
    }

    Ok(RecordSummary {
        trace_path: out.to_path_buf(),
        steps: steps.len(),
    })
}

#[cfg(test)]
mod tests {
    use flowproof_driver::mock::MockAppDriver;
    use flowproof_trace::TraceLine;

    use super::*;
    use crate::spec::FlowSpec;

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

    #[test]
    fn records_the_calc_flow_against_a_mock() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let mut driver =
            MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
        let dir = std::env::temp_dir().join("flowproof-recorder-test");
        let out = dir.join("calc.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");

        assert_eq!(summary.steps, 5); // 4 presses + 1 assert
        assert_eq!(
            driver.launched,
            Some(("calc.exe".to_string(), "Calculator".to_string()))
        );
        // Recording really performed the flow.
        assert_eq!(
            driver.invoked,
            vec!["num5Button", "plusButton", "num3Button", "equalButton"]
        );

        let contents = std::fs::read_to_string(&out).expect("trace written");
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(matches!(
            TraceLine::parse(lines[0]).expect("header parses"),
            TraceLine::Header(_)
        ));
        for line in &lines[1..] {
            assert!(matches!(
                TraceLine::parse(line).expect("step parses"),
                TraceLine::Step(_)
            ));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_element_fails_recording() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        // No plusButton in the fake UI.
        let mut driver = MockAppDriver::new(&[
            "num5Button",
            "num3Button",
            "equalButton",
            "CalculatorResults",
        ]);
        let out = std::env::temp_dir().join("flowproof-recorder-missing.trace.jsonl");
        let err = record(&spec, &mut driver, &out).expect_err("must fail");
        assert!(matches!(err, RecordError::ElementNotFound { .. }));
    }

    #[test]
    fn failing_assert_aborts_recording() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let mut driver =
            MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 9");
        let out = std::env::temp_dir().join("flowproof-recorder-mismatch.trace.jsonl");
        let err = record(&spec, &mut driver, &out).expect_err("must fail");
        assert!(matches!(err, RecordError::AssertMismatch { .. }));
    }

    const NOTEPAD_SPEC: &str = "\
name: Write a note
app: notepad
steps:
  - Type hello from flowproof
  - assert: document contains hello
";

    #[test]
    fn records_the_notepad_flow_against_a_mock() {
        let spec = FlowSpec::parse(NOTEPAD_SPEC).expect("spec parses");
        let mut driver = MockAppDriver::new(&["15"]);
        let dir = std::env::temp_dir().join("flowproof-recorder-notepad");
        let out = dir.join("notepad.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");

        assert_eq!(summary.steps, 2); // one type + one assert
        assert_eq!(
            driver.typed,
            vec![("15".to_string(), "hello from flowproof".to_string())]
        );

        // The editor step carries the two-rung selector ladder.
        let contents = std::fs::read_to_string(&out).expect("trace written");
        let step_line = contents.lines().nth(1).expect("first step");
        let step: serde_json::Value = serde_json::from_str(step_line).expect("step is JSON");
        let selectors = step["selectors"].as_array().expect("selectors array");
        assert_eq!(selectors.len(), 2);
        assert_eq!(selectors[0]["payload"]["automation_id"], "15");
        assert_eq!(selectors[1]["payload"]["control_type"], "Edit");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_app_is_rejected() {
        let spec = FlowSpec::parse("name: x\napp: sap\nsteps:\n  - Type 1\n").expect("parses");
        let mut driver = MockAppDriver::new(&[]);
        let out = std::env::temp_dir().join("unused.trace.jsonl");
        assert!(matches!(
            record(&spec, &mut driver, &out).expect_err("must fail"),
            RecordError::UnknownApp(_)
        ));
    }
}
