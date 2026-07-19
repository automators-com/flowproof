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
}
