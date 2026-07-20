//! Secret indirection: `${VAR}` references in trace text.
//!
//! Sensitive values (passwords, tokens, PII) must never be persisted in a
//! trace. Instead of masking after the fact — which would break
//! deterministic replay — the trace stores an environment *reference*:
//! specs write `Type ${LOGIN_PASSWORD} into the password field`, the
//! engine resolves the reference from the environment at the moment of
//! use (recording AND every replay), and the trace only ever contains the
//! literal string `${LOGIN_PASSWORD}`.
//!
//! Syntax: `${NAME}` where `NAME` is `[A-Za-z_][A-Za-z0-9_]*`. Anything
//! else — `$X`, `${`, `${1BAD}` — is treated as literal text, so ordinary
//! values never trip the resolver.

/// A reference named a variable that is not set in the environment.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("secret ${{{var}}} is not set in the environment")]
pub struct MissingSecret {
    pub var: String,
}

fn parse_ref(text: &str) -> Option<(&str, usize)> {
    // `text` starts right after `${`; returns (name, chars consumed incl `}`).
    let end = text.find('}')?;
    let name = &text[..end];
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    valid.then_some((name, end + 1))
}

/// Does `text` contain at least one `${VAR}` reference?
pub fn has_refs(text: &str) -> bool {
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        if parse_ref(&rest[start + 2..]).is_some() {
            return true;
        }
        rest = &rest[start + 2..];
    }
    false
}

/// Resolve every `${VAR}` reference in `text` from the environment.
/// Fails on the first unset variable — a missing secret must never
/// silently degrade into typing the literal reference.
pub fn resolve_refs(text: &str) -> Result<String, MissingSecret> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match parse_ref(after) {
            Some((name, consumed)) => {
                let value = std::env::var(name).map_err(|_| MissingSecret {
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

/// Resolve `${VAR}` references inside a JSON value by walking its string
/// leaves: objects and arrays recurse, every `Value::String` goes through
/// [`resolve_refs`], keys and non-string scalars pass verbatim. This — not
/// serialize→resolve→reparse — because a resolved secret containing `"` or
/// `\` must land as DATA, never corrupt the JSON structure.
pub fn resolve_refs_in_json(value: &serde_json::Value) -> Result<serde_json::Value, MissingSecret> {
    use serde_json::Value;
    Ok(match value {
        Value::String(s) => Value::String(resolve_refs(s)?),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(resolve_refs_in_json)
                .collect::<Result<_, _>>()?,
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), resolve_refs_in_json(v)?)))
                .collect::<Result<_, MissingSecret>>()?,
        ),
        other => other.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through_untouched() {
        assert!(!has_refs("hello from flowproof"));
        assert_eq!(
            resolve_refs("hello from flowproof").expect("resolves"),
            "hello from flowproof"
        );
    }

    #[test]
    fn invalid_ref_shapes_are_literal() {
        for text in ["$VAR", "${", "${}", "${1BAD}", "${no space}", "$ {X}"] {
            assert!(!has_refs(text), "{text:?} must not count as a reference");
            assert_eq!(&resolve_refs(text).expect("resolves"), text);
        }
    }

    #[test]
    fn refs_resolve_from_the_environment() {
        std::env::set_var("FLOWPROOF_TEST_SECRET_A", "hunter2");
        std::env::set_var("FLOWPROOF_TEST_SECRET_B", "42");
        assert!(has_refs("pw: ${FLOWPROOF_TEST_SECRET_A}"));
        assert_eq!(
            resolve_refs("pw: ${FLOWPROOF_TEST_SECRET_A}, n=${FLOWPROOF_TEST_SECRET_B}!")
                .expect("resolves"),
            "pw: hunter2, n=42!"
        );
    }

    #[test]
    fn missing_variable_is_a_hard_error_naming_it() {
        let err = resolve_refs("${FLOWPROOF_TEST_SECRET_UNSET_XYZ}").expect_err("must fail");
        assert_eq!(err.var, "FLOWPROOF_TEST_SECRET_UNSET_XYZ");
        assert!(err
            .to_string()
            .contains("${FLOWPROOF_TEST_SECRET_UNSET_XYZ}"));
    }

    #[test]
    fn json_refs_resolve_at_string_leaves_only() {
        std::env::set_var("FLOWPROOF_TEST_JSON_TOKEN", "tok-123");
        let value = serde_json::json!({
            "provider": "postgres",
            "auth": "Bearer ${FLOWPROOF_TEST_JSON_TOKEN}",
            "nested": {"list": ["${FLOWPROOF_TEST_JSON_TOKEN}", 7, true, null]},
            "count": 42
        });
        let resolved = resolve_refs_in_json(&value).expect("resolves");
        assert_eq!(resolved["auth"], "Bearer tok-123");
        assert_eq!(resolved["nested"]["list"][0], "tok-123");
        // Non-string leaves and keys pass verbatim.
        assert_eq!(resolved["nested"]["list"][1], 7);
        assert_eq!(resolved["count"], 42);
        assert_eq!(resolved["provider"], "postgres");
    }

    #[test]
    fn json_resolution_survives_quote_bearing_secrets() {
        // A secret containing quotes and backslashes must land as data —
        // the reason resolution walks leaves instead of reparsing text.
        std::env::set_var("FLOWPROOF_TEST_JSON_QUOTED", r#"pa"ss\word"#);
        let value = serde_json::json!({"conn": "user:${FLOWPROOF_TEST_JSON_QUOTED}@host"});
        let resolved = resolve_refs_in_json(&value).expect("resolves");
        assert_eq!(resolved["conn"], r#"user:pa"ss\word@host"#);
        // And the result still serializes to valid JSON.
        let text = serde_json::to_string(&resolved).expect("serializes");
        let back: serde_json::Value = serde_json::from_str(&text).expect("round-trips");
        assert_eq!(back, resolved);
    }

    #[test]
    fn json_missing_variable_fails_hard() {
        let value = serde_json::json!(["${FLOWPROOF_TEST_JSON_UNSET_XYZ}"]);
        let err = resolve_refs_in_json(&value).expect_err("must fail");
        assert_eq!(err.var, "FLOWPROOF_TEST_JSON_UNSET_XYZ");
    }
}
