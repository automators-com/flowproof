//! Deterministic step resolution, per app.
//!
//! This is the first, rule-based "authoring backend": each supported app has
//! a small vocabulary of natural-language steps mapped to concrete UIA
//! targets. LLM-backed authoring for arbitrary apps slots in beside it later
//! — the recorder only consumes the resolved actions, not the rules.

use crate::spec::SpecStep;

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("cannot resolve step '{step}': {reason}")]
    Unresolvable { step: String, reason: String },
    #[error("no rules for app '{0}' (supported: calc, notepad)")]
    UnsupportedApp(String),
}

/// A concrete action planned from one natural-language step. One step may
/// expand to several actions (e.g. `Type 53` in calc → two button presses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    /// Press (invoke) a button.
    Press {
        automation_id: String,
        /// Human-readable label (recorded as the selector name hint).
        label: String,
    },
    /// Type literal text into an element.
    TypeText { automation_id: String, text: String },
    /// Assert on an element's visible text.
    AssertText {
        automation_id: String,
        expected: String,
        /// Substring match instead of equality.
        contains: bool,
        /// Compare the trailing numeric value instead of raw text.
        numeric: bool,
    },
}

/// AutomationId of the Windows Calculator result display.
pub const CALC_DISPLAY_ID: &str = "CalculatorResults";

/// AutomationId of classic Notepad's edit control (Win32 control id 15).
pub const NOTEPAD_EDITOR_ID: &str = "15";

fn unresolvable(step: &str, reason: impl Into<String>) -> RulesError {
    RulesError::Unresolvable {
        step: step.to_string(),
        reason: reason.into(),
    }
}

/// Case-insensitively strip an ASCII `prefix`, returning the rest of the
/// ORIGINAL string (case preserved).
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// Resolve one spec step into concrete actions for `app`.
pub fn resolve_step(app: &str, step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
    match app {
        "calc" => calc::resolve(step),
        "notepad" => notepad::resolve(step),
        other => Err(RulesError::UnsupportedApp(other.to_string())),
    }
}

mod calc {
    use super::*;

    fn digit_button(c: char) -> Option<(String, String)> {
        if let Some(d) = c.to_digit(10) {
            const NAMES: [&str; 10] = [
                "Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine",
            ];
            Some((format!("num{d}Button"), NAMES[d as usize].to_string()))
        } else if c == '.' {
            Some(("decimalSeparatorButton".into(), "Decimal separator".into()))
        } else {
            None
        }
    }

    fn operator_button(word: &str) -> Option<(&'static str, &'static str)> {
        match word {
            "plus" | "add" => Some(("plusButton", "Plus")),
            "minus" | "subtract" => Some(("minusButton", "Minus")),
            "times" | "multiply" => Some(("multiplyButton", "Multiply by")),
            "divide" => Some(("divideButton", "Divide by")),
            "equals" | "equal" => Some(("equalButton", "Equals")),
            "clear" => Some(("clearButton", "Clear")),
            _ => None,
        }
    }

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => resolve_plain(text),
            SpecStep::Assert { assert } => Ok(vec![resolve_assert(assert)?]),
        }
    }

    fn resolve_plain(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
        let trimmed = text.trim();

        if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
            let value = rest.trim();
            if value.is_empty() {
                return Err(unresolvable(trimmed, "nothing to type"));
            }
            let mut actions = Vec::new();
            for c in value.chars() {
                let (automation_id, label) = digit_button(c).ok_or_else(|| {
                    unresolvable(trimmed, format!("'{c}' is not a digit or decimal point"))
                })?;
                actions.push(ResolvedAction::Press {
                    automation_id,
                    label,
                });
            }
            return Ok(actions);
        }

        if let Some(rest) = strip_prefix_ci(trimmed, "press ") {
            let word = rest.trim().to_lowercase();
            let (automation_id, label) = operator_button(&word)
                .ok_or_else(|| unresolvable(trimmed, format!("unknown calculator key '{word}'")))?;
            return Ok(vec![ResolvedAction::Press {
                automation_id: automation_id.into(),
                label: label.into(),
            }]);
        }

        Err(unresolvable(
            trimmed,
            "expected 'Type <digits>' or 'Press <key>'",
        ))
    }

    fn resolve_assert(text: &str) -> Result<ResolvedAction, RulesError> {
        let trimmed = text.trim();
        if let Some(rest) = strip_prefix_ci(trimmed, "display shows ") {
            let expected = rest.trim();
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected value"));
            }
            return Ok(ResolvedAction::AssertText {
                automation_id: CALC_DISPLAY_ID.into(),
                expected: expected.to_string(),
                contains: false,
                numeric: true,
            });
        }
        Err(unresolvable(trimmed, "expected 'display shows <value>'"))
    }
}

mod notepad {
    use super::*;

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => {
                let trimmed = text.trim();
                if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
                    let value = rest.trim();
                    if value.is_empty() {
                        return Err(unresolvable(trimmed, "nothing to type"));
                    }
                    return Ok(vec![ResolvedAction::TypeText {
                        automation_id: NOTEPAD_EDITOR_ID.into(),
                        text: value.to_string(),
                    }]);
                }
                Err(unresolvable(trimmed, "expected 'Type <text>'"))
            }
            SpecStep::Assert { assert } => {
                let trimmed = assert.trim();
                if let Some(rest) = strip_prefix_ci(trimmed, "document contains ") {
                    let expected = rest.trim();
                    if expected.is_empty() {
                        return Err(unresolvable(trimmed, "no expected text"));
                    }
                    return Ok(vec![ResolvedAction::AssertText {
                        automation_id: NOTEPAD_EDITOR_ID.into(),
                        expected: expected.to_string(),
                        contains: true,
                        numeric: false,
                    }]);
                }
                Err(unresolvable(trimmed, "expected 'document contains <text>'"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_type_expands_per_digit() {
        let actions =
            resolve_step("calc", &SpecStep::Plain("Type 53".into())).expect("digits resolve");
        assert_eq!(
            actions,
            vec![
                ResolvedAction::Press {
                    automation_id: "num5Button".into(),
                    label: "Five".into()
                },
                ResolvedAction::Press {
                    automation_id: "num3Button".into(),
                    label: "Three".into()
                },
            ]
        );
    }

    #[test]
    fn calc_press_maps_operators() {
        let actions =
            resolve_step("calc", &SpecStep::Plain("Press plus".into())).expect("plus resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                automation_id: "plusButton".into(),
                label: "Plus".into()
            }]
        );
    }

    #[test]
    fn calc_assert_is_numeric_equals() {
        let actions = resolve_step(
            "calc",
            &SpecStep::Assert {
                assert: "display shows 8".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                automation_id: CALC_DISPLAY_ID.into(),
                expected: "8".into(),
                contains: false,
                numeric: true,
            }]
        );
    }

    #[test]
    fn notepad_type_preserves_case() {
        let actions = resolve_step(
            "notepad",
            &SpecStep::Plain("Type Hello from FlowProof".into()),
        )
        .expect("type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                automation_id: NOTEPAD_EDITOR_ID.into(),
                text: "Hello from FlowProof".into(),
            }]
        );
    }

    #[test]
    fn notepad_assert_is_contains() {
        let actions = resolve_step(
            "notepad",
            &SpecStep::Assert {
                assert: "document contains Hello".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                automation_id: NOTEPAD_EDITOR_ID.into(),
                expected: "Hello".into(),
                contains: true,
                numeric: false,
            }]
        );
    }

    #[test]
    fn unknown_steps_and_apps_error_clearly() {
        let err = resolve_step("calc", &SpecStep::Plain("Wave at the screen".into()))
            .expect_err("unknown step must fail");
        assert!(err.to_string().contains("Wave at the screen"));

        let err = resolve_step("sap", &SpecStep::Plain("Type 5".into()))
            .expect_err("unknown app must fail");
        assert!(matches!(err, RulesError::UnsupportedApp(_)));
    }
}
