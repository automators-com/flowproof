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

use crate::doc_formats::hp_alm::{self, DocStepRecord, DocTestCase, HpAlmParseError};
use crate::draft_assembly::{self, DraftLine, STEP_GRAMMAR_RULES};
use crate::llm::ModelClient;
use crate::{AgentError, BackendConfig};

#[derive(Debug, thiserror::Error)]
pub enum DocAuthorError {
    #[error("failed extracting text from '{path}': {reason}")]
    PdfExtraction { path: String, reason: String },
    #[error(transparent)]
    Parse(#[from] HpAlmParseError),
    #[error(
        "the document holds {0} test cases; choose one with --test-case \
         (its number from 1 to {0} or its name, as --check lists them)"
    )]
    TestCaseRequired(usize),
    #[error("the document has no test case `{0}`; --check lists them")]
    UnknownTestCase(String),
    #[error("'{0}' already exists; draft into an empty folder or move it first")]
    WouldOverwrite(PathBuf),
    #[error("no steps were translated from the document's fields — nothing to draft")]
    NoStepsInferred,
    #[error("model produced a draft that does not parse as a flow spec: {0}")]
    DraftInvalid(#[from] crate::spec::SpecError),
    #[error("model produced invalid business-data value `{0}`")]
    InvalidValue(String),
    #[error("model produced conflicting values for `{name}`: `{first}` and `{second}`")]
    ValueConflict {
        name: String,
        first: String,
        second: String,
    },
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

A Description often holds several actions (`select the plant, then click \
Save`). Write one line per action, in the order they happen, and never fold \
two actions into one line. Judge each action on its own: one you can map is \
a STEP even when another action in the same Description is not, and only \
the action you cannot map gets the OUT_OF_SCOPE or UNEXPLAINED line below. \
Splitting never licenses inventing: use only a path, transaction code, label \
or value the Description gives, and an action that needs one it does not give \
is UNEXPLAINED.

If the Description states no discrete action at all (purely informational, \
e.g. a note or a pass/fail observation with nothing to do), respond with \
exactly: STEP: NO_ACTION

If the Description describes a real action but that action targets a \
different app or tool than the one under test (e.g. opening a downloaded \
file in a spreadsheet program, sending an email, checking a filesystem) — \
not ambiguity about which UI element to use, but the action clearly \
happening somewhere else entirely — do NOT drop it and do NOT force it \
into the forms above. Write one line for that action: `OUT_OF_SCOPE: ` \
followed by a short factual restatement of what it asks for.

If an action is within the app under test but you cannot map it to any \
of the forms above, or it names more than one plausible target with no way \
to tell which was meant, do NOT guess. Write one line for that action: \
`UNEXPLAINED: ` followed by a short factual restatement of what it asks \
for, then ` — ` and what it leaves out that a step needs (e.g. `open the dashboard — no URL or menu \
path given`, `pick a location — does not say which one`).

Then, on its own line, translate the Expected outcome into flowproof's \
assert prose, prefixed `ASSERT: ` (e.g. `ASSERT: page shows Widgets \
Overview`) - light copy-editing of the Expected text, not invention. If \
Expected is empty or not a meaningful, checkable outcome, respond with \
exactly: ASSERT: NONE

If the document gives concrete non-secret business data that should be \
reusable across runs (material number, supplier, plant, customer, order id, \
dates, quantities, document numbers), put a placeholder in the STEP:/ASSERT: \
text using `${{UPPER_SNAKE_CASE}}` and add one line per value as \
`VALUE: UPPER_SNAKE_CASE=the literal value`. Do not emit VALUE lines for \
passwords, tokens, API keys, or login credentials; those belong in \
`flowproof config` or the caller's secret environment.

Respond with ONLY the STEP:/UNEXPLAINED:/ASSERT:/VALUE: line(s) — no prose, \
no quotes, no numbering, no code fences."
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
    Ok(translate_record_with_values(record, client)?.lines)
}

fn translate_record_with_values(
    record: &DocStepRecord,
    client: &mut dyn ModelClient,
) -> Result<DocTranslation, DocAuthorError> {
    let system = translation_system_prompt();
    let reply = client.complete(&system, &user_prompt(record))?;
    let mut translated = DocTranslation::default();
    // The Expected outcome follows the step's last action, wherever the
    // reply put its ASSERT line.
    let mut asserts = Vec::new();
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
            translated.lines.push(DraftLine::Action(step.to_string()));
        } else if let Some(observed) = line.strip_prefix("UNEXPLAINED:") {
            translated
                .lines
                .push(DraftLine::Flagged(observed.trim().to_string()));
        } else if let Some(observed) = line.strip_prefix("OUT_OF_SCOPE:") {
            translated
                .lines
                .push(DraftLine::OutOfScope(observed.trim().to_string()));
        } else if let Some(assert) = line.strip_prefix("ASSERT:") {
            let assert = assert.trim();
            if assert.is_empty() || assert == "NONE" {
                continue;
            }
            asserts.push(DraftLine::Assert(assert.to_string()));
        } else if let Some(value) = line.strip_prefix("VALUE:") {
            translated.values.push(parse_value_line(value.trim())?);
        }
        // Any other line shape is silently ignored rather than treated
        // as an error: a model occasionally adds a stray line despite
        // instructions, and failing the whole step over one ignorable
        // line would be worse than tolerating it.
    }
    translated.lines.extend(asserts);
    Ok(translated)
}

/// doc-in, draft-spec-out options for [`author_from_doc`].
pub struct DocAuthorOptions {
    pub doc: PathBuf,
    pub app: String,
    pub name: String,
    pub out: PathBuf,
    /// Which test case to draft when the document holds several: its
    /// 1-based number in document order, its name, or its ID (the part
    /// of the name before ` - `).
    pub test_case: Option<String>,
}

pub struct DocAuthorResult {
    pub flow: PathBuf,
    /// The drafted test case's name, when the document gives one.
    pub test_case: Option<String>,
    pub values: Option<PathBuf>,
    /// The drafted lines in file order, so a caller can show which steps
    /// were flagged without reading the `# TODO` comments back.
    pub lines: Vec<DraftLine>,
}

/// What one test case in a document holds before anything is sent to a
/// model: enough for a caller to confirm the format is readable, say how
/// much it found and offer the test cases to choose from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocInspection {
    pub name: Option<String>,
    pub run_id: Option<String>,
    pub steps: usize,
    /// Steps whose `Expected` field is filled in, each a candidate assert.
    pub with_expected: usize,
}

/// Reads and segments the document without a model call, one entry per
/// test case in document order.
pub fn inspect_doc(doc: &Path) -> Result<Vec<DocInspection>, DocAuthorError> {
    let cases = hp_alm::parse(&extract_pdf_text(doc)?)?;
    Ok(cases
        .into_iter()
        .map(|case| DocInspection {
            with_expected: case
                .steps
                .iter()
                .filter(|r| !r.expected.trim().is_empty())
                .count(),
            steps: case.steps.len(),
            name: case.name,
            run_id: case.run_id,
        })
        .collect())
}

/// The test case `choice` names (see [`DocAuthorOptions::test_case`]).
/// A document with one test case needs no choice.
fn select_test_case(
    mut cases: Vec<DocTestCase>,
    choice: Option<&str>,
) -> Result<DocTestCase, DocAuthorError> {
    let Some(choice) = choice else {
        if cases.len() == 1 {
            return Ok(cases.remove(0));
        }
        return Err(DocAuthorError::TestCaseRequired(cases.len()));
    };
    let at = position_of(&cases, choice)?;
    Ok(cases.swap_remove(at))
}

/// The test cases `choices` name, each as [`select_test_case`] takes it,
/// in document order and each once. `all` (in any case) names every one.
fn select_test_cases(
    cases: Vec<DocTestCase>,
    choices: &[String],
) -> Result<Vec<DocTestCase>, DocAuthorError> {
    if choices.iter().any(|c| c.trim().eq_ignore_ascii_case("all")) {
        return Ok(cases);
    }
    let mut picked = std::collections::BTreeSet::new();
    for choice in choices {
        picked.insert(position_of(&cases, choice)?);
    }
    Ok(cases
        .into_iter()
        .enumerate()
        .filter(|(i, _)| picked.contains(i))
        .map(|(_, case)| case)
        .collect())
}

fn position_of(cases: &[DocTestCase], choice: &str) -> Result<usize, DocAuthorError> {
    let wanted = choice.split_whitespace().collect::<Vec<_>>().join(" ");
    let at = match wanted.parse::<usize>() {
        Ok(n) => n.checked_sub(1).filter(|&i| i < cases.len()),
        Err(_) => cases.iter().position(|c| {
            c.name.as_deref().is_some_and(|name| {
                let id = name.split(" - ").next().unwrap_or(name);
                name.eq_ignore_ascii_case(&wanted) || id.eq_ignore_ascii_case(&wanted)
            })
        }),
    };
    at.ok_or_else(|| DocAuthorError::UnknownTestCase(choice.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BusinessValue {
    pub name: String,
    pub value: String,
}

#[derive(Default)]
struct DocTranslation {
    lines: Vec<DraftLine>,
    values: Vec<BusinessValue>,
}

fn default_values_path(spec: &Path) -> PathBuf {
    let stem = spec
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = stem.strip_suffix(".flow.yaml").unwrap_or_else(|| {
        stem.strip_suffix(".yaml")
            .or_else(|| stem.strip_suffix(".yml"))
            .unwrap_or(&stem)
    });
    spec.with_file_name(format!("{base}.values.yaml"))
}

fn valid_value_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn parse_value_line(line: &str) -> Result<BusinessValue, DocAuthorError> {
    let Some((name, value)) = line.split_once('=') else {
        return Err(DocAuthorError::InvalidValue(line.to_string()));
    };
    let name = name.trim();
    if !valid_value_name(name) {
        return Err(DocAuthorError::InvalidValue(line.to_string()));
    }
    Ok(BusinessValue {
        name: name.to_string(),
        value: value.trim().to_string(),
    })
}

fn values_yaml(values: &[BusinessValue]) -> Result<String, DocAuthorError> {
    let mut map: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for value in values {
        match map.get(&value.name) {
            Some(existing) if existing != &value.value => {
                return Err(DocAuthorError::ValueConflict {
                    name: value.name.clone(),
                    first: existing.clone(),
                    second: value.value.clone(),
                });
            }
            Some(_) => {}
            None => {
                map.insert(value.name.clone(), value.value.clone());
            }
        }
    }
    serde_yaml::to_string(&map).map_err(|e| DocAuthorError::InvalidValue(e.to_string()))
}

pub fn author_from_doc(opts: &DocAuthorOptions) -> Result<DocAuthorResult, DocAuthorError> {
    author_from_doc_with_progress(opts, &mut |_, _| {})
}

/// [`author_from_doc`], calling `on_step(done, total)` before each
/// document step is sent to the model: the one slow part.
pub fn author_from_doc_with_progress(
    opts: &DocAuthorOptions,
    on_step: &mut dyn FnMut(usize, usize),
) -> Result<DocAuthorResult, DocAuthorError> {
    let mut client = model_client()?;
    let text = extract_pdf_text(&opts.doc)?;
    let case = select_test_case(hp_alm::parse(&text)?, opts.test_case.as_deref())?;
    draft_case(
        case,
        &opts.doc,
        &opts.app,
        &opts.name,
        &opts.out,
        &mut client,
        on_step,
    )
}

/// Several test cases of one document, each drafted into its own flow in
/// `out_dir`, for [`author_many_from_doc_with_progress`].
pub struct DocBatchOptions {
    pub doc: PathBuf,
    pub app: String,
    pub out_dir: PathBuf,
    /// The test cases to draft, each as [`DocAuthorOptions::test_case`]
    /// takes it, or `all`.
    pub test_cases: Vec<String>,
}

/// One test case of a batch: its draft, or why it has none. A failure
/// stops only that test case.
pub struct DocBatchEntry {
    pub test_case: Option<String>,
    pub outcome: Result<DocAuthorResult, DocAuthorError>,
}

/// Drafts each chosen test case into `<out_dir>/<id or name>.flow.yaml`,
/// named after its test case, calling `on_step(case, cases, done, total)`
/// before each document step is sent to the model. Nothing is drafted if
/// a file it would write already exists.
pub fn author_many_from_doc_with_progress(
    opts: &DocBatchOptions,
    on_step: &mut dyn FnMut(usize, usize, usize, usize),
) -> Result<Vec<DocBatchEntry>, DocAuthorError> {
    let mut client = model_client()?;
    let text = extract_pdf_text(&opts.doc)?;
    let cases = select_test_cases(hp_alm::parse(&text)?, &opts.test_cases)?;
    draft_cases(cases, opts, &mut client, on_step)
}

fn draft_cases(
    cases: Vec<DocTestCase>,
    opts: &DocBatchOptions,
    client: &mut dyn ModelClient,
    on_step: &mut dyn FnMut(usize, usize, usize, usize),
) -> Result<Vec<DocBatchEntry>, DocAuthorError> {
    let mut taken = std::collections::HashSet::new();
    let outs: Vec<PathBuf> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| {
            opts.out_dir
                .join(flow_file_name(case.name.as_deref(), i + 1, &mut taken))
        })
        .collect();
    for out in &outs {
        if let Some(existing) = [out.clone(), default_values_path(out)]
            .into_iter()
            .find(|p| p.exists())
        {
            return Err(DocAuthorError::WouldOverwrite(existing));
        }
    }
    std::fs::create_dir_all(&opts.out_dir).map_err(|e| io_err(&opts.out_dir, e))?;
    // Two test cases that differ only by ID keep their IDs, so the
    // flows can be told apart.
    let short: Vec<String> = cases
        .iter()
        .enumerate()
        .map(|(i, case)| flow_name(case.name.as_deref(), i + 1))
        .collect();
    let total = cases.len();
    let mut entries = Vec::new();
    for (i, (case, out)) in cases.into_iter().zip(outs).enumerate() {
        let test_case = case.name.clone();
        let name = match &case.name {
            Some(full) if short.iter().filter(|n| **n == short[i]).count() > 1 => full.clone(),
            _ => short[i].clone(),
        };
        let outcome = draft_case(
            case,
            &opts.doc,
            &opts.app,
            &name,
            &out,
            client,
            &mut |done, steps| on_step(i, total, done, steps),
        );
        entries.push(DocBatchEntry { test_case, outcome });
    }
    Ok(entries)
}

/// A test case's name without the ID before ` - `, as a flow name.
fn flow_name(test_case: Option<&str>, n: usize) -> String {
    let name = test_case.map(|name| name.split_once(" - ").map_or(name, |(_, rest)| rest).trim());
    match name {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => format!("Test case {n}"),
    }
}

/// `<test case name>.flow.yaml` with only letters, digits and dashes, so
/// it is safe on any file system, numbered when two test cases share one.
fn flow_file_name(
    test_case: Option<&str>,
    n: usize,
    taken: &mut std::collections::HashSet<String>,
) -> String {
    let slug: String = test_case
        .unwrap_or_default()
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .chars()
        .take(80)
        .collect();
    let base = match slug.trim_end_matches('-') {
        "" => format!("test-case-{n}"),
        slug => slug.to_string(),
    };
    let mut stem = base.clone();
    let mut k = 2;
    while !taken.insert(stem.clone()) {
        stem = format!("{base}-{k}");
        k += 1;
    }
    format!("{stem}.flow.yaml")
}

fn model_client() -> Result<crate::llm::HttpModelClient, DocAuthorError> {
    let config = BackendConfig::from_env().map_err(DocAuthorError::from)?;
    if !config.is_usable() {
        return Err(DocAuthorError::Agent(AgentError::Config(
            "no usable model backend configured (run `flowproof config ai` or set FLOWPROOF_AI_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY)"
                .into(),
        )));
    }
    Ok(crate::llm::HttpModelClient::new(config))
}

fn draft_case(
    case: DocTestCase,
    doc: &Path,
    app: &str,
    name: &str,
    out: &Path,
    client: &mut dyn ModelClient,
    on_step: &mut dyn FnMut(usize, usize),
) -> Result<DocAuthorResult, DocAuthorError> {
    let records = case.steps;
    let mut lines = Vec::new();
    let mut values = Vec::new();
    for (i, record) in records.iter().enumerate() {
        on_step(i, records.len());
        let translated = translate_record_with_values(record, client)?;
        lines.extend(translated.lines);
        values.extend(translated.values);
    }
    if lines.is_empty() {
        return Err(DocAuthorError::NoStepsInferred);
    }

    let source = match &case.name {
        Some(name) => format!("{} (test case {name})", doc.display()),
        None => doc.display().to_string(),
    };
    let header = format!(
        "# DRAFT generated by `flowproof author-from-doc` from {source}.\n\
         # Review every step below before `flowproof record` - a flagged\n\
         # step (marked TODO) needs the live app to resolve it, and any\n\
         # assert here is a light translation of the document's own\n\
         # 'Expected' text, not a guarantee it's phrased the way you'd\n\
         # want.",
    );
    let yaml = draft_assembly::assemble(&header, name, app, &lines)?;
    std::fs::write(out, &yaml).map_err(|e| io_err(out, e))?;
    let values = if values.is_empty() {
        None
    } else {
        let path = default_values_path(out);
        std::fs::write(&path, values_yaml(&values)?).map_err(|e| io_err(&path, e))?;
        Some(path)
    };
    Ok(DocAuthorResult {
        flow: out.to_path_buf(),
        test_case: case.name,
        values,
        lines,
    })
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

    fn cases(names: &[&str]) -> Vec<DocTestCase> {
        names
            .iter()
            .map(|n| DocTestCase {
                name: Some(n.to_string()),
                ..Default::default()
            })
            .collect()
    }

    fn chosen(names: &[&str], choice: Option<&str>) -> Result<Option<String>, DocAuthorError> {
        select_test_case(cases(names), choice).map(|c| c.name)
    }

    fn batch_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fp-doc-batch-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn batch(dir: &Path) -> DocBatchOptions {
        DocBatchOptions {
            doc: PathBuf::from("report.pdf"),
            app: "web".into(),
            out_dir: dir.to_path_buf(),
            test_cases: vec!["all".into()],
        }
    }

    fn with_one_step(names: &[&str]) -> Vec<DocTestCase> {
        let mut cases = cases(names);
        for case in &mut cases {
            case.steps = vec![record("Click on the 'Save' button.", "")];
        }
        cases
    }

    #[test]
    fn several_test_cases_are_chosen_once_each_in_document_order() {
        let names = ["T-1 - Create", "T-2 - Change", "T-3 - Display"];
        let picked = |choices: &[&str]| {
            let choices: Vec<String> = choices.iter().map(|c| c.to_string()).collect();
            select_test_cases(cases(&names), &choices).map(|cs| {
                cs.into_iter()
                    .map(|c| c.name.expect("as set up"))
                    .collect::<Vec<_>>()
            })
        };
        assert_eq!(
            picked(&["3", "t-1", "3"]).expect("as set up"),
            ["T-1 - Create", "T-3 - Display"]
        );
        assert_eq!(picked(&["2", "ALL"]).expect("as set up").len(), 3);
        assert!(
            matches!(picked(&["1", "T-9"]), Err(DocAuthorError::UnknownTestCase(c)) if c == "T-9")
        );
    }

    /// Each test case is its own flow, named after the test case; a model
    /// failure on one leaves the others drafted.
    #[test]
    fn each_test_case_is_its_own_flow_and_one_failing_stops_only_itself() {
        let dir = batch_dir("each");
        let mut client = ScriptedModel {
            replies: [
                "STEP: Press the \"Save\" button\nASSERT: NONE",
                "STEP: NO_ACTION\nASSERT: NONE",
                "STEP: Type ${PLANT} into the \"Plant\" field\nVALUE: PLANT=1000\nASSERT: NONE",
            ]
            .into_iter()
            .collect(),
        };
        let mut seen = Vec::new();
        let entries = draft_cases(
            with_one_step(&["T-1 - Create order", "T-2 - Note only", "Kanban (crates)"]),
            &batch(&dir),
            &mut client,
            &mut |case, cases, done, steps| seen.push((case, cases, done, steps)),
        )
        .expect("drafts");
        assert_eq!(seen, [(0, 3, 0, 1), (1, 3, 0, 1), (2, 3, 0, 1)]);
        assert_eq!(entries.len(), 3);
        let first = entries[0].outcome.as_ref().expect("first drafted");
        assert_eq!(first.flow, dir.join("t-1-create-order.flow.yaml"));
        let yaml = std::fs::read_to_string(&first.flow).expect("as set up");
        assert!(yaml.contains("name: \"Create order\""), "{yaml}");
        assert!(yaml.contains("(test case T-1 - Create order)"), "{yaml}");
        assert!(matches!(
            entries[1].outcome,
            Err(DocAuthorError::NoStepsInferred)
        ));
        assert_eq!(entries[1].test_case.as_deref(), Some("T-2 - Note only"));
        assert!(!dir.join("t-2-note-only.flow.yaml").exists());
        let third = entries[2].outcome.as_ref().expect("third drafted");
        assert_eq!(third.flow, dir.join("kanban-crates.flow.yaml"));
        assert_eq!(third.values, Some(dir.join("kanban-crates.values.yaml")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_cases_sharing_a_name_or_without_one_get_their_own_files() {
        let dir = batch_dir("names");
        let mut cases = with_one_step(&[
            "Order - Create",
            "order  create",
            "",
            "%%%",
            "T-1 - Pay",
            "T-2 - Pay",
        ]);
        cases[2].name = None;
        let mut client = ScriptedModel {
            replies: std::iter::repeat_n("STEP: Press the \"Save\" button\nASSERT: NONE", 6)
                .collect(),
        };
        let entries =
            draft_cases(cases, &batch(&dir), &mut client, &mut |_, _, _, _| {}).expect("drafts");
        let files: Vec<_> = entries
            .iter()
            .map(|e| {
                e.outcome
                    .as_ref()
                    .expect("as set up")
                    .flow
                    .file_name()
                    .expect("as set up")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            files[..4],
            [
                "order-create.flow.yaml",
                "order-create-2.flow.yaml",
                "test-case-3.flow.yaml",
                "test-case-4.flow.yaml"
            ]
        );
        let unnamed =
            std::fs::read_to_string(dir.join("test-case-3.flow.yaml")).expect("as set up");
        assert!(unnamed.contains("name: \"Test case 3\""), "{unnamed}");
        let pay = std::fs::read_to_string(dir.join("t-2-pay.flow.yaml")).expect("as set up");
        assert!(
            pay.contains("name: \"T-2 - Pay\""),
            "a clashing name keeps its ID: {pay}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rerun must not replace drafts someone has started editing, and
    /// must say so before spending any model calls.
    #[test]
    fn a_batch_refuses_to_overwrite_before_any_model_call() {
        let dir = batch_dir("overwrite");
        std::fs::create_dir_all(&dir).expect("as set up");
        std::fs::write(dir.join("t-2-change.values.yaml"), "KEEP: me\n").expect("as set up");
        let mut client = ScriptedModel {
            replies: Default::default(),
        };
        let err = draft_cases(
            with_one_step(&["T-1 - Create", "T-2 - Change"]),
            &batch(&dir),
            &mut client,
            &mut |_, _, _, _| {},
        )
        .err()
        .expect("refused");
        assert!(
            matches!(&err, DocAuthorError::WouldOverwrite(p) if p.ends_with("t-2-change.values.yaml")),
            "{err}"
        );
        assert!(!dir.join("t-1-create.flow.yaml").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_single_test_case_is_drafted_without_choosing_it() {
        let only = chosen(&["T-1 - Only"], None).expect("no choice needed");
        assert_eq!(only.as_deref(), Some("T-1 - Only"));
    }

    /// Drafting every test case of a test-set report into one flow mixed
    /// unrelated tests; a report with several needs one named.
    #[test]
    fn a_test_set_report_needs_a_test_case_chosen() {
        let names = ["T-1 - Create order", "T-2 - Display order"];
        let err = chosen(&names, None).expect_err("two test cases, no choice");
        assert!(err.to_string().contains("holds 2 test cases"), "{err}");

        for (choice, want) in [
            ("2", "T-2 - Display order"),
            ("t-1 - create  order", "T-1 - Create order"),
            ("t-2", "T-2 - Display order"),
        ] {
            let got = chosen(&names, Some(choice)).expect("chosen");
            assert_eq!(got.as_deref(), Some(want), "--test-case {choice}");
        }
        for choice in ["0", "3", "T-3", "Create"] {
            let err = chosen(&names, Some(choice)).expect_err("no such test case");
            assert!(
                matches!(err, DocAuthorError::UnknownTestCase(_)),
                "{choice}"
            );
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

    /// "Select a location, then click Production Schedule": the vague
    /// half is flagged, the concrete half stays a real step, and the
    /// Expected outcome follows both even when the reply leads with it.
    #[test]
    fn a_step_with_several_actions_flags_only_the_one_it_cannot_map() {
        let mut client = ScriptedModel {
            replies: ["ASSERT: page shows the production schedule\n\
                 UNEXPLAINED: select a location — does not say which one\n\
                 STEP: Press the \"Production Schedule\" button"]
            .into_iter()
            .collect(),
        };
        let lines = translate_record(
            &record(
                "Select the desired location, then click the Production Schedule button.",
                "The production schedule is displayed.",
            ),
            &mut client,
        )
        .expect("translates");
        assert_eq!(
            lines,
            vec![
                DraftLine::Flagged("select a location — does not say which one".into()),
                DraftLine::Action("Press the \"Production Schedule\" button".into()),
                DraftLine::Assert("page shows the production schedule".into()),
            ]
        );
    }

    /// Regression: a real UAT document had a step ("open the exported
    /// file to review it") that the model correctly recognized wasn't an
    /// in-app action, but the old two-way NO_ACTION/UNEXPLAINED split had
    /// nowhere honest to put it — it silently vanished as NO_ACTION,
    /// indistinguishable from a step with no action at all. OUT_OF_SCOPE
    /// gives it a distinct, visible outcome instead.
    #[test]
    fn translate_record_flags_cross_app_steps_as_out_of_scope_not_no_action() {
        let mut client = ScriptedModel {
            replies: [
                "OUT_OF_SCOPE: open the exported file in a spreadsheet program to review it\n\
                 ASSERT: the exported data is available for review",
            ]
            .into_iter()
            .collect(),
        };
        let lines = translate_record(
            &record(
                "Open the exported file to review and analyze the data.",
                "The data is available for review and analysis.",
            ),
            &mut client,
        )
        .expect("translates");
        assert_eq!(lines.len(), 2);
        assert!(matches!(&lines[0], DraftLine::OutOfScope(_)));
        assert!(
            !matches!(&lines[0], DraftLine::Flagged(_)),
            "cross-app steps must not be indistinguishable from in-app ambiguity"
        );
        assert!(matches!(&lines[1], DraftLine::Assert(_)));
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
    fn translate_record_extracts_business_values_separately() {
        let mut client = ScriptedModel {
            replies: ["STEP: Type ${MATERIAL} into the \"Material\" field\n\
                 ASSERT: page shows ${MATERIAL}\n\
                 VALUE: MATERIAL=M-10092"]
            .into_iter()
            .collect(),
        };
        let translated = translate_record_with_values(
            &record(
                "Enter material M-10092 in the Material field.",
                "The page shows M-10092.",
            ),
            &mut client,
        )
        .expect("translates");
        assert_eq!(translated.lines.len(), 2);
        assert_eq!(
            translated.values,
            vec![BusinessValue {
                name: "MATERIAL".to_string(),
                value: "M-10092".to_string()
            }]
        );
    }

    #[test]
    fn values_yaml_deduplicates_identical_values_and_rejects_conflicts() {
        let yaml = values_yaml(&[
            BusinessValue {
                name: "MATERIAL".into(),
                value: "M-10092".into(),
            },
            BusinessValue {
                name: "MATERIAL".into(),
                value: "M-10092".into(),
            },
        ])
        .expect("identical duplicates are harmless");
        assert!(yaml.contains("MATERIAL: M-10092"), "{yaml}");

        let err = values_yaml(&[
            BusinessValue {
                name: "MATERIAL".into(),
                value: "M-10092".into(),
            },
            BusinessValue {
                name: "MATERIAL".into(),
                value: "M-99999".into(),
            },
        ])
        .expect_err("conflicting values are not silently resolved");
        assert!(
            err.to_string().contains("conflicting values"),
            "error names the conflict: {err}"
        );
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
