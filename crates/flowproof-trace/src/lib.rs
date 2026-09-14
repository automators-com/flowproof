//! The flowproof trace format: what the recording agent writes and the
//! deterministic replayer consumes.
//!
//! A trace is JSON-lines: a versioned header line followed by one step per
//! line. The normative definition lives in `docs/trace-format.md` and the
//! JSON Schema in `schema/trace-v1.schema.json`; the serde types in this
//! crate are implemented against that schema.
//!
pub mod captures;
pub mod cassette;
pub mod cassette_diff;
pub mod egress;
pub mod format;
pub mod secret;
pub mod secret_scan;
pub mod side_effect;
pub mod substitution;
pub mod toolcalls;

pub use format::{Header, Step, TraceError, TraceLine};

/// Value of the `format` field in the trace header line.
pub const FORMAT_NAME: &str = "flowproof-trace";

/// Current trace format version.
pub const FORMAT_VERSION: u32 = 1;

/// The selector ladder: strategies tried in order during replay. Lower
/// discriminant = tried first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum SelectorTier {
    /// Accessibility-tree identity: role + accessible name (+ nearest named
    /// ancestor when that pair alone isn't unique on the page). Sourced from
    /// the browser's own computed accessibility tree (Chrome DevTools
    /// Protocol `Accessibility.getFullAXTree`), not from anything the page
    /// author had to opt into - works whether or not ARIA was authored
    /// explicitly, because the browser computes it either way. Ranked above
    /// `native_id` on evidence, not just argument: a live probe against a
    /// real SAPUI5 app (docs/fiori-reliability/FINDINGS.md, "real-system
    /// probe #1") showed `native_id` capturing generated, view-instance- and
    /// clone-index-bearing ids (`__xmlview1--...`, `__text6-__clone0`) for
    /// controls whose accessible name was stable across the same session.
    A11y = 0,
    /// Native stable ID (UIA AutomationId, SAP GUI Scripting ID, CSS/DOM id).
    NativeId = 1,
    /// Structural path through the accessibility/DOM tree.
    Structural = 2,
    /// OCR/text anchor plus a spatial relation.
    TextAnchor = 3,
    /// Visual template match.
    VisualTemplate = 4,
    /// AI relocation from the recorded intent (never silent: proposes a diff).
    AiRelocation = 5,
}

impl SelectorTier {
    /// All tiers, in the order replay attempts them.
    pub const LADDER: [SelectorTier; 6] = [
        SelectorTier::A11y,
        SelectorTier::NativeId,
        SelectorTier::Structural,
        SelectorTier::TextAnchor,
        SelectorTier::VisualTemplate,
        SelectorTier::AiRelocation,
    ];

    /// The tier's wire name (matches the serde/schema encoding).
    pub fn name(self) -> &'static str {
        match self {
            SelectorTier::A11y => "a11y",
            SelectorTier::NativeId => "native_id",
            SelectorTier::Structural => "structural",
            SelectorTier::TextAnchor => "text_anchor",
            SelectorTier::VisualTemplate => "visual_template",
            SelectorTier::AiRelocation => "ai_relocation",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_ordered_deterministic_first() {
        let mut sorted = SelectorTier::LADDER;
        sorted.sort();
        assert_eq!(sorted, SelectorTier::LADDER);
        assert_eq!(sorted[0], SelectorTier::A11y);
        assert_eq!(sorted[5], SelectorTier::AiRelocation);
    }

    /// The fix this tier exists for: on a real SAPUI5 app, a generated
    /// `native_id` proved less stable than the accessible name for the same
    /// control (FINDINGS.md, "real-system probe #1"). A ladder that doesn't
    /// try `a11y` before `native_id` doesn't get that benefit no matter how
    /// good the a11y selector is.
    #[test]
    fn a11y_is_tried_before_native_id() {
        let a11y_index = SelectorTier::LADDER
            .iter()
            .position(|t| *t == SelectorTier::A11y)
            .expect("a11y is in the ladder");
        let native_id_index = SelectorTier::LADDER
            .iter()
            .position(|t| *t == SelectorTier::NativeId)
            .expect("native_id is in the ladder");
        assert!(a11y_index < native_id_index);
    }

    #[test]
    fn tier_names_match_the_wire_encoding() {
        for tier in SelectorTier::LADDER {
            let wire = serde_json::to_value(tier).expect("tier serializes");
            assert_eq!(wire, serde_json::Value::String(tier.name().to_string()));
        }
    }

    #[test]
    fn format_identity() {
        assert_eq!(FORMAT_NAME, "flowproof-trace");
        assert_eq!(FORMAT_VERSION, 1);
    }
}
