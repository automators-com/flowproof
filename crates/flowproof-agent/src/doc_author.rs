//! Draft a `.flow.yaml` spec from a requirement/test-case document (a
//! PDF export from a test-management tool). Same spirit as
//! [`crate::video_author`], but an easier, less contentious problem: the
//! step text (Description/Expected) is extractable straight from the
//! PDF's text layer — no pixel-parsing at all — and the document's own
//! "Expected" field is already an assertion in prose, something a video
//! never has (video shows behaviour with no stated belief about
//! correctness; a test case states one for every step).
//!
//! The translation step still needs a model: a Description is loosely-
//! structured human English (sometimes several actions bundled into one
//! sentence, sometimes naming several alternatives disambiguated only by
//! a later "Actual" note), and turning that into flowproof's exact step
//! grammar — while knowing when to honestly flag rather than guess — is
//! a language-understanding problem, not a lookup table. It's a lighter
//! one than video's, though: plain text in, plain text out, reusing the
//! existing text-only [`ModelClient`], no vision/image plumbing needed.

use std::path::{Path, PathBuf};

use crate::doc_formats::hp_alm::{self, DocStepRecord, HpAlmParseError};
use crate::draft_assembly::{self, DraftLine, STEP_GRAMMAR_RULES};
use crate::llm::ModelClient;
use crate::{AgentError, BackendConfig};

#[derive(Debug, thiserror::Error)]
pub enum DocAuthorError {
    #[error("failed extracting text from '{path}': {reason}")]
    PdfExtraction { path: String, reason: String },
    #[error(transparent)]
    Parse(#[from] HpAlmParseError),
    #[error("no steps were translated from the document's fields — nothing to draft")]
    NoStepsInferred,
    #[error("model produced a draft that does not parse as a flow spec: {0}")]
    DraftInvalid(#[from] crate::spec::SpecError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("io error at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> DocAuthorError {
    DocAuthorError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// Extracts the PDF's text layer. Isolated behind one function so the
/// underlying crate is swappable if a real export's text quality proves
/// inadequate later.
pub fn extract_pdf_text(path: &Path) -> Result<String, DocAuthorError> {
    pdf_extract::extract_text(path).map_err(|e| DocAuthorError::PdfExtraction {
        path: path.display().to_string(),
        reason: e.to_string(),
    })
}

fn translation_system_prompt() -> String {
    format!(
        "You translate ONE test-case step (a Description of the action taken, \
         and an Expected outcome) into flowproof's step grammar. Unlike a video \
         frame pair, you are given an explicit English description of the \
         action - your job is closer to translation than inference.

Describe the action(s) the Description implies, each on its own line \
prefixed `STEP: `, using ONLY these forms. {STEP_GRAMMAR_RULES}

If the Description states no discrete UI action (purely informational, \
or describes something outside the app under test), respond with \
exactly: STEP: NO_ACTION

If the Description implies SOME action but you cannot map it to any of \
the forms above, or it names more than one plausible target with no way \
to tell which was meant, do NOT guess. Respond with exactly one line: \
`UNEXPLAINED: ` followed by a short factual restatement of what the \
Description asks for.

Then, on its own line, translate the Expected outcome into flowproof's \
assert prose, prefixed `ASSERT: ` (e.g. `ASSERT: page shows Widgets \
Overview`) - light copy-editing of the Expected text, not invention. If \
Expected is empty or not a meaningful, checkable outcome, respond with \
exactly: ASSERT: NONE

Respond with ONLY the STEP:/UNEXPLAINED:/ASSERT: line(s) — no prose, no \
quotes, no numbering, no code fences."
    )
}

fn user_prompt(record: &DocStepRecord) -> String {
    format!(
        "Step: {}\nDescription: {}\nExpected: {}\n\
         Actual (UAT run notes, context only - do not turn this into the assert): {}",
        record.step_name,
        record.description,
        record.expected,
        if record.actual.is_empty() {
            "(none)"
        } else {
            &record.actual
        },
    )
}

/// Translates one document step record into draft lines via a
/// text-only model call — no vision/image plumbing needed, since
/// Description/Expected is plain text, not pixels.
pub fn translate_record(
    record: &DocStepRecord,
    client: &mut dyn ModelClient,
) -> Result<Vec<DraftLine>, DocAuthorError> {
    let system = translation_system_prompt();
    let reply = client.complete(&system, &user_prompt(record))?;
    let mut lines = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(step) = line.strip_prefix("STEP:") {
            let step = step.trim();
            if step.is_empty() || step == "NO_ACTION" {
                continue;
            }
            lines.push(DraftLine::Action(step.to_string()));
        } else if let Some(observed) = line.strip_prefix("UNEXPLAINED:") {
            lines.push(DraftLine::Flagged(observed.trim().to_string()));
        } else if let Some(assert) = line.strip_prefix("ASSERT:") {
            let assert = assert.trim();
            if assert.is_empty() || assert == "NONE" {
                continue;
            }
            lines.push(DraftLine::Assert(assert.to_string()));
        }
        // Any other line shape is silently ignored rather than treated
        // as an error: a model occasionally adds a stray line despite
        // instructions, and failing the whole step over one ignorable
        // line would be worse than tolerating it.
    }
    Ok(lines)
}

/// doc-in, draft-spec-out options for [`author_from_doc`].
pub struct DocAuthorOptions {
    pub doc: PathBuf,
    pub app: String,
    pub name: String,
    pub out: PathBuf,
}

pub fn author_from_doc(opts: &DocAuthorOptions) -> Result<PathBuf, DocAuthorError> {
    let config = BackendConfig::from_env().map_err(DocAuthorError::from)?;
    if !config.is_usable() {
        return Err(DocAuthorError::Agent(AgentError::Config(
            "no usable model backend configured (set FLOWPROOF_AI_API_KEY or ANTHROPIC_API_KEY)"
                .into(),
        )));
    }

    let text = extract_pdf_text(&opts.doc)?;
    let records = hp_alm::parse(&text)?;
    let mut client = crate::llm::HttpModelClient::new(config);

    let mut lines = Vec::new();
    for record in &records {
        lines.extend(translate_record(record, &mut client)?);
    }
    if lines.is_empty() {
        return Err(DocAuthorError::NoStepsInferred);
    }

    let header = format!(
        "# DRAFT generated by `flowproof author-from-doc` from {}.\n\
         # Review every step below before `flowproof record` - a flagged\n\
         # step (marked TODO) needs the live app to resolve it, and any\n\
         # assert here is a light translation of the document's own\n\
         # 'Expected' text, not a guarantee it's phrased the way you'd\n\
         # want.",
        opts.doc.display(),
    );
    let yaml = draft_assembly::assemble(&header, &opts.name, &opts.app, &lines)?;
    std::fs::write(&opts.out, &yaml).map_err(|e| io_err(&opts.out, e))?;
    Ok(opts.out.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ScriptedModel {
        replies: std::collections::VecDeque<&'static str>,
    }

    impl ModelClient for ScriptedModel {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            Ok(self
                .replies
                .pop_front()
                .expect("test provided enough replies")
                .to_string())
        }
        fn identity(&self) -> (String, String) {
            ("test".to_string(), "test".to_string())
        }
    }

    fn record(description: &str, expected: &str) -> DocStepRecord {
        DocStepRecord {
            step_name: "Step 1".to_string(),
            description: description.to_string(),
            expected: expected.to_string(),
            actual: String::new(),
        }
    }

    #[test]
    fn translate_record_drops_steps_with_no_actionable_description() {
        let mut client = ScriptedModel {
            replies: ["STEP: NO_ACTION\nASSERT: page shows the login screen"]
                .into_iter()
                .collect(),
        };
        let lines = translate_record(&record("A purely informational note.", "N/A"), &mut client)
            .expect("translates");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], DraftLine::Assert(a) if a == "page shows the login screen"));
    }

    /// The point of the whole flagging design carried over from video:
    /// a Description that doesn't map to the grammar becomes a flagged
    /// freeform step for the live agent to resolve, never a forced
    /// guess and never silently dropped.
    #[test]
    fn translate_record_flags_unparseable_description_instead_of_guessing() {
        let mut client = ScriptedModel {
            replies: ["UNEXPLAINED: extract a report from search results\nASSERT: NONE"]
                .into_iter()
                .collect(),
        };
        let lines = translate_record(
            &record("Extract the complete record from the search results.", ""),
            &mut client,
        )
        .expect("translates");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], DraftLine::Flagged(_)));
    }

    #[test]
    fn translate_record_converts_expected_into_an_assert_line() {
        let mut client = ScriptedModel {
            replies: ["STEP: Press the \"Go\" button\nASSERT: page shows search results"]
                .into_iter()
                .collect(),
        };
        let lines = translate_record(
            &record(
                "Click on the 'Go' button.",
                "The search results are displayed.",
            ),
            &mut client,
        )
        .expect("translates");
        assert_eq!(lines.len(), 2);
        assert!(matches!(&lines[0], DraftLine::Action(a) if a == "Press the \"Go\" button"));
        assert!(matches!(&lines[1], DraftLine::Assert(a) if a == "page shows search results"));
    }

    #[test]
    fn translate_record_treats_assert_none_as_no_assertion() {
        let mut client = ScriptedModel {
            replies: ["STEP: Press Enter\nASSERT: NONE"].into_iter().collect(),
        };
        let lines = translate_record(&record("Press enter to submit.", ""), &mut client)
            .expect("translates");
        assert_eq!(lines.len(), 1);
        assert!(matches!(&lines[0], DraftLine::Action(a) if a == "Press Enter"));
    }

    /// Unlike video's invariant (never emits an assert), a doc-authored
    /// draft both CAN and SHOULD contain one, since the document already
    /// states an expected outcome.
    #[test]
    fn a_doc_authored_draft_can_contain_a_real_assert_unlike_video() {
        let lines = vec![
            DraftLine::Action("Go to /nVA01".to_string()),
            DraftLine::Assert("page shows Create Standard Order".to_string()),
        ];
        let yaml = draft_assembly::assemble("# DRAFT", "n", "sap", &lines).expect("assembles");
        let spec = crate::FlowSpec::parse(&yaml).expect("draft parses");
        assert!(spec
            .steps
            .iter()
            .any(|s| matches!(s, crate::spec::SpecStep::Assert { .. })));
    }
}
