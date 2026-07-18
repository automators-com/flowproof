//! Deterministic step resolution for Windows Calculator.
//!
//! This is the first, rule-based "authoring backend": it maps the small
//! calculator vocabulary of natural-language steps to concrete UIA targets.
//! LLM-backed authoring for arbitrary apps slots in beside it later — the
//! recorder only consumes the resolved actions, not the rules.

use crate::spec::SpecStep;

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("cannot resolve step '{step}': {reason}")]
    Unresolvable { step: String, reason: String },
}

/// A concrete action planned from one natural-language step. One step may
/// expand to several actions (e.g. `Type 53` → two button presses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    /// Press a calculator button.
    Press {
        automation_id: String,
        /// Human-readable button label (recorded as the selector name hint).
        label: String,
    },
    /// Assert the display shows a value.
    AssertDisplay {
        automation_id: String,
        expected: String,
    },
}

/// AutomationId of the Windows Calculator result display.
pub const CALC_DISPLAY_ID: &str = "CalculatorResults";

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

fn unresolvable(step: &str, reason: impl Into<String>) -> RulesError {
    RulesError::Unresolvable {
        step: step.to_string(),
        reason: reason.into(),
    }
}

/// Resolve one spec step into concrete calculator actions.
pub fn resolve_step(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
    match step {
        SpecStep::Plain(text) => resolve_plain(text),
        SpecStep::Assert { assert } => Ok(vec![resolve_assert(assert)?]),
    }
}

fn resolve_plain(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();

    if let Some(rest) = lower.strip_prefix("type ") {
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

    if let Some(rest) = lower.strip_prefix("press ") {
        let word = rest.trim();
        let (automation_id, label) = operator_button(word)
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
    let lower = trimmed.to_lowercase();
    if let Some(rest) = lower.strip_prefix("display shows ") {
        let expected = rest.trim();
        if expected.is_empty() {
            return Err(unresolvable(trimmed, "no expected value"));
        }
        return Ok(ResolvedAction::AssertDisplay {
            automation_id: CALC_DISPLAY_ID.into(),
            expected: expected.to_string(),
        });
    }
    Err(unresolvable(trimmed, "expected 'display shows <value>'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_expands_per_digit() {
        let actions =
            resolve_step(&SpecStep::Plain("Type 53".into())).expect("digits resolve");
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
    fn press_maps_operators() {
        let actions = resolve_step(&SpecStep::Plain("Press plus".into())).expect("plus resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                automation_id: "plusButton".into(),
                label: "Plus".into()
            }]
        );
    }

    #[test]
    fn assert_extracts_expected_value() {
        let actions = resolve_step(&SpecStep::Assert {
            assert: "display shows 8".into(),
        })
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertDisplay {
                automation_id: CALC_DISPLAY_ID.into(),
                expected: "8".into()
            }]
        );
    }

    #[test]
    fn unknown_steps_error_clearly() {
        let err = resolve_step(&SpecStep::Plain("Wave at the screen".into()))
            .expect_err("unknown step must fail");
        assert!(err.to_string().contains("Wave at the screen"));
    }
}
