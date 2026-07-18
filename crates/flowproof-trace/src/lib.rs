//! The flowproof trace format: what the recording agent writes and the
//! deterministic replayer consumes.
//!
//! A trace is JSON-lines: a versioned header line followed by one step per
//! line. The normative definition lives in `docs/trace-format.md` and the
//! JSON Schema in `schema/trace-v1.schema.json`; the serde types in this
//! crate are implemented against that schema.
//!
//! Serde types land once the v1 schema is approved; until then this crate
//! only exposes the format identity and the selector-ladder ordering.

/// Value of the `format` field in the trace header line.
pub const FORMAT_NAME: &str = "flowproof-trace";

/// Current trace format version.
pub const FORMAT_VERSION: u32 = 1;

/// The selector ladder: strategies tried in order during replay. Lower
/// discriminant = tried first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SelectorTier {
    /// Native stable ID (UIA AutomationId, SAP GUI Scripting ID, CSS/DOM id).
    NativeId = 0,
    /// Structural path through the accessibility/DOM tree.
    Structural = 1,
    /// OCR/text anchor plus a spatial relation.
    TextAnchor = 2,
    /// Visual template match.
    VisualTemplate = 3,
    /// AI relocation from the recorded intent (never silent: proposes a diff).
    AiRelocation = 4,
}

impl SelectorTier {
    /// All tiers, in the order replay attempts them.
    pub const LADDER: [SelectorTier; 5] = [
        SelectorTier::NativeId,
        SelectorTier::Structural,
        SelectorTier::TextAnchor,
        SelectorTier::VisualTemplate,
        SelectorTier::AiRelocation,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_ordered_deterministic_first() {
        let mut sorted = SelectorTier::LADDER;
        sorted.sort();
        assert_eq!(sorted, SelectorTier::LADDER);
        assert_eq!(sorted[0], SelectorTier::NativeId);
        assert_eq!(sorted[4], SelectorTier::AiRelocation);
    }

    #[test]
    fn format_identity() {
        assert_eq!(FORMAT_NAME, "flowproof-trace");
        assert_eq!(FORMAT_VERSION, 1);
    }
}
