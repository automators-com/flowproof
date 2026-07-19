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
    #[error("no rules for app '{0}' (supported: calc, notepad, web)")]
    UnsupportedApp(String),
}

/// What an action targets: a UIA automation id or a CSS selector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    AutomationId(String),
    Css(String),
}

impl Target {
    pub fn id(id: impl Into<String>) -> Self {
        Target::AutomationId(id.into())
    }

    pub fn css(css: impl Into<String>) -> Self {
        Target::Css(css.into())
    }
}

/// A concrete action planned from one natural-language step. One step may
/// expand to several actions (e.g. `Type 53` in calc → two button presses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    /// Press (invoke/click) an element.
    Press {
        target: Target,
        /// Human-readable label (recorded as the selector name hint).
        label: String,
    },
    /// Type literal text into an element.
    TypeText { target: Target, text: String },
    /// Assert on an element's visible text. Assertions AUTO-WAIT: the
    /// engine polls until the expectation holds or `timeout_ms` elapses —
    /// deterministic (bounded, recorded in the trace), and what makes slow
    /// async UIs testable without sleeps.
    AssertText {
        target: Target,
        expected: String,
        /// Substring match instead of equality.
        contains: bool,
        /// Compare the trailing numeric value instead of raw text.
        numeric: bool,
        /// How long the expectation may take to become true.
        timeout_ms: u64,
    },
}

/// Default auto-wait for assertions (Playwright's expect default).
pub const ASSERT_TIMEOUT_MS: u64 = 10_000;
/// Default for explicit `Wait until …` steps — sized for slow backend
/// operations (a data-generation job, a report build).
pub const WAIT_STEP_TIMEOUT_MS: u64 = 60_000;

/// Parse a trailing `within <N>s` / `within <N> seconds` qualifier off a
/// step, returning (rest, timeout override).
fn split_within(text: &str) -> (&str, Option<u64>) {
    let lower = text.to_lowercase();
    let Some(pos) = lower.rfind(" within ") else {
        return (text, None);
    };
    let qualifier = lower[pos + " within ".len()..].trim();
    let digits = qualifier
        .strip_suffix(" seconds")
        .or_else(|| qualifier.strip_suffix(" second"))
        .or_else(|| qualifier.strip_suffix("sec"))
        .or_else(|| qualifier.strip_suffix('s'))
        .map(str::trim)
        .unwrap_or(qualifier);
    match digits.parse::<u64>() {
        Ok(seconds) if seconds > 0 => (text[..pos].trim_end(), Some(seconds * 1000)),
        _ => (text, None),
    }
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
        "web" => web::resolve(step),
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
                    target: Target::AutomationId(automation_id),
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
                target: Target::id(automation_id),
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
                target: Target::id(CALC_DISPLAY_ID),
                expected: expected.to_string(),
                contains: false,
                numeric: true,
                timeout_ms: ASSERT_TIMEOUT_MS,
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
                        target: Target::id(NOTEPAD_EDITOR_ID),
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
                        target: Target::id(NOTEPAD_EDITOR_ID),
                        expected: expected.to_string(),
                        contains: true,
                        numeric: false,
                        timeout_ms: ASSERT_TIMEOUT_MS,
                    }]);
                }
                Err(unresolvable(trimmed, "expected 'document contains <text>'"))
            }
        }
    }
}

mod web {
    use super::*;

    /// Where "page shows …" asserts look: the whole document body.
    pub(super) const PAGE_TEXT_CSS: &str = "body";

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => resolve_plain(text),
            SpecStep::Assert { assert } => {
                let trimmed = assert.trim();
                let (trimmed, timeout) = split_within(trimmed);
                if let Some(rest) = strip_prefix_ci(trimmed, "page shows ") {
                    let expected = rest.trim();
                    if expected.is_empty() {
                        return Err(unresolvable(trimmed, "no expected text"));
                    }
                    return Ok(vec![ResolvedAction::AssertText {
                        target: Target::css(PAGE_TEXT_CSS),
                        expected: expected.to_string(),
                        contains: true,
                        numeric: false,
                        timeout_ms: timeout.unwrap_or(ASSERT_TIMEOUT_MS),
                    }]);
                }
                Err(unresolvable(trimmed, "expected 'page shows <text>'"))
            }
        }
    }

    fn resolve_plain(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
        let trimmed = text.trim();

        // `Wait until page shows <text> [within <N>s]` → an auto-waiting
        // assert with a long default, for slow backend operations.
        if let Some(rest) = strip_prefix_ci(trimmed, "wait until page shows ") {
            let (expected, timeout) = split_within(rest.trim());
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected text"));
            }
            return Ok(vec![ResolvedAction::AssertText {
                target: Target::css(PAGE_TEXT_CSS),
                expected: expected.trim().to_string(),
                contains: true,
                numeric: false,
                timeout_ms: timeout.unwrap_or(WAIT_STEP_TIMEOUT_MS),
            }]);
        }

        // `Type <text> into the <id> field` → type into `#<id>`.
        if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
            let lower = rest.to_lowercase();
            if let Some(pos) = lower.rfind(" into the ") {
                let value = rest[..pos].trim();
                let field = lower[pos + " into the ".len()..].trim();
                if let Some(id) = field.strip_suffix(" field").map(str::trim) {
                    if value.is_empty() || id.is_empty() {
                        return Err(unresolvable(trimmed, "missing text or field name"));
                    }
                    return Ok(vec![ResolvedAction::TypeText {
                        target: Target::css(format!("#{id}")),
                        text: value.to_string(),
                    }]);
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Type <text> into the <id> field'",
            ));
        }

        // `Press the <id> button` → click `#<id>`.
        if let Some(rest) = strip_prefix_ci(trimmed, "press the ") {
            let lower = rest.to_lowercase();
            if let Some(id) = lower.strip_suffix(" button").map(str::trim) {
                if id.is_empty() {
                    return Err(unresolvable(trimmed, "missing button name"));
                }
                return Ok(vec![ResolvedAction::Press {
                    target: Target::css(format!("#{id}")),
                    label: id.to_string(),
                }]);
            }
            return Err(unresolvable(trimmed, "expected 'Press the <id> button'"));
        }

        Err(unresolvable(
            trimmed,
            "expected 'Type <text> into the <id> field' or 'Press the <id> button'",
        ))
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
                    target: Target::id("num5Button"),
                    label: "Five".into()
                },
                ResolvedAction::Press {
                    target: Target::id("num3Button"),
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
                target: Target::id("plusButton"),
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
                target: Target::id(CALC_DISPLAY_ID),
                expected: "8".into(),
                contains: false,
                numeric: true,
                timeout_ms: ASSERT_TIMEOUT_MS,
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
                target: Target::id(NOTEPAD_EDITOR_ID),
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
                target: Target::id(NOTEPAD_EDITOR_ID),
                expected: "Hello".into(),
                contains: true,
                numeric: false,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn web_type_and_press_map_to_css() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type Ada into the name field".into()),
        )
        .expect("type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "Ada".into(),
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Press the greet button".into()))
            .expect("press resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                target: Target::css("#greet"),
                label: "greet".into(),
            }]
        );
    }

    #[test]
    fn web_assert_is_contains_on_body() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows Hello, Ada".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::css("body"),
                expected: "Hello, Ada".into(),
                contains: true,
                numeric: false,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn wait_until_maps_to_a_long_auto_waiting_assert() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Wait until page shows Generation complete".into()),
        )
        .expect("wait resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::css("body"),
                expected: "Generation complete".into(),
                contains: true,
                numeric: false,
                timeout_ms: WAIT_STEP_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn within_qualifier_overrides_the_timeout() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Wait until page shows Done within 90s".into()),
        )
        .expect("wait resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: 90_000, .. } if expected == "Done"
        ));

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows Ready within 3 seconds".into(),
            },
        )
        .expect("assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: 3000, .. } if expected == "Ready"
        ));
    }

    #[test]
    fn within_only_strips_a_valid_qualifier() {
        // "within" followed by a non-number stays part of the expectation.
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows delivered within budget".into(),
            },
        )
        .expect("assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: ASSERT_TIMEOUT_MS, .. }
                if expected == "delivered within budget"
        ));
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
