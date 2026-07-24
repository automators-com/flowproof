//! The `assert_no_secret_leak` corpus scan, shared verbatim by every flow
//! kind that captures an observable output corpus (agent, web, api).
//!
//! The control's lane is a CLOSED corpus of the run's observable outputs; the
//! forbidden event is an occurrence of a resolved secret VALUE in that corpus.
//! The scan is a pure function of `(resolved values, corpus)`: it resolves
//! each asserted `${VAR}` through [`crate::secret::resolve_refs`] and
//! substring-scans the in-memory corpus. Because both the resolved values and
//! the re-observed corpus are the same at record and every replay, an
//! unchanged system yields the same verdict - the `page shows` determinism
//! grade, never a diff against a stored corpus.
//!
//! Only variable NAMES ever travel through this module: a failure names the
//! variable, the corpus element it appeared in, and the asserting step - never
//! the value. A resolved value too short to scan precisely is REFUSED at
//! execution (both phases), in the same shape as the `MissingSecret` error.

use crate::secret::resolve_refs;

/// The smallest resolved secret length the corpus scan will accept.
/// Scanning for a one- or two-character value would fire on almost any output
/// (the design's `"1"` example), so a secret shorter than this is REFUSED at
/// execution rather than asserted imprecisely - a control that cannot be
/// checked precisely is refused, not weakened.
pub const MIN_SECRET_LEN: usize = 4;

/// One `assert_no_secret_leak` step: the `${VAR}` selectors it declares and
/// its 1-based position in the flow's `steps:` (named in the failure message).
/// Only the variable NAMES live here - never a resolved value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeakAssertion {
    pub step_index: usize,
    pub selectors: Vec<String>,
}

/// Whether an app kind exposes a readable output corpus this control can scan.
/// `web` (surface text at step boundaries + `assert_api` bodies), `api`
/// (`assert_api` bodies), and `agent` (model-boundary trajectory + MCP lanes)
/// do; every other kind fails as a CAPABILITY error rather than certifying
/// nothing - the same honesty rule `assert_no_egress` follows.
pub fn has_readable_corpus(app: &str) -> bool {
    matches!(app, "web" | "api" | "agent")
}

/// The capability-error message for a flow kind that cannot observe a
/// scannable corpus. A control that cannot be asserted is refused, never
/// laundered into a vacuous pass.
pub fn capability_error(app: &str) -> String {
    format!(
        "assert_no_secret_leak cannot certify on an `app: {app}` flow: this flow kind \
         exposes no readable output corpus to scan (surface text or assert_api response \
         bodies); a control that cannot be checked is refused, not vacuously passed"
    )
}

/// Scan `corpus` (a list of `(element-label, text)`) for any declared secret.
///
/// For each assertion, every `${VAR}` selector resolves through the shared
/// resolve-refs machinery; a resolved value under [`MIN_SECRET_LEN`] is
/// refused; otherwise the value is substring-scanned across every corpus
/// element. A leak names ALL matching variables in a stable (sorted) order,
/// the corpus element each appeared in, and the asserting step index - never
/// the value. Returns `Ok(())` when nothing leaked.
pub fn scan_corpus(
    assertions: &[LeakAssertion],
    corpus: &[(String, String)],
) -> Result<(), String> {
    for leak in assertions {
        // (selector, corpus element) for every variable that appeared,
        // collected then sorted so multiple leaks report in a stable order.
        let mut hits: Vec<(String, String)> = Vec::new();
        for selector in &leak.selectors {
            let value = resolve_refs(selector).map_err(|e| e.to_string())?;
            if value.chars().count() < MIN_SECRET_LEN {
                // Named like MissingSecret: the variable and the minimum,
                // never the value.
                return Err(format!(
                    "assert_no_secret_leak (step {}): {selector} resolves to a value shorter \
                     than the {MIN_SECRET_LEN}-character minimum needed to scan for it \
                     precisely; a secret that short cannot be asserted without false positives",
                    leak.step_index
                ));
            }
            if let Some((element, _)) = corpus.iter().find(|(_, text)| text.contains(&value)) {
                hits.push((selector.clone(), element.clone()));
            }
        }
        if !hits.is_empty() {
            hits.sort();
            let list = hits
                .iter()
                .map(|(selector, element)| format!("{selector} in {element}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "assert_no_secret_leak (step {}): a declared secret appeared in the run \
                 output: {list}",
                leak.step_index
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(var: &str, val: &str) {
        // SAFETY: single-threaded test setup, no other threads read env here.
        unsafe { std::env::set_var(var, val) };
    }

    #[test]
    fn clean_corpus_passes() {
        set("FLOWPROOF_SCAN_TEST_PW", "hunter2-long-enough");
        let assertions = vec![LeakAssertion {
            step_index: 3,
            selectors: vec!["${FLOWPROOF_SCAN_TEST_PW}".into()],
        }];
        let corpus = vec![("the surface text".into(), "welcome, no secrets here".into())];
        assert!(scan_corpus(&assertions, &corpus).is_ok());
    }

    #[test]
    fn a_leak_names_the_variable_the_element_and_the_step_never_the_value() {
        set("FLOWPROOF_SCAN_TEST_PW2", "s3cr3t-connection-string");
        let assertions = vec![LeakAssertion {
            step_index: 5,
            selectors: vec!["${FLOWPROOF_SCAN_TEST_PW2}".into()],
        }];
        let corpus = vec![(
            "an assert_api response body".into(),
            "{\"error\":\"cannot connect with s3cr3t-connection-string\"}".into(),
        )];
        let err = scan_corpus(&assertions, &corpus).expect_err("must catch the leak");
        assert!(err.contains("step 5"), "{err}");
        assert!(err.contains("${FLOWPROOF_SCAN_TEST_PW2}"), "{err}");
        assert!(err.contains("an assert_api response body"), "{err}");
        // The VALUE must never appear in the message.
        assert!(!err.contains("s3cr3t-connection-string"), "{err}");
    }

    #[test]
    fn multiple_leaks_report_in_a_stable_sorted_order() {
        set("FLOWPROOF_SCAN_TEST_A", "alpha-value-long");
        set("FLOWPROOF_SCAN_TEST_B", "bravo-value-long");
        let assertions = vec![LeakAssertion {
            step_index: 2,
            // Declared B-then-A; the message must still sort them.
            selectors: vec![
                "${FLOWPROOF_SCAN_TEST_B}".into(),
                "${FLOWPROOF_SCAN_TEST_A}".into(),
            ],
        }];
        let corpus = vec![(
            "elem".into(),
            "alpha-value-long and bravo-value-long".into(),
        )];
        let err = scan_corpus(&assertions, &corpus).expect_err("both leak");
        let a = err.find("${FLOWPROOF_SCAN_TEST_A}").expect("A named");
        let b = err.find("${FLOWPROOF_SCAN_TEST_B}").expect("B named");
        assert!(a < b, "sorted order puts A before B: {err}");
    }

    #[test]
    fn a_too_short_secret_is_refused_naming_the_variable_not_the_value() {
        set("FLOWPROOF_SCAN_TEST_SHORT", "ab");
        let assertions = vec![LeakAssertion {
            step_index: 1,
            selectors: vec!["${FLOWPROOF_SCAN_TEST_SHORT}".into()],
        }];
        let corpus = vec![("elem".into(), "ab appears everywhere".into())];
        let err = scan_corpus(&assertions, &corpus).expect_err("too short is refused");
        assert!(err.contains("${FLOWPROOF_SCAN_TEST_SHORT}"), "{err}");
        assert!(err.contains(&MIN_SECRET_LEN.to_string()), "{err}");
        // Even a refusal never prints the value.
        assert!(!err.contains("'ab'"), "{err}");
    }

    #[test]
    fn only_web_api_and_agent_have_a_readable_corpus() {
        assert!(has_readable_corpus("web"));
        assert!(has_readable_corpus("api"));
        assert!(has_readable_corpus("agent"));
        assert!(!has_readable_corpus("calc"));
        assert!(!has_readable_corpus("sap"));
        assert!(!has_readable_corpus("vision"));
    }
}
