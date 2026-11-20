//! Flow-scoped capture references: `${captured.<name>}`.
//!
//! A sibling of [`crate::secret`] rather than part of it — a capture is not
//! a secret, but both are references the trace stores *unresolved* and both
//! resolve at execution time, on record and on every replay alike. Lives
//! here because the recorder and the replayer each need it and neither
//! depends on the other.

/// The opening of a capture reference.
pub const OPEN: &str = "${captured.";

/// Substitute `${captured.<name>}` against the flow's captures.
///
/// Resolves at EXECUTION time — on record and on every replay — so like a
/// `${VAR}` secret, only the reference ever enters the trace. `${VAR}`
/// resolution deliberately does not do this: its names may not contain a
/// dot, so `${captured.x}` passes through it untouched, and before this
/// existed that literal was what got typed.
///
/// Captures only: `${VAR}` is left as written. Where a template can hold
/// both, use [`resolve`]; substituting here and then resolving `${VAR}`
/// over the result would read references out of captured values.
pub fn substitute(
    text: &str,
    captures: &std::collections::HashMap<String, String>,
) -> Result<String, String> {
    if !text.contains(OPEN) {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            return Err(format!("unterminated {OPEN}…}} reference in '{text}'"));
        };
        out.push_str(lookup(&after[..end], captures)?);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The value remembered as `name`, or an error naming what IS in scope.
fn lookup<'a>(
    name: &str,
    captures: &'a std::collections::HashMap<String, String>,
) -> Result<&'a str, String> {
    captures.get(name).map(String::as_str).ok_or_else(|| {
        let mut names: Vec<&str> = captures.keys().map(String::as_str).collect();
        names.sort_unstable();
        format!(
            "capture '{name}' was never remembered ({})",
            if names.is_empty() {
                "no captures in scope".to_string()
            } else {
                format!("in scope: {}", names.join(", "))
            }
        )
    })
}

/// Why [`resolve`] could not fill a template in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Unresolved {
    /// An unknown or unterminated `${captured.<name>}`.
    #[error("{0}")]
    Capture(String),
    /// A `${VAR}` that is not set in the environment.
    #[error(transparent)]
    Secret(#[from] crate::secret::MissingSecret),
}

/// Fill in `${captured.<name>}` from the flow's captures and `${VAR}` from
/// the environment, in ONE pass over the template. What goes in is never
/// scanned again: a captured value is text the app under test showed, so a
/// `${SECRET}` inside one is that text, not a reference. Substituting
/// captures and then resolving `${VAR}` over the result let the app under
/// test name any environment variable, secrets included, and have its value
/// typed back or sent on. A secret's value is likewise never read for
/// `${captured.…}`. Use this, not [`substitute`] followed by
/// [`crate::secret::resolve_refs`], wherever a template can hold both.
pub fn resolve(
    text: &str,
    captures: &std::collections::HashMap<String, String>,
) -> Result<String, Unresolved> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        if let Some(after) = rest[start..].strip_prefix(OPEN) {
            let Some(end) = after.find('}') else {
                return Err(Unresolved::Capture(format!(
                    "unterminated {OPEN}…}} reference in '{text}'"
                )));
            };
            out.push_str(lookup(&after[..end], captures).map_err(Unresolved::Capture)?);
            rest = &after[end + 1..];
            continue;
        }
        let after = &rest[start + 2..];
        match crate::secret::parse_ref(after) {
            Some((name, consumed)) => {
                let value = std::env::var(name).map_err(|_| crate::secret::MissingSecret {
                    var: name.to_string(),
                })?;
                out.push_str(&value);
                rest = &after[consumed..];
            }
            None => {
                out.push_str("${");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// [`resolve`] every string leaf of a JSON value (an `assert_api` body),
/// leaving keys and non-string leaves untouched. Leaf by leaf, so a value
/// containing `"` or `\` lands as data and never changes the structure.
pub fn resolve_json(
    value: &serde_json::Value,
    captures: &std::collections::HashMap<String, String>,
) -> Result<serde_json::Value, Unresolved> {
    Ok(match value {
        serde_json::Value::String(s) => serde_json::Value::String(resolve(s, captures)?),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .map(|v| resolve_json(v, captures))
                .collect::<Result<_, _>>()?,
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), resolve_json(v, captures)?)))
                .collect::<Result<_, Unresolved>>()?,
        ),
        other => other.clone(),
    })
}

/// The capture names `text` references, in order, repeats included. An
/// unterminated reference ends the scan; [`substitute`] reports it.
pub fn capture_refs(text: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            break;
        };
        names.push(&after[..end]);
        rest = &after[end + 1..];
    }
    names
}

/// The capture names a recorded step remembers: a `Capture` or
/// `CaptureDownload` step's `name`, and the keys of an `assert_api` step's
/// `expect.capture`.
pub fn produced_by(step: &crate::Step) -> Vec<String> {
    use crate::format::{Action, Assertion};
    match &step.action {
        Action::Capture(params) | Action::CaptureDownload(params) => params
            .get("name")
            .and_then(|n| n.as_str())
            .map(|n| vec![n.to_string()])
            .unwrap_or_default(),
        Action::Assert(Assertion::Api {
            expect: Some(expect),
            ..
        }) => expect
            .get("capture")
            .and_then(|c| c.as_object())
            .map(|c| c.keys().cloned().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The capture names a recorded step reads: every `${captured.x}` anywhere
/// in its action or the conditions it runs under, and the name an
/// `element_state` assertion compares against (`expect.capture`).
pub fn used_by(step: &crate::Step) -> Vec<String> {
    use crate::format::{Action, Assertion};
    let mut names: Vec<String> = Vec::new();
    let mut scan = |value: serde_json::Value| {
        let text = value.to_string();
        for name in capture_refs(&text) {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    };
    scan(serde_json::to_value(&step.action).unwrap_or_default());
    scan(serde_json::to_value(&step.guards).unwrap_or_default());
    scan(serde_json::to_value(&step.repeat).unwrap_or_default());
    if let Action::Assert(Assertion::ElementState { expect, .. }) = &step.action {
        if let Some(name) = expect.get("capture").and_then(|c| c.as_str()) {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn scope(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn a_reference_resolves_to_the_remembered_value() {
        let c = scope(&[("oid", "1060049")]);
        assert_eq!(
            substitute("${captured.oid}", &c).expect("resolves"),
            "1060049"
        );
        // Surrounding text and repeats survive.
        assert_eq!(
            substitute("id ${captured.oid}/${captured.oid}!", &c).expect("resolves"),
            "id 1060049/1060049!"
        );
    }

    #[test]
    fn text_without_a_reference_is_returned_unchanged() {
        let c = scope(&[("oid", "1")]);
        assert_eq!(substitute("plain text", &c).expect("no refs"), "plain text");
        // A `${VAR}` secret is NOT ours to resolve: it passes through for the
        // secret resolver, which runs next.
        assert_eq!(
            substitute("${TOKEN}", &c).expect("secret refs pass through"),
            "${TOKEN}"
        );
    }

    /// The failure that matters: before this existed, `${captured.oid}` was
    /// typed into the field LITERALLY, because a `${VAR}` name may not
    /// contain a dot so the secret resolver left it alone. Silently entering
    /// the wrong value is the worst outcome available, so an unknown name is
    /// an error that names what IS in scope.
    /// The documented claim, pinned: a typed capture is INTERPOLATED, not
    /// evaluated. `12 + 30` is three tokens of displayed text, and the
    /// reason it is worth a test of its own is that it looks close enough
    /// to an answer for a flow to go green on it while asserting nothing
    /// anybody meant. If arithmetic is ever added it must be a spelling
    /// that cannot be confused with this one, and this test is what says so.
    #[test]
    fn a_typed_capture_is_interpolated_and_never_evaluated() {
        let c = scope(&[("a", "12"), ("b", "30")]);
        assert_eq!(
            substitute("${captured.a} + ${captured.b}", &c).expect("both resolve"),
            "12 + 30",
            "interpolation substitutes; it does not compute"
        );
        // Literal text around a reference is typed as written, and several
        // references in one step each resolve.
        let c = scope(&[("first", "Grace"), ("last", "Hopper"), ("oid", "1061367")]);
        assert_eq!(
            substitute("${captured.first} ${captured.last}", &c).expect("both resolve"),
            "Grace Hopper"
        );
        assert_eq!(
            substitute("order-${captured.oid}", &c).expect("resolves"),
            "order-1061367"
        );
    }

    #[test]
    fn an_unremembered_name_is_an_error_that_lists_the_scope() {
        let err = substitute("${captured.typo}", &scope(&[("oid", "1"), ("amount", "2")]))
            .expect_err("unknown capture must fail");
        assert!(err.contains("typo"), "names the miss: {err}");
        assert!(err.contains("amount, oid"), "lists scope, sorted: {err}");

        let err = substitute("${captured.oid}", &scope(&[])).expect_err("empty scope must fail");
        assert!(err.contains("no captures in scope"), "{err}");
    }

    #[test]
    fn json_string_leaves_resolve_and_the_rest_is_kept() {
        std::env::set_var("FLOWPROOF_TEST_CAPTURE_JSON_TOKEN", "tok-123");
        let c = scope(&[("oid", "1060049")]);
        let body = serde_json::json!({
            "env": {"ORDER": "${captured.oid}", "TOKEN": "${FLOWPROOF_TEST_CAPTURE_JSON_TOKEN}"},
            "ids": ["x-${captured.oid}", 7, true],
        });
        assert_eq!(
            resolve_json(&body, &c).expect("resolves"),
            serde_json::json!({
                "env": {"ORDER": "1060049", "TOKEN": "tok-123"},
                "ids": ["x-1060049", 7, true],
            })
        );
        assert!(resolve_json(&serde_json::json!(["${captured.nope}"]), &c).is_err());
    }

    #[test]
    fn an_unterminated_reference_is_an_error_not_a_silent_passthrough() {
        let err = substitute("${captured.oid", &scope(&[("oid", "1")]))
            .expect_err("unterminated must fail");
        assert!(err.contains("unterminated"), "{err}");
    }

    #[test]
    fn capture_refs_lists_every_reference_in_order() {
        assert_eq!(
            capture_refs("${captured.a} then ${TOKEN} and ${captured.b}/${captured.a}"),
            ["a", "b", "a"]
        );
        assert!(capture_refs("plain").is_empty());
        assert_eq!(capture_refs("${captured.a} ${captured.open"), ["a"]);
    }

    #[test]
    fn a_step_reports_what_it_remembers_and_what_it_reads() {
        let step = |action: serde_json::Value| -> crate::Step {
            serde_json::from_value(serde_json::json!({
                "id": "s0001",
                "intent": "x",
                "action": action,
                "selectors": [],
                "sync": {"pre": [], "post": []},
                "artifacts": {},
            }))
            .expect("step")
        };
        let capture = step(serde_json::json!({"type": "capture", "params": {"name": "order"}}));
        assert_eq!(produced_by(&capture), ["order"]);
        assert!(used_by(&capture).is_empty());

        let typed = step(serde_json::json!({
            "type": "type_text",
            "params": {"text": "${captured.order}-${captured.line}"}
        }));
        assert!(produced_by(&typed).is_empty());
        assert_eq!(used_by(&typed), ["order", "line"]);

        let compare = step(serde_json::json!({
            "type": "assert",
            "params": {"kind": "element_state", "expect": {"capture": "balance"}}
        }));
        assert_eq!(used_by(&compare), ["balance"]);
    }

    /// The app under test decides what a capture holds, so a `${SECRET}` in
    /// a captured value must reach the field as that text. Resolving it
    /// would let the app name any environment variable and have its value
    /// typed back or sent on to an API host.
    #[test]
    fn a_captured_value_is_never_read_for_env_references() {
        std::env::set_var("FLOWPROOF_TEST_CAPTURE_SECRET", "hunter2");
        let c = scope(&[
            ("note", "${FLOWPROOF_TEST_CAPTURE_SECRET}"),
            ("file", "report-${date}.xlsx"),
        ]);
        assert_eq!(
            resolve("${captured.note}", &c).expect("resolves"),
            "${FLOWPROOF_TEST_CAPTURE_SECRET}",
            "the value is text, not a reference"
        );
        // A legitimate `${…}` in captured text no longer fails as an unset
        // secret.
        assert_eq!(
            resolve("/downloads/${captured.file}", &c).expect("resolves"),
            "/downloads/report-${date}.xlsx"
        );
        // A reference the flow itself wrote still resolves beside a capture.
        assert_eq!(
            resolve("${FLOWPROOF_TEST_CAPTURE_SECRET}:${captured.note}", &c).expect("resolves"),
            "hunter2:${FLOWPROOF_TEST_CAPTURE_SECRET}"
        );
        let body = serde_json::json!({"memo": "${captured.note}", "n": 1});
        assert_eq!(
            resolve_json(&body, &c).expect("resolves"),
            serde_json::json!({"memo": "${FLOWPROOF_TEST_CAPTURE_SECRET}", "n": 1})
        );
    }

    #[test]
    fn a_secret_value_is_never_read_for_capture_references() {
        std::env::set_var("FLOWPROOF_TEST_CAPTURE_REF_IN_SECRET", "${captured.oid}");
        let c = scope(&[("oid", "1060049")]);
        assert_eq!(
            resolve("${FLOWPROOF_TEST_CAPTURE_REF_IN_SECRET}", &c).expect("resolves"),
            "${captured.oid}"
        );
    }

    #[test]
    fn resolve_fails_by_kind() {
        let c = scope(&[("oid", "1")]);
        assert!(matches!(
            resolve("${captured.typo}", &c),
            Err(Unresolved::Capture(why)) if why.contains("typo") && why.contains("oid")
        ));
        assert!(matches!(
            resolve("${captured.oid", &c),
            Err(Unresolved::Capture(why)) if why.contains("unterminated")
        ));
        assert_eq!(
            resolve("${FLOWPROOF_TEST_CAPTURE_UNSET}", &c),
            Err(Unresolved::Secret(crate::secret::MissingSecret {
                var: "FLOWPROOF_TEST_CAPTURE_UNSET".to_string()
            }))
        );
        // Not a reference of either kind: kept as written.
        assert_eq!(
            resolve("$X ${ ${1BAD}", &c).expect("literal"),
            "$X ${ ${1BAD}"
        );
    }
}
