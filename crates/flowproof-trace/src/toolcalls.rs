//! Matching `assert_tool_call` expectations against a recorded
//! trajectory: which tools the agent called, and what it called them with.
//!
//! Which tool was called is half the test. What it was called WITH is the
//! other half, and usually where the bugs are - a multi-step agent that
//! calls the right tools in the right order but threads the wrong id from
//! one result into the next call is broken in a way only argument
//! matching catches.
//!
//! Two properties shape everything here:
//!
//! **Ordered subsequence, not equality.** The listed calls must appear in
//! the listed order; unlisted calls between them are fine. Real agents
//! retry, look things up twice, and call a logging tool nobody wants to
//! write down. A `strict` flow forbids the unlisted ones, for the flows
//! where the exact call set IS the contract.
//!
//! **Partial arguments, not deep equality.** Assert the arguments that
//! carry the intent. The cassette already pins every argument byte-exactly
//! for regression purposes, so an expectation's job is to say which
//! properties are MEANINGFUL - the ones a reviewer should defend when a
//! re-record produces a diff, as against values the cassette merely
//! happens to have pinned.

use crate::cassette::ToolCall;
use serde::{Deserialize, Serialize};

/// How one argument must compare.
///
/// Deliberately a closed set of named intents rather than an expression
/// language: a spec is read far more often than it is written, and
/// `where flight.id equals KQ311` survives being read aloud in review.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgMatch {
    /// Equal as text, with numbers compared numerically so `1` and `1.0`
    /// agree. Models emit both.
    Equals(String),
    Contains(String),
    /// The value, as text, matches a regular expression. For arguments
    /// whose SHAPE is the contract but whose value is volatile - a seat
    /// like `[0-9]+[A-F]`, an id with a fixed format, a rendered date.
    Matches(String),
    /// Present at all, whatever the value. For arguments whose VALUE is
    /// volatile - a rendered date, a generated idempotency key - but whose
    /// presence is the point.
    Exists,
    /// Absent. The guard-path counterpart to `Exists`.
    Absent,
}

/// One `where` clause: a dotted path into the call's JSON arguments, and
/// how the value there must compare.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgExpectation {
    pub path: String,
    #[serde(flatten)]
    pub matcher: ArgMatch,
}

/// One `assert_tool_call` step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallExpectation {
    pub tool: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<ArgExpectation>,
}

impl ToolCallExpectation {
    pub fn new(tool: &str) -> Self {
        Self {
            tool: tool.into(),
            args: Vec::new(),
        }
    }

    /// Does this call satisfy the expectation? Returns why not, so a
    /// failure can say "create_booking was called, but with flight.id
    /// KQ999" instead of "not found".
    pub fn check(&self, call: &ToolCall) -> Result<(), String> {
        if call.name != self.tool {
            return Err(format!("called {} instead", call.name));
        }
        if self.args.is_empty() {
            return Ok(());
        }
        // Arguments come off the wire as a string, and models do emit
        // malformed JSON. That is a finding to report, not a panic.
        let Some(json) = call.arguments_json() else {
            return Err(format!(
                "its arguments are not valid JSON: {}",
                abbreviate(&call.arguments)
            ));
        };
        for expectation in &self.args {
            let found = lookup(&json, &expectation.path);
            match (&expectation.matcher, found) {
                (ArgMatch::Exists, Some(_)) | (ArgMatch::Absent, None) => {}
                (ArgMatch::Exists, None) => return Err(format!("{} is absent", expectation.path)),
                (ArgMatch::Absent, Some(value)) => {
                    return Err(format!(
                        "{} is present, as {}",
                        expectation.path,
                        render(value)
                    ))
                }
                (matcher, None) => {
                    return Err(format!(
                        "{} is absent, so it cannot {}",
                        expectation.path,
                        describe(matcher)
                    ))
                }
                (ArgMatch::Equals(want), Some(value)) => {
                    let got = render(value);
                    if !equal_values(want, &got) {
                        return Err(format!("{} is {got}, not {want}", expectation.path));
                    }
                }
                (ArgMatch::Contains(want), Some(value)) => {
                    let got = render(value);
                    if !got.contains(want.as_str()) {
                        return Err(format!(
                            "{} is {got}, which does not contain {want}",
                            expectation.path
                        ));
                    }
                }
                (ArgMatch::Matches(pattern), Some(value)) => {
                    // Compile per check: a Regex is not Clone/PartialEq/
                    // Serialize, so the expectation stores the pattern
                    // string and the grammar validated that it compiles.
                    let regex = regex::Regex::new(pattern).map_err(|e| {
                        format!(
                            "{} has an invalid pattern `{pattern}`: {e}",
                            expectation.path
                        )
                    })?;
                    let got = render(value);
                    if !regex.is_match(&got) {
                        return Err(format!(
                            "{} is {got}, which does not match /{pattern}/",
                            expectation.path
                        ));
                    }
                }
            }
        }
        Ok(())
    }
}

fn describe(matcher: &ArgMatch) -> String {
    match matcher {
        ArgMatch::Equals(want) => format!("equal {want}"),
        ArgMatch::Contains(want) => format!("contain {want}"),
        ArgMatch::Matches(pattern) => format!("match /{pattern}/"),
        ArgMatch::Exists => "exist".into(),
        ArgMatch::Absent => "be absent".into(),
    }
}

fn abbreviate(text: &str) -> String {
    const LIMIT: usize = 120;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    format!("{}...", text.chars().take(LIMIT).collect::<String>())
}

/// A JSON scalar as the text an expectation compares against. A string
/// arrives without its quotes, which is what a spec author writes; an
/// object or array renders compactly so an error can still show it.
fn render(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Compare as numbers when both sides are numbers, so `1` matches `1.0`
/// and `1e3` matches `1000`. Models are inconsistent about this and a
/// spec author should not have to guess which spelling today's model
/// picked.
fn equal_values(want: &str, got: &str) -> bool {
    if want == got {
        return true;
    }
    match (want.trim().parse::<f64>(), got.trim().parse::<f64>()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Follow a dotted path into JSON. A numeric segment indexes an array,
/// so `flights.0.id` works, which is how tool arguments usually nest.
fn lookup<'a>(root: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    let mut current = root;
    for segment in path.split('.') {
        current = match current {
            serde_json::Value::Object(map) => map.get(segment)?,
            serde_json::Value::Array(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// Why a trajectory did not satisfy its expectations.
#[derive(Debug, Clone, PartialEq)]
pub enum TrajectoryError {
    /// An expected call never happened. `near` carries the closest thing
    /// that did, when there was one, because "create_booking was called
    /// with the wrong flight" is a different bug from "never called".
    Missing {
        expectation: ToolCallExpectation,
        near: Option<String>,
    },
    /// A `strict` flow saw a call nobody listed.
    Unexpected { tool: String, turn: usize },
    /// An `assert_no_tool_call` was violated.
    Forbidden { tool: String, turn: usize },
}

impl std::fmt::Display for TrajectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrajectoryError::Missing { expectation, near } => {
                write!(f, "{} was never called", expectation.tool)?;
                if !expectation.args.is_empty() {
                    write!(f, " with the expected arguments")?;
                }
                match near {
                    Some(why) => write!(f, "; the closest call {why}"),
                    None => Ok(()),
                }
            }
            TrajectoryError::Unexpected { tool, turn } => write!(
                f,
                "{tool} was called at turn {} and the flow is strict, so no unlisted call is allowed",
                turn + 1
            ),
            TrajectoryError::Forbidden { tool, turn } => {
                write!(f, "{tool} must not be called, but it was, at turn {}", turn + 1)
            }
        }
    }
}

/// Check expectations against a trajectory as an ORDERED SUBSEQUENCE.
///
/// `calls` is the flattened trajectory, `(turn, call)` in order, as
/// [`crate::cassette::Cassette::tool_calls`] produces. `strict` forbids
/// calls no expectation claimed.
pub fn check_trajectory(
    expectations: &[ToolCallExpectation],
    calls: &[(usize, &ToolCall)],
    strict: bool,
) -> Result<(), TrajectoryError> {
    let mut cursor = 0usize;
    // Which calls an expectation consumed, so strict mode can tell an
    // unlisted call from a matched one.
    let mut claimed = vec![false; calls.len()];

    for expectation in expectations {
        // Why the nearest same-named call did not qualify. Worth keeping:
        // "called with flight.id KQ999" is the actual bug report, and
        // without it the failure is just "never called".
        let mut near = None;
        let found = calls[cursor..]
            .iter()
            .enumerate()
            .find_map(|(offset, (_, call))| match expectation.check(call) {
                Ok(()) => Some(cursor + offset),
                Err(why) => {
                    if call.name == expectation.tool && near.is_none() {
                        near = Some(why);
                    }
                    None
                }
            });
        let Some(index) = found else {
            return Err(TrajectoryError::Missing {
                expectation: expectation.clone(),
                near,
            });
        };
        claimed[index] = true;
        // Subsequence: the next expectation starts AFTER this match, so
        // order is enforced, and the calls skipped over stay unclaimed.
        cursor = index + 1;
    }

    if strict {
        if let Some((turn, call)) = calls
            .iter()
            .zip(&claimed)
            .find(|(_, claimed)| !**claimed)
            .map(|(c, _)| c)
        {
            return Err(TrajectoryError::Unexpected {
                tool: call.name.clone(),
                turn: *turn,
            });
        }
    }
    Ok(())
}

/// Check an `assert_no_tool_call`: the tool must not appear ANYWHERE in
/// the trajectory, regardless of position.
///
/// This is the guard-path assertion, and arguably the highest-value one
/// in the feature. The dangerous tool is mocked at the boundary, so even
/// a misbehaving agent causes no harm while the test proves it misbehaved.
/// An expectation with `args` narrows it: "never called with an amount
/// above the limit" rather than "never called".
pub fn check_absent(
    expectation: &ToolCallExpectation,
    calls: &[(usize, &ToolCall)],
) -> Result<(), TrajectoryError> {
    match calls
        .iter()
        .find(|(_, call)| expectation.check(call).is_ok())
    {
        Some((turn, call)) => Err(TrajectoryError::Forbidden {
            tool: call.name.clone(),
            turn: *turn,
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: format!("call_{name}"),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn expect_with(tool: &str, args: &[(&str, ArgMatch)]) -> ToolCallExpectation {
        ToolCallExpectation {
            tool: tool.into(),
            args: args
                .iter()
                .map(|(path, matcher)| ArgExpectation {
                    path: (*path).into(),
                    matcher: matcher.clone(),
                })
                .collect(),
        }
    }

    fn trajectory(calls: &[ToolCall]) -> Vec<(usize, &ToolCall)> {
        calls.iter().enumerate().collect()
    }

    /// The default: listed calls in order, unlisted ones in between are
    /// fine. Real agents log, retry, and look things up twice.
    #[test]
    fn unlisted_calls_between_expected_ones_are_allowed() {
        let calls = [
            call("search_flights", r#"{"destination":"NBO"}"#),
            call("log_event", r#"{"kind":"search"}"#),
            call("create_booking", r#"{"flight":{"id":"KQ311"}}"#),
        ];
        let expectations = [
            ToolCallExpectation::new("search_flights"),
            ToolCallExpectation::new("create_booking"),
        ];
        assert_eq!(
            check_trajectory(&expectations, &trajectory(&calls), false),
            Ok(())
        );
    }

    /// Order is part of the assertion: booking before searching is a real
    /// bug, not a detail.
    #[test]
    fn order_is_enforced() {
        let calls = [call("create_booking", "{}"), call("search_flights", "{}")];
        let expectations = [
            ToolCallExpectation::new("search_flights"),
            ToolCallExpectation::new("create_booking"),
        ];
        let err = check_trajectory(&expectations, &trajectory(&calls), false)
            .expect_err("booking came first");
        assert!(
            err.to_string().contains("create_booking was never called"),
            "{err}"
        );
    }

    /// Strict mode is for flows where the exact call set is the contract.
    #[test]
    fn strict_forbids_a_call_nobody_listed() {
        let calls = [
            call("search_flights", "{}"),
            call("charge_card", r#"{"amount":900}"#),
        ];
        let expectations = [ToolCallExpectation::new("search_flights")];
        assert_eq!(
            check_trajectory(&expectations, &trajectory(&calls), false),
            Ok(()),
            "subsequence tolerates it"
        );
        let err = check_trajectory(&expectations, &trajectory(&calls), true)
            .expect_err("strict must not");
        assert!(err.to_string().contains("charge_card"), "{err}");
        assert!(err.to_string().contains("turn 2"), "1-based: {err}");
    }

    /// The chained-argument case the design doc calls out: because the
    /// tool results are spec-authored mocks, the id a downstream call
    /// SHOULD carry is known when the spec is written. This is what
    /// multi-step agents actually get wrong.
    #[test]
    fn a_wrong_threaded_argument_is_caught_and_named() {
        let calls = [call("create_booking", r#"{"flight":{"id":"KQ999"}}"#)];
        let expectations = [expect_with(
            "create_booking",
            &[("flight.id", ArgMatch::Equals("KQ311".into()))],
        )];
        let err =
            check_trajectory(&expectations, &trajectory(&calls), false).expect_err("wrong id");
        let message = err.to_string();
        // The failure must distinguish "called with the wrong id" from
        // "never called" - a different bug entirely.
        assert!(message.contains("with the expected arguments"), "{message}");
        assert!(
            message.contains("flight.id is KQ999, not KQ311"),
            "{message}"
        );
    }

    #[test]
    fn paths_reach_into_arrays_and_compare_numbers_numerically() {
        let calls = [call(
            "book",
            r#"{"passengers":[{"name":"Casey Jordan"}],"seats":2,"price":1000.0}"#,
        )];
        let ok = expect_with(
            "book",
            &[
                ("passengers.0.name", ArgMatch::Contains("Casey".into())),
                // 2 against "2", and 1000 against "1000.0".
                ("seats", ArgMatch::Equals("2".into())),
                ("price", ArgMatch::Equals("1000".into())),
            ],
        );
        assert_eq!(ok.check(&calls[0]), Ok(()));

        let missing = expect_with("book", &[("passengers.1.name", ArgMatch::Exists)]);
        assert!(missing.check(&calls[0]).is_err());
    }

    /// `matches` is for arguments whose SHAPE is the contract: the value
    /// changes every run but its format does not.
    #[test]
    fn matches_checks_a_value_against_a_regex() {
        let calls = [call("book", r#"{"seat":"12A","id":"BK-99327"}"#)];
        let ok = expect_with(
            "book",
            &[
                ("seat", ArgMatch::Matches("^[0-9]+[A-F]$".into())),
                ("id", ArgMatch::Matches(r"^BK-\d+$".into())),
            ],
        );
        assert_eq!(ok.check(&calls[0]), Ok(()));

        let wrong = expect_with("book", &[("seat", ArgMatch::Matches("^[A-F]+$".into()))]);
        let err = wrong.check(&calls[0]).expect_err("12A is not all letters");
        assert!(err.contains("does not match"), "{err}");
        assert!(err.contains("12A"), "the failure shows the value: {err}");
    }

    /// A pattern that does not compile is reported, not a panic. The
    /// grammar rejects it earlier, but the matcher must be total.
    #[test]
    fn an_invalid_pattern_is_an_error_not_a_panic() {
        let calls = [call("book", r#"{"seat":"12A"}"#)];
        let bad = expect_with("book", &[("seat", ArgMatch::Matches("[unclosed".into()))]);
        let err = bad.check(&calls[0]).expect_err("bad regex");
        assert!(err.contains("invalid pattern"), "{err}");
    }

    /// Volatile arguments: assert shape, not value. The cassette still
    /// pins the exact recorded bytes for regression purposes.
    #[test]
    fn existence_covers_volatile_arguments() {
        let calls = [call(
            "book",
            r#"{"idempotency_key":"c8f1-2026-07-22","coupon":null}"#,
        )];
        assert_eq!(
            expect_with("book", &[("idempotency_key", ArgMatch::Exists)]).check(&calls[0]),
            Ok(())
        );
        // An explicit null is PRESENT. Saying otherwise would make
        // `Absent` mean two different things.
        assert_eq!(
            expect_with("book", &[("coupon", ArgMatch::Exists)]).check(&calls[0]),
            Ok(())
        );
        let err = expect_with("book", &[("coupon", ArgMatch::Absent)])
            .check(&calls[0])
            .expect_err("null is present");
        assert!(err.contains("is present"), "{err}");
    }

    /// The guard path: the dangerous tool is mocked, so a misbehaving
    /// agent does no harm while the test proves it misbehaved.
    #[test]
    fn a_forbidden_tool_is_caught_anywhere_in_the_trajectory() {
        let calls = [
            call("search_flights", "{}"),
            call("charge_card", r#"{"amount":900}"#),
        ];
        let err = check_absent(
            &ToolCallExpectation::new("charge_card"),
            &trajectory(&calls),
        )
        .expect_err("it was called");
        assert!(err.to_string().contains("must not be called"), "{err}");

        assert_eq!(
            check_absent(&ToolCallExpectation::new("refund"), &trajectory(&calls)),
            Ok(())
        );

        // Narrowed: charging is fine, charging over the limit is not.
        let over = expect_with("charge_card", &[("amount", ArgMatch::Equals("900".into()))]);
        assert!(check_absent(&over, &trajectory(&calls)).is_err());
        let other = expect_with("charge_card", &[("amount", ArgMatch::Equals("10".into()))]);
        assert_eq!(check_absent(&other, &trajectory(&calls)), Ok(()));
    }

    /// Models emit malformed JSON. Report it as the finding it is.
    #[test]
    fn malformed_arguments_are_reported_not_fatal() {
        let calls = [call("book", "{not json")];
        let err = expect_with("book", &[("flight.id", ArgMatch::Exists)])
            .check(&calls[0])
            .expect_err("unparseable");
        assert!(err.contains("not valid JSON"), "{err}");
    }
}
