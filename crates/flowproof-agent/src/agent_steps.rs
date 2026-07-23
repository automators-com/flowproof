//! The prose form of `assert_tool_call`, parsed into the expectation the
//! matcher takes.
//!
//! ```text
//! assert_tool_call: create_booking
//! assert_tool_call: create_booking where flight.id equals KQ311
//! assert_tool_call: charge_card where amount equals 900 and currency equals EUR
//! assert_no_tool_call: charge_card
//! ```
//!
//! Same shape as every other flowproof step: a sentence a reviewer can
//! read aloud, over a closed set of named intents rather than an
//! expression language. `where` clauses join with `and` because there is
//! no `or` - an expectation that could be satisfied two ways is two
//! expectations, and saying so in the spec is clearer than encoding it.
//!
//! The value runs to the end of its clause and is NOT quoted. Tool
//! arguments are ids, codes and amounts far more often than prose, so
//! quoting everything would be noise; a value that needs to contain the
//! word `and` is the case this trades away, and the structured `args:`
//! form exists for it.

use flowproof_trace::toolcalls::{ArgExpectation, ArgMatch, ToolCallExpectation};

/// Why a prose expectation did not parse. Carries the whole clause: the
/// author needs to see what they wrote, not just what was wrong with it.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseError {
    pub clause: String,
    pub detail: String,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot read `{}`: {}. Expected `<tool>[ where <path> \
             equals|contains <value>[ and ...]]`, `<path> exists`, or \
             `<path> is absent`",
            self.clause, self.detail
        )
    }
}

/// A tool name: what a JSON schema would accept, so a typo is caught here
/// rather than as a call that never matches.
fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// A dotted argument path, optionally indexing arrays: `flight.id`,
/// `passengers.0.name`.
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.split('.').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

pub fn parse(text: &str) -> Result<ToolCallExpectation, ParseError> {
    let clause = text.trim();
    let fail = |detail: &str| ParseError {
        clause: clause.to_string(),
        detail: detail.to_string(),
    };

    let (tool, rest) = match split_keyword(clause, "where") {
        Some((tool, rest)) => (tool.trim(), Some(rest)),
        None => (clause, None),
    };
    if !valid_tool_name(tool) {
        return Err(fail(&format!("`{tool}` is not a tool name")));
    }
    let mut expectation = ToolCallExpectation::new(tool);
    let Some(rest) = rest else {
        return Ok(expectation);
    };
    if rest.trim().is_empty() {
        return Err(fail("`where` with nothing after it"));
    }

    for raw in split_on_keyword(rest, "and") {
        let clause_text = raw.trim();
        if clause_text.is_empty() {
            return Err(fail("an empty `and` clause"));
        }
        expectation.args.push(parse_clause(clause_text, &fail)?);
    }
    Ok(expectation)
}

fn parse_clause(
    text: &str,
    fail: &impl Fn(&str) -> ParseError,
) -> Result<ArgExpectation, ParseError> {
    // Suffix forms first: `<path> exists` and `<path> is absent` have no
    // value, so trying them before the value forms keeps the value forms
    // from swallowing the keyword.
    for (suffix, matcher) in [
        (" exists", ArgMatch::Exists),
        (" is absent", ArgMatch::Absent),
        (" is missing", ArgMatch::Absent),
    ] {
        if let Some(path) = strip_suffix_ci(text, suffix) {
            let path = path.trim();
            if !valid_path(path) {
                return Err(fail(&format!("`{path}` is not an argument path")));
            }
            return Ok(ArgExpectation {
                path: path.to_string(),
                matcher,
            });
        }
    }

    for (keyword, make) in [
        ("equals", ArgMatch::Equals as fn(String) -> ArgMatch),
        ("contains", ArgMatch::Contains as fn(String) -> ArgMatch),
        ("matches", ArgMatch::Matches as fn(String) -> ArgMatch),
        ("is", ArgMatch::Equals as fn(String) -> ArgMatch),
    ] {
        let Some((path, value)) = split_keyword(text, keyword) else {
            continue;
        };
        let (path, value) = (path.trim(), value.trim());
        if !valid_path(path) {
            return Err(fail(&format!("`{path}` is not an argument path")));
        }
        if value.is_empty() {
            return Err(fail(&format!("`{keyword}` with no value after it")));
        }
        // A `matches` value is a regex: reject a broken pattern HERE, at
        // parse time, rather than at replay against a live trajectory.
        if keyword == "matches" {
            regex::Regex::new(value)
                .map_err(|e| fail(&format!("`{value}` is not a valid pattern: {e}")))?;
        }
        return Ok(ArgExpectation {
            path: path.to_string(),
            matcher: make(value.to_string()),
        });
    }
    // A clause that STARTS with a comparison has no path in front of it,
    // which is a more useful thing to say than "names no comparison" about
    // text that visibly contains one.
    for keyword in ["equals", "contains", "is"] {
        if text
            .split_whitespace()
            .next()
            .is_some_and(|first| first.eq_ignore_ascii_case(keyword))
        {
            return Err(fail(&format!(
                "`` is not an argument path: nothing comes before `{keyword}`"
            )));
        }
    }
    Err(fail(&format!("`{text}` names no comparison")))
}

fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    let at = text.len().checked_sub(suffix.len())?;
    text[at..].eq_ignore_ascii_case(suffix).then(|| &text[..at])
}

/// Split at the first standalone `keyword`, returning what is on each
/// side. Standalone matters: a tool called `where_clause` or an argument
/// path containing `and` must not be cut in half.
fn split_keyword<'a>(text: &'a str, keyword: &str) -> Option<(&'a str, &'a str)> {
    find_keyword(text, keyword).map(|at| (&text[..at], &text[at + keyword.len()..]))
}

fn split_on_keyword<'a>(text: &'a str, keyword: &str) -> Vec<&'a str> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(at) = find_keyword(rest, keyword) {
        parts.push(&rest[..at]);
        rest = &rest[at + keyword.len()..];
    }
    parts.push(rest);
    parts
}

/// Where `keyword` appears as a whole word, bounded by whitespace or by
/// the end of the text.
///
/// End-of-text counts as a boundary so a dangling `book where` is still
/// recognized as a `where` with nothing after it. Refusing to see the
/// keyword there would blame the tool name for the author's real mistake.
fn find_keyword(text: &str, keyword: &str) -> Option<usize> {
    let mut from = 0usize;
    while let Some(offset) = text[from..].find(keyword) {
        let at = from + offset;
        let before_ok = at > 0 && text[..at].ends_with(char::is_whitespace);
        let after = at + keyword.len();
        let rest = &text[after..];
        let after_ok = rest.is_empty() || rest.starts_with(char::is_whitespace);
        if before_ok && after_ok {
            return Some(at);
        }
        from = at + keyword.len();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<(String, ArgMatch)> {
        parse(text)
            .expect("parses")
            .args
            .into_iter()
            .map(|a| (a.path, a.matcher))
            .collect()
    }

    #[test]
    fn a_bare_tool_name_is_the_whole_expectation() {
        let expectation = parse("search_flights").expect("parses");
        assert_eq!(expectation.tool, "search_flights");
        assert!(expectation.args.is_empty());
    }

    #[test]
    fn where_clauses_parse_and_chain_with_and() {
        assert_eq!(
            args("create_booking where flight.id equals KQ311"),
            [("flight.id".into(), ArgMatch::Equals("KQ311".into()))]
        );
        assert_eq!(
            args("charge where amount equals 900 and currency equals EUR"),
            [
                ("amount".into(), ArgMatch::Equals("900".into())),
                ("currency".into(), ArgMatch::Equals("EUR".into())),
            ]
        );
        assert_eq!(
            args("book where passenger.name contains Casey"),
            [("passenger.name".into(), ArgMatch::Contains("Casey".into()))]
        );
        // `is` reads better than `equals` in some sentences and means the
        // same thing.
        assert_eq!(
            args("book where seat is 12A"),
            [("seat".into(), ArgMatch::Equals("12A".into()))]
        );
    }

    /// Volatile arguments assert shape, not value.
    #[test]
    fn existence_forms_take_no_value() {
        assert_eq!(
            args("book where idempotency_key exists"),
            [("idempotency_key".into(), ArgMatch::Exists)]
        );
        assert_eq!(
            args("book where coupon is absent"),
            [("coupon".into(), ArgMatch::Absent)]
        );
        // `is missing` is the same claim; accept it rather than teaching
        // the one true spelling.
        assert_eq!(
            args("book where coupon is missing"),
            [("coupon".into(), ArgMatch::Absent)]
        );
    }

    #[test]
    fn matches_parses_and_validates_the_pattern() {
        assert_eq!(
            args("book where seat matches [0-9]+[A-F]"),
            [("seat".into(), ArgMatch::Matches("[0-9]+[A-F]".into()))]
        );
        // A broken pattern is rejected at parse time, naming the value.
        let err = parse("book where seat matches [unclosed").expect_err("bad regex");
        assert!(err.to_string().contains("not a valid pattern"), "{err}");
    }

    /// `is absent` must win over `is`, or it would parse as
    /// `coupon equals "absent"` and quietly assert something else.
    #[test]
    fn the_suffix_forms_are_tried_before_the_value_forms() {
        assert_eq!(
            args("book where coupon is absent"),
            [("coupon".into(), ArgMatch::Absent)]
        );
        // And a value that merely CONTAINS the word still parses as a value.
        assert_eq!(
            args("book where note is absent minded"),
            [("note".into(), ArgMatch::Equals("absent minded".into()))]
        );
    }

    /// A value runs to the end of its clause, spaces and all. Quoting
    /// every id would be noise on the common case.
    #[test]
    fn values_may_contain_spaces() {
        assert_eq!(
            args("book where passenger.name equals Casey Jordan"),
            [(
                "passenger.name".into(),
                ArgMatch::Equals("Casey Jordan".into())
            )]
        );
    }

    /// Keywords are whole words. A tool or path that merely contains one
    /// must not be cut in half.
    #[test]
    fn keywords_match_only_as_whole_words() {
        let expectation = parse("where_is_it").expect("a tool may be named that");
        assert_eq!(expectation.tool, "where_is_it");

        assert_eq!(
            args("book where brand_and_model equals Airbus"),
            [("brand_and_model".into(), ArgMatch::Equals("Airbus".into()))]
        );
    }

    #[test]
    fn bad_input_says_what_was_wrong_and_shows_the_forms() {
        for (text, expected) in [
            ("book where", "nothing after it"),
            ("book where flight.id", "names no comparison"),
            ("book where flight.id equals", "no value after it"),
            ("book where equals KQ311", "not an argument path"),
            ("has spaces", "not a tool name"),
            ("book where a equals 1 and", "empty `and` clause"),
        ] {
            let err = parse(text).expect_err(text).to_string();
            assert!(
                err.contains(expected),
                "`{text}` should say {expected}: {err}"
            );
            // Every failure shows the accepted shapes and echoes what was
            // written, so an author can fix it without opening the docs.
            assert!(err.contains("Expected `<tool>[ where"), "{err}");
            assert!(err.contains(text.trim()), "{err}");
        }
    }
}
