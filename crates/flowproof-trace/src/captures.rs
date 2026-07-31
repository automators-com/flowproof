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
        let name = &after[..end];
        let Some(value) = captures.get(name) else {
            let mut names: Vec<&str> = captures.keys().map(String::as_str).collect();
            names.sort_unstable();
            return Err(format!(
                "capture '{name}' was never remembered ({})",
                if names.is_empty() {
                    "no captures in scope".to_string()
                } else {
                    format!("in scope: {}", names.join(", "))
                }
            ));
        };
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
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
    fn an_unterminated_reference_is_an_error_not_a_silent_passthrough() {
        let err = substitute("${captured.oid", &scope(&[("oid", "1")]))
            .expect_err("unterminated must fail");
        assert!(err.contains("unterminated"), "{err}");
    }
}
