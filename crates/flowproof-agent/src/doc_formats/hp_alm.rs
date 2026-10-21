//! Parses the text layer of an HP ALM / Quality Center test-run PDF
//! export into test cases and their steps. Scoped narrowly to ALM's run
//! report (`Run ID :` / `Step Name :` / `Description:` / `Expected` /
//! `Actual` / `Run Step Attachments` blocks), whether it holds one run or
//! a whole test set's, and whether or not the export numbered its
//! headings (`1.1.1.1 Step Name : Step 1`, as it does once the report
//! has been through Word) — a different export tool (qTest, Zephyr,
//! Azure DevOps, a plain Word doc) gets its own sibling module in
//! `doc_formats/`, not a rewrite of this one.

use std::sync::LazyLock;

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

/// One test case's steps, from the latest run of it in the document.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DocTestCase {
    /// The run's `Test: Test Case Name:` field. `None` when the document
    /// has no run header to read it from.
    pub name: Option<String>,
    pub run_id: Option<String>,
    pub steps: Vec<DocStepRecord>,
}

#[derive(Debug, thiserror::Error)]
pub enum HpAlmParseError {
    #[error("no 'Step Name :' blocks found in the document — nothing to draft")]
    NoStepsFound,
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex is well-formed")
}

/// A heading's optional outline number, as in `1.1.1.1 Step Name :`.
const NUMBER: &str = r"(?:\d+(?:\.\d+)*[ \t]+)?";

static ATTACHMENT: LazyLock<Regex> = LazyLock::new(|| re(r"\[Attachment:[^\]]*\]"));
static PAGE_FURNITURE: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?m)^[ \t]*(?:Application Lifecycle Management|\d+[ \t]*\|[ \t]*Page)[ \t]*$")
});
static RUN_START: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?m)^{NUMBER}Run ID[ \t]+:[ \t]*(\S+)")));
static TEST_CASE_NAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?s)Test:\s*Test Case\s+Name:(.*?)(?:\n[ \t]*\n|OS Build Number:|\z)"));
static STEP_START: LazyLock<Regex> =
    LazyLock::new(|| re(&format!(r"(?m)^{NUMBER}Step Name[ \t]+:[ \t]*(.+?)[ \t]*$")));
// Each label sits on its own line in the real template; `Description:`
// carries a literal colon, `Expected`/`Actual` do not. Anchoring to the
// line keeps prose such as "Target/Actual - comparison" whole.
static DESCRIPTION: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?ms)^[ \t]*Description:(.*?)^[ \t]*Expected[ \t]*$"));
static EXPECTED: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?ms)^[ \t]*Expected[ \t]*$(.*?)^[ \t]*Actual[ \t]*$"));
// Stops at "Run Step Attachments" OR end of this step's own block slice
// (`\z` here means end of `block`, not end of the document) - a step
// with no attachment still terminates cleanly.
static ACTUAL: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"(?ms)^[ \t]*Actual[ \t]*$(.*?)(?:^[ \t]*{NUMBER}Run Step Attachments\b|\z)"
    ))
});

/// Collapses arbitrary PDF-extraction whitespace (wrapped lines, runs of
/// spaces, blank lines) into single-spaced prose, dropping the inline
/// `[Attachment: name.png]` markers ALM leaves where a picture was.
fn normalize(s: &str) -> String {
    let s = ATTACHMENT.replace_all(s, " ");
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Removes the running header and page number ALM prints on every page.
/// They land wherever the page broke, often mid-Description.
fn without_page_furniture(text: &str) -> String {
    PAGE_FURNITURE.replace_all(text, "").into_owned()
}

/// Segments the document's extracted text into test cases, one per run.
///
/// Block boundaries are headings at the start of a line, optionally
/// numbered: `Run ID : 4066 - …` and `Step Name : Step 1`. The same
/// labels reappear in each block's own metadata table ("Change Status:
/// Run ID: 4066", "Exec Time: … Step Name: Step 1"), but never as the
/// first token on a line and never with a space before the colon, so
/// the anchors never double-match within one block.
///
/// A test case run more than once keeps only its highest Run ID, the
/// latest run of it. Steps are put in step-number order, since a step
/// added to a test case later can be printed after the others.
pub fn parse(text: &str) -> Result<Vec<DocTestCase>, HpAlmParseError> {
    let text = without_page_furniture(text);
    let mut bounds: Vec<(usize, Option<String>)> = vec![(0, None)];
    bounds.extend(
        RUN_START
            .captures_iter(&text)
            .map(|c| (c.get(0).expect("group 0").start(), Some(c[1].to_string()))),
    );

    let mut cases: Vec<DocTestCase> = Vec::new();
    for (i, (start, run_id)) in bounds.iter().enumerate() {
        let end = bounds.get(i + 1).map_or(text.len(), |b| b.0);
        let run = &text[*start..end];
        let steps = parse_steps(run);
        if steps.is_empty() {
            continue;
        }
        let case = DocTestCase {
            name: test_case_name(run),
            run_id: run_id.clone(),
            steps,
        };
        let earlier = cases
            .iter_mut()
            .find(|c| c.name.is_some() && c.name == case.name);
        match earlier {
            Some(earlier) if run_number(&case) > run_number(earlier) => *earlier = case,
            Some(_) => {}
            None => cases.push(case),
        }
    }
    if cases.is_empty() {
        return Err(HpAlmParseError::NoStepsFound);
    }
    Ok(cases)
}

fn run_number(case: &DocTestCase) -> u64 {
    case.run_id
        .as_deref()
        .and_then(|id| id.parse().ok())
        .unwrap_or(0)
}

/// The test case's name, wrapped over as many lines as the report's
/// column needed, up to the next field or a blank line.
fn test_case_name(run: &str) -> Option<String> {
    let preamble = run.split("Step Name").next().unwrap_or(run);
    TEST_CASE_NAME
        .captures(preamble)
        .map(|c| normalize(&c[1]))
        .filter(|n| !n.is_empty())
}

fn parse_steps(run: &str) -> Vec<DocStepRecord> {
    let starts: Vec<_> = STEP_START.captures_iter(run).collect();
    let mut records = Vec::with_capacity(starts.len());
    for (i, cap) in starts.iter().enumerate() {
        let block_start = cap.get(0).expect("group 0 always matches").start();
        let block_end = starts
            .get(i + 1)
            .map(|next| next.get(0).expect("group 0 always matches").start())
            .unwrap_or(run.len());
        let block = &run[block_start..block_end];
        let field = |r: &Regex| {
            r.captures(block)
                .map(|c| normalize(&c[1]))
                .unwrap_or_default()
        };

        records.push(DocStepRecord {
            step_name: normalize(&cap[1]),
            description: field(&DESCRIPTION),
            expected: field(&EXPECTED),
            actual: field(&ACTUAL),
        });
    }

    let numbers: Option<Vec<u32>> = records
        .iter()
        .map(|r| r.step_name.strip_prefix("Step ")?.parse().ok())
        .collect();
    if let Some(numbers) = numbers {
        let mut numbered: Vec<_> = numbers.into_iter().zip(records).collect();
        numbered.sort_by_key(|(n, _)| *n);
        records = numbered.into_iter().map(|(_, r)| r).collect();
    }
    records
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

    /// The steps of a document that holds exactly one test case.
    fn steps(text: &str) -> Vec<DocStepRecord> {
        let mut cases = parse(text).expect("parses");
        assert_eq!(cases.len(), 1, "one test case");
        cases.remove(0).steps
    }

    #[test]
    fn parse_segments_a_realistic_export_into_step_records() {
        let records = steps(SAMPLE_EXPORT);
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
        let records = steps(SAMPLE_EXPORT);
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
        let records = steps(SAMPLE_EXPORT);
        assert_eq!(records[0].actual, "");
    }

    #[test]
    fn parse_ignores_document_preamble_before_the_first_step() {
        // The fixture's leading "Run ID : 1234 - Run_Sample" / baseline
        // metadata block must not become a phantom step 0.
        let records = steps(SAMPLE_EXPORT);
        assert!(records.iter().all(|r| r.step_name.starts_with("Step ")));
    }

    #[test]
    fn parse_returns_no_steps_found_for_a_document_with_no_step_markers() {
        let err = parse("Just some unrelated document text.\nNo steps here.")
            .expect_err("no 'Step Name :' markers at all");
        assert!(matches!(err, HpAlmParseError::NoStepsFound));
    }

    /// A test-set report as `pdf-extract` reads one that went through
    /// Word: numbered headings, a page header mid-Description, a step
    /// printed out of order, and an older run of the same test case.
    /// Synthetic content in the real layout.
    const TEST_SET_REPORT: &str = "\
1.1  Run ID : 1990  - Run_4-30_9-0-0 
Change Status:    Run ID:  1990 
 Test: Test Case 
Name:  T-001 - Create 
sample order 

1.1.1.1 Step Name : Step 1 
  Description: 
An older wording of the step 
  Expected 
Done 
  Actual 

1.2  Run ID : 2050  - Run_5-2_10-0-0 
Change Status:    Run ID:  2050 
Name:  T-001 - Create  Test: Test Case Name:  T-001 - Create 
sample order 
OS Build Number:    Tester:  tester(Sample, Tester) 

1.2.1.1 Step Name : Step 1 
Exec Date:  5/2/25  Test Step Status:  Passed 
Exec Time:  10:00:00  Step Name:  Step 1 
  Description: 
Open the sample app and go to the 

      Application Lifecycle Management 

 2 |Page    

orders screen 
  Expected 
Target/Actual - comparison displayed 
  Actual 

1.2.1.2 Step Name : Step 3 
  Description: 
[Attachment: snapshot.png]Press \"Save\" 
  Expected 
The order is saved 
  Actual 
saved 

1.2.1.2.1  Run Step Attachments 
1.2.1.2.1.1 File Name: snapshot.png 
Description:  shows the saved order 

1.2.1.3 Step Name : Step 2 
  Description: 
Enter the order details 
  Expected 
The details are accepted 
  Actual 
ok 

1.10 Run ID : 2010  - Run_5-1_8-0-0 
Change Status:    Run ID:  2010 
 Test: Test Case Name:  T-002 - Display sample order 
OS Build Number:    Tester:  tester(Sample, Tester) 

1.10.1.1  Step Name : Step 1 
  Description: 
Open the order display 
  Expected 
The order is shown 
  Actual 
";

    #[test]
    fn a_test_set_report_is_one_test_case_per_run_named_from_the_report() {
        let cases = parse(TEST_SET_REPORT).expect("parses");
        let names: Vec<_> = cases.iter().map(|c| c.name.as_deref()).collect();
        assert_eq!(
            names,
            [
                Some("T-001 - Create sample order"),
                Some("T-002 - Display sample order")
            ]
        );
        assert_eq!(cases[1].run_id.as_deref(), Some("2010"));
    }

    #[test]
    fn a_test_case_run_twice_keeps_only_its_latest_run() {
        let cases = parse(TEST_SET_REPORT).expect("parses");
        assert_eq!(cases[0].run_id.as_deref(), Some("2050"));
        assert_eq!(cases[0].steps.len(), 3);
        assert!(cases[0]
            .steps
            .iter()
            .all(|s| !s.description.contains("older")));
    }

    #[test]
    fn numbered_headings_and_page_breaks_do_not_split_or_pollute_a_step() {
        let first = &parse(TEST_SET_REPORT).expect("parses")[0].steps[0];
        assert_eq!(
            first.description,
            "Open the sample app and go to the orders screen"
        );
        assert_eq!(first.expected, "Target/Actual - comparison displayed");
        assert_eq!(first.actual, "");
    }

    #[test]
    fn steps_come_out_in_step_number_order() {
        let steps = &parse(TEST_SET_REPORT).expect("parses")[0].steps;
        let names: Vec<_> = steps.iter().map(|s| s.step_name.as_str()).collect();
        assert_eq!(names, ["Step 1", "Step 2", "Step 3"]);
    }

    #[test]
    fn a_runs_last_step_stops_at_the_next_run_and_attachments_stay_out() {
        let steps = &parse(TEST_SET_REPORT).expect("parses")[0].steps;
        assert_eq!(steps[1].actual, "ok", "printed last, before run 2010");
        assert_eq!(steps[2].description, "Press \"Save\"");
        assert_eq!(steps[2].actual, "saved");
    }

    #[test]
    fn steps_without_any_run_header_are_one_unnamed_test_case() {
        let text = "Step Name : Step 1\n  Description:\nOpen it\n  Expected\nOpen\n  Actual\n";
        let cases = parse(text).expect("parses");
        assert_eq!(cases.len(), 1);
        assert_eq!(
            (cases[0].name.as_deref(), cases[0].run_id.as_deref()),
            (None, None)
        );
        assert_eq!(cases[0].steps[0].description, "Open it");
    }
}
