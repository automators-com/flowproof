//! Shared draft-assembly logic between authoring paths that turn some
//! external source (a screen recording, a requirement document, …) into
//! a `.flow.yaml` draft. Both need the identical "flag, don't guess"
//! mechanism for anything a model can't map to flowproof's step
//! grammar, and both render the same underlying step vocabulary.

use crate::FlowSpec;

/// The step forms an authoring model's replies are constrained to, and
/// the quoting discipline that makes them parseable by flowproof's real
/// grammar (`rules.rs`): a quoted label is a text-anchor search, bare
/// text is parsed as a literal internal element id and will never match
/// anything real.
pub const STEP_GRAMMAR_RULES: &str = "\
<label> is always the element's VISIBLE text, in double quotes, exactly \
as flowproof's own step grammar expects it (unquoted text there is \
parsed as a literal internal element id, not a label to search for - it \
will never match anything real). <value> and <option> are the opposite: \
plain text, NEVER quoted - example: for a field visibly labelled Order \
Type where you typed OR, write exactly Type OR into the \"Order Type\" \
field, not \"OR\". \"Press Enter\" is its own distinct form, for a KEY \
NAME, and is NEVER quoted either - write exactly Press Enter, not Press \
\"Enter\" - do not confuse it with Press the \"<label>\" button, which \
is for clicking something and DOES need its label quoted:
- \"Go to /nVA01\" (a transaction code in the command field)
- Type <value> into the \"<label>\" field
- Press Enter (bare, no quotes - a key name, not a button label)
- Press the \"<label>\" button
- Select <option> from the \"<label>\" field";

/// Marks a step whose exact action a source (video, document, …) could
/// not determine — carried through as a real (freeform) step, not a
/// comment, so the existing live authoring agent resolves it against
/// the real screen when `flowproof record` runs, instead of a human
/// retyping it by hand.
pub const FLAGGED_MARKER: &str = "__FLAGGED__: ";

/// One line of a drafted spec, before it's rendered to YAML.
pub enum DraftLine {
    /// A step in flowproof's grammar, verbatim.
    Action(String),
    /// An `assert:` step — just the assertion text, not the `assert:`
    /// keyword itself.
    Assert(String),
    /// The observation behind a [`FLAGGED_MARKER`]-prefixed step,
    /// already stripped of the marker — renders as a `# TODO` comment
    /// plus a real freeform step.
    Flagged(String),
}

/// Double-quoted YAML scalar, safe for arbitrary model-generated text.
pub fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Assembles a draft spec and validates it parses before returning — a
/// bad model reply fails here, not silently. `header_comment` carries
/// the caller's own provenance/review-instructions text (source-specific
/// wording lives with the caller, not here).
pub fn assemble(
    header_comment: &str,
    name: &str,
    app: &str,
    lines: &[DraftLine],
) -> Result<String, crate::spec::SpecError> {
    let mut yaml = format!(
        "{header_comment}\nname: {}\napp: {}\nsteps:\n",
        yaml_quote(name),
        yaml_quote(app),
    );
    for line in lines {
        match line {
            DraftLine::Action(step) => {
                yaml.push_str(&format!("  - {}\n", yaml_quote(step)));
            }
            DraftLine::Assert(text) => {
                yaml.push_str(&format!("  - assert: {}\n", yaml_quote(text)));
            }
            DraftLine::Flagged(observed) => {
                yaml.push_str(
                    "  # TODO: unexplained step here — flagged for the live\n  \
                     # authoring agent to resolve when `flowproof record` runs, not\n  \
                     # dropped silently. Rewrite by hand first if you already know\n  \
                     # what this should be.\n",
                );
                yaml.push_str(&format!(
                    "  - {}\n",
                    yaml_quote(&format!(
                        "resolve the action needed here against the live screen — a \
                         keyboard-only action like Enter may be the cause. Observed: {observed}"
                    ))
                ));
            }
        }
    }
    FlowSpec::parse(&yaml)?;
    Ok(yaml)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assemble_renders_actions_asserts_and_flags() {
        let lines = vec![
            DraftLine::Action("Go to /nVA01".to_string()),
            DraftLine::Assert("page shows Create Standard Order".to_string()),
            DraftLine::Flagged("the screen changed to an order overview".to_string()),
        ];
        let yaml = assemble("# DRAFT", "n", "sap", &lines).expect("assembles");
        assert!(yaml.contains("- \"Go to /nVA01\""));
        assert!(yaml.contains("- assert: \"page shows Create Standard Order\""));
        assert!(yaml.contains("TODO"), "flagged step must be visibly marked");
        assert!(
            !yaml.contains(FLAGGED_MARKER),
            "internal marker must not leak into the draft"
        );
        let spec = FlowSpec::parse(&yaml).expect("draft parses");
        assert_eq!(spec.steps.len(), 3);
        assert!(spec
            .steps
            .iter()
            .any(|s| matches!(s, crate::spec::SpecStep::Assert { .. })));
    }

    #[test]
    fn assemble_with_no_lines_surfaces_flow_specs_own_empty_error() {
        // draft_assembly itself takes no position on "must have steps" -
        // callers with their own emptiness semantics (video never emits
        // empty; doc requires at least one) check that themselves before
        // calling assemble(). This just confirms assemble() doesn't
        // silently succeed with zero steps - FlowSpec::parse's own
        // Empty-steps rule catches it.
        let err = assemble("# DRAFT", "n", "sap", &[]).expect_err("empty steps must not parse");
        assert!(matches!(err, crate::spec::SpecError::Empty));
    }
}
