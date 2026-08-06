//! Parses the text layer of an HP ALM / Quality Center UAT test-run PDF
//! export into per-step records. Scoped narrowly to the one export
//! template observed so far (`Step Name :` / `Description:` / `Expected`
//! / `Actual` / `Run Step Attachments` blocks) — a different export tool
//! (qTest, Zephyr, Azure DevOps, a plain Word doc) gets its own sibling
//! module in `doc_formats/`, not a rewrite of this one.

use regex::Regex;

/// One test-case step, as extracted from the document's text layer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocStepRecord {
    pub step_name: String,
    /// The action taken — becomes flowproof step line(s).
    pub description: String,
    /// The documented outcome — already an assertion in prose, unlike
    /// video-authoring which never has one.
    pub expected: String,
    /// UAT run commentary (often describing what actually went WRONG,
    /// not the expectation) — never sent to the model or used to derive
    /// an assert; rendered only as human context.
    pub actual: String,
}

#[derive(Debug, thiserror::Error)]
pub enum HpAlmParseError {
    #[error("no 'Step Name :' blocks found in the document — nothing to draft")]
    NoStepsFound,
}

/// Collapses arbitrary PDF-extraction whitespace (wrapped lines, runs of
/// spaces, blank lines) into single-spaced prose.
fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Segments the document's extracted text into step records. Anchors on
/// `^Step Name\s*:\s*(.+)$` (line-start only, via multiline mode) to
/// find block boundaries — the SAME label reappears mid-line later in
/// each block's own metadata table ("Exec Time: ... Step Name: Step 1"),
/// but never as the first token on its own line, so the anchor never
/// double-matches within one block.
pub fn parse(text: &str) -> Result<Vec<DocStepRecord>, HpAlmParseError> {
    let step_start =
        Regex::new(r"(?m)^Step Name\s*:\s*(.+)$").expect("static regex is well-formed");
    let starts: Vec<_> = step_start.captures_iter(text).collect();
    if starts.is_empty() {
        return Err(HpAlmParseError::NoStepsFound);
    }

    // `Description:` carries a literal colon; `Expected`/`Actual` do
    // not, in the real template - anchors mirror that exactly rather
    // than normalizing the asymmetry away.
    let description_re = Regex::new(r"(?s)Description:\s*(.*?)\s*\bExpected\b")
        .expect("static regex is well-formed");
    let expected_re =
        Regex::new(r"(?s)\bExpected\b\s*(.*?)\s*\bActual\b").expect("static regex is well-formed");
    // Stops at "Run Step Attachments" OR end of this step's own block
    // slice (`\z` here means end of `block`, not end of the document) -
    // a step with no attachment still terminates cleanly.
    let actual_re = Regex::new(r"(?s)\bActual\b\s*(.*?)\s*(?:\bRun Step Attachments\b|\z)")
        .expect("static regex is well-formed");

    let mut records = Vec::with_capacity(starts.len());
    for (i, cap) in starts.iter().enumerate() {
        let block_start = cap.get(0).expect("group 0 always matches").start();
        let block_end = starts
            .get(i + 1)
            .map(|next| next.get(0).expect("group 0 always matches").start())
            .unwrap_or(text.len());
        let block = &text[block_start..block_end];

        let step_name = normalize(cap.get(1).expect("capture group 1 is required").as_str());
        let description = description_re
            .captures(block)
            .map(|c| normalize(&c[1]))
            .unwrap_or_default();
        let expected = expected_re
            .captures(block)
            .map(|c| normalize(&c[1]))
            .unwrap_or_default();
        let actual = actual_re
            .captures(block)
            .map(|c| normalize(&c[1]))
            .unwrap_or_default();

        records.push(DocStepRecord {
            step_name,
            description,
            expected,
            actual,
        });
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic, hand-crafted fixture matching the real export's field
    /// layout - deliberately NOT the real uploaded document's content
    /// (which contains a real tester's name/email and real project
    /// data). Structure is what these tests exercise, not the words.
    const SAMPLE_EXPORT: &str = "\
Run ID : 1234 - Run_Sample
Field Label Field Value Field Label Field Value
Baseline: Operating System:

Step Name : Step 1
Field Label Field Value Field Label Field Value
Condition: Source Test: 9999
Exec Date: 1/1/25 Test Step
Status:
Passed
Exec Time: 09:00:00 Step Name: Step 1

Description:
Open the sample application and log in using the provided credentials.

Expected
User is successfully logged into the sample application.

Actual


Run Step Attachments
File Name: RichContentImage_sample_1.png
Description:


Step Name : Step 2
Field Label Field Value Field Label Field Value
Condition: Source Test: 9999
Exec Date: 1/1/25 Test Step
Status:
Passed
Exec Time: 09:05:00 Step Name: Step 2

Description:
Click on the 'Widgets' button to open the widgets module.

Expected
User is on the 'Widgets Overview' screen.

Actual
Loaded slower than expected but succeeded.


Run Step Attachments
File Name: RichContentImage_sample_2.png
Description: shows the widgets overview screen after loading
";

    #[test]
    fn parse_segments_a_realistic_export_into_step_records() {
        let records = parse(SAMPLE_EXPORT).expect("parses");
        assert_eq!(records.len(), 2);

        assert_eq!(records[0].step_name, "Step 1");
        assert_eq!(
            records[0].description,
            "Open the sample application and log in using the provided credentials."
        );
        assert_eq!(
            records[0].expected,
            "User is successfully logged into the sample application."
        );
        assert_eq!(records[0].actual, "");

        assert_eq!(records[1].step_name, "Step 2");
        assert_eq!(
            records[1].description,
            "Click on the 'Widgets' button to open the widgets module."
        );
        assert_eq!(
            records[1].expected,
            "User is on the 'Widgets Overview' screen."
        );
        assert_eq!(
            records[1].actual,
            "Loaded slower than expected but succeeded."
        );
    }

    #[test]
    fn parse_does_not_leak_the_attachment_description_field_into_a_step() {
        // Step 2's fixture attachment has a NON-EMPTY description
        // ("shows the widgets overview screen..."), the real trap this
        // parser has to defend against. It must never appear as this
        // step's own `description`, nor bleed into a next step (there
        // isn't one here, so leakage would otherwise inflate `actual`
        // or fail to terminate correctly).
        let records = parse(SAMPLE_EXPORT).expect("parses");
        let step2 = &records[1];
        assert!(!step2.description.contains("shows the widgets overview"));
        assert!(!step2.actual.contains("shows the widgets overview"));
        assert_eq!(
            step2.description,
            "Click on the 'Widgets' button to open the widgets module."
        );
    }

    #[test]
    fn parse_handles_a_blank_actual_field() {
        let records = parse(SAMPLE_EXPORT).expect("parses");
        assert_eq!(records[0].actual, "");
    }

    #[test]
    fn parse_ignores_document_preamble_before_the_first_step() {
        // The fixture's leading "Run ID : 1234 - Run_Sample" / baseline
        // metadata block must not become a phantom step 0.
        let records = parse(SAMPLE_EXPORT).expect("parses");
        assert!(records.iter().all(|r| r.step_name.starts_with("Step ")));
    }

    #[test]
    fn parse_returns_no_steps_found_for_a_document_with_no_step_markers() {
        let err = parse("Just some unrelated document text.\nNo steps here.")
            .expect_err("no 'Step Name :' markers at all");
        assert!(matches!(err, HpAlmParseError::NoStepsFound));
    }
}
