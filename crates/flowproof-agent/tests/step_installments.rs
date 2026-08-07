//! A step authored in installments, driven end to end through `record`.
//!
//! The unit tests next to the authoring code prove that a reply carrying
//! `step_continues` splits and that `step_complete` is recognised. Neither
//! proves the thing the feature is for: that the recorder PERFORMS the first
//! installment, lets the page change, reads the screen it became, and writes
//! both installments into one trace. That needs the whole pipeline — spec,
//! driver, model client, minted trace — so it lives here rather than beside
//! any one of them.
//!
//! Everything runs against `MockAppDriver`, so these are green on the stub
//! backend with no browser and no key.

use std::collections::VecDeque;
use std::path::Path;

use flowproof_agent::recorder::record_with_client;
use flowproof_agent::{AgentError, Author, ClarifyStage, FlowSpec, ModelClient, RecordError};
use flowproof_driver::mock::MockAppDriver;
use flowproof_trace::TraceLine;

/// Hands out one scripted reply per call and keeps every prompt it was sent,
/// so a test can play both sides of a two-installment step: what the model
/// authored against the first screen, and what it authored against the one
/// the first installment produced.
struct ScriptedClient {
    replies: VecDeque<String>,
    prompts: Vec<String>,
}

impl ScriptedClient {
    fn new(replies: &[&str]) -> Self {
        Self {
            replies: replies.iter().map(|r| (*r).to_string()).collect(),
            prompts: Vec::new(),
        }
    }
}

impl ModelClient for ScriptedClient {
    fn complete(&mut self, _system: &str, user: &str) -> Result<String, AgentError> {
        self.prompts.push(user.to_string());
        self.replies
            .pop_front()
            .ok_or_else(|| AgentError::Authoring {
                step: "scripted".into(),
                reason: "the recorder asked for more replies than the test scripted".into(),
            })
    }

    fn identity(&self) -> (String, String) {
        ("openai-compatible".into(), "test-model".into())
    }
}

const FIRST_SCREEN: &str = r##"[{"target":"css:#make","tag":"input","label":"Make"},
     {"target":"css:#next","tag":"button","text":"Next"}]"##;

const SECOND_SCREEN: &str = r##"[{"target":"css:#model","tag":"input","label":"Model"},
     {"target":"css:#save","tag":"button","text":"Save"}]"##;

const SPEC: &str = "name: Quote
app: web
url: https://example.test
steps:
  - Enter the make, then the model on the screen that follows, and save
";

/// A wizard whose second page only exists once the first one is submitted:
/// `#make` and `#next` to begin with, `#model` and `#save` after the click.
fn two_page_wizard() -> MockAppDriver {
    let mut driver = MockAppDriver::new(&["#make", "#next"])
        .revealing("#next", &["#model", "#save"])
        .hiding("#next", &["#make", "#next"])
        // Authoring reads the screen, and so does the guard in front of the
        // click that leaves it. Both happen while page one is still up; every
        // read after the click falls through to the static scene, which is
        // page two.
        .with_scene_sequence(&[FIRST_SCREEN, FIRST_SCREEN])
        .with_surface_text("Step 2 of 2: which model?");
    driver.scene = Some(SECOND_SCREEN.into());
    // The one reading taken before the click. What the page SAYS is how the
    // test can tell which screen a prompt was written against.
    driver.text_sequence.insert(
        MockAppDriver::SURFACE.to_string(),
        VecDeque::from(vec!["Step 1 of 2: which make?".to_string()]),
    );
    driver
}

/// Every step in the trace, as `(action type, the selectors it carries)`.
fn recorded(path: &Path) -> Vec<(String, String)> {
    let contents = std::fs::read_to_string(path).expect("the trace was written");
    contents
        .lines()
        .filter_map(|line| match TraceLine::parse(line).expect("line parses") {
            TraceLine::Header(_) => None,
            TraceLine::Step(step) => {
                let value = serde_json::to_value(&step).expect("a step serializes");
                let kind = value["action"]["type"]
                    .as_str()
                    .expect("every action is tagged")
                    .to_string();
                let selectors =
                    serde_json::to_string(&step.selectors).expect("selectors serialize");
                Some((kind, selectors))
            }
        })
        .collect()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(name);
    std::fs::remove_file(&path).ok();
    path
}

/// The step the recorder could not have planned in one go.
///
/// `#model` is not on the screen the step is authored against, and no amount
/// of prompting invents it — the model can only say so and be asked again
/// once the page has moved. Before this could be said, the whole step failed
/// here: either the model guessed a selector that did not ground, or it
/// authored only the half it could see and the rest was silently dropped.
#[test]
fn a_step_spanning_two_screens_records_both_installments() {
    let spec = FlowSpec::parse(SPEC).expect("spec parses");
    let mut driver = two_page_wizard();
    let mut client = ScriptedClient::new(&[
        r##"[{"action":"type_text","target":"css:#make","text":"BMW"},
             {"action":"click","target":"css:#next"},
             {"action":"step_continues"}]"##,
        r##"[{"action":"type_text","target":"css:#model","text":"M3"},
             {"action":"click","target":"css:#save"}]"##,
    ]);
    let out = scratch("flowproof-two-installments.trace.jsonl");

    let summary = record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
        .expect("a step that spans two screens records");

    // What actually happened to the application, in order: the first
    // installment ran, the page changed, the second installment ran on it.
    assert_eq!(
        driver.typed,
        vec![
            ("#make".to_string(), "BMW".to_string()),
            ("#model".to_string(), "M3".to_string()),
        ],
        "the second field is on a screen the first installment produced"
    );
    assert_eq!(driver.invoked, vec!["#next", "#save"]);

    // And what a reviewer and a replayer see: one straight line of four
    // actions under the one intent the flow asked for. The trace records
    // what ran, never the deliberation that chose it, so a step authored in
    // two installments must be indistinguishable from one authored in one.
    assert_eq!(summary.steps, 4);
    let steps = recorded(&out);
    let kinds: Vec<&str> = steps.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds, vec!["type_text", "click", "type_text", "click"]);
    for (target, (_, selectors)) in ["#make", "#next", "#model", "#save"].iter().zip(&steps) {
        assert!(
            selectors.contains(target),
            "step targets {target}, but its selectors are {selectors}"
        );
    }

    // Exactly two model calls: the first reply and the remainder. A third
    // would mean the continuation budget had stopped bounding anything.
    assert_eq!(client.prompts.len(), 2);
    assert!(
        client.prompts[0].contains("Step 1 of 2"),
        "the step is authored against the screen it starts on: {}",
        client.prompts[0]
    );
    let remainder = &client.prompts[1];
    assert!(
        remainder.contains("Step 2 of 2"),
        "the remainder is authored against the screen the click produced, \
         not the one that was there before it: {remainder}"
    );
    assert!(
        remainder.contains("PARTWAY DONE") && remainder.contains(r#"typed "BMW""#),
        "and it is told what already ran, so it does not repeat it: {remainder}"
    );
    assert!(
        !remainder.contains("M3"),
        "nothing from the second installment can be in the prompt that authored it: {remainder}"
    );

    std::fs::remove_file(&out).ok();
}

/// The other half, so the test above cannot be satisfied by always asking
/// for more: when the page turns out to have nothing left of the step on it,
/// `step_complete` ends the step, and the trace holds only what ran.
///
/// This is also the only place `step_complete` is legal. A recorder that
/// accepted it anywhere would let a model end a step it never started, and
/// the empty trace would replay green.
#[test]
fn step_complete_ends_a_step_and_records_only_what_ran() {
    let spec = FlowSpec::parse(SPEC).expect("spec parses");
    let mut driver = two_page_wizard();
    let mut client = ScriptedClient::new(&[
        r##"[{"action":"type_text","target":"css:#make","text":"BMW"},
             {"action":"click","target":"css:#next"},
             {"action":"step_continues"}]"##,
        r##"[{"action":"step_complete"}]"##,
    ]);
    let out = scratch("flowproof-installment-complete.trace.jsonl");

    let summary = record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
        .expect("a step the page had already satisfied records what it did");

    assert_eq!(driver.typed, vec![("#make".to_string(), "BMW".to_string())]);
    assert_eq!(driver.invoked, vec!["#next"]);
    assert_eq!(summary.steps, 2);
    let kinds: Vec<String> = recorded(&out).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(
        kinds,
        vec!["type_text", "click"],
        "step_complete is an answer, not an action: nothing is minted for it"
    );
    assert_eq!(
        client.prompts.len(),
        2,
        "`step_complete` ends the step — it is not a rejected reply to retry"
    );

    std::fs::remove_file(&out).ok();
}

/// `step_complete` in a FIRST reply is a step that never happened.
///
/// The model has understood the vocabulary and mistaken the moment, and the
/// refusal has to say which — lumped in with typos it reads as "unknown
/// action 'step_complete'", which tells a model that has just used the word
/// correctly-shaped that the word does not exist, and the retry reaches for
/// something else instead of authoring the step.
#[test]
fn step_complete_in_a_first_reply_is_refused_by_name() {
    let spec = FlowSpec::parse(
        "name: Quote\napp: web\nurl: https://example.test\nsteps:\n  - Save the quote\n",
    )
    .expect("spec parses");
    let mut driver = MockAppDriver::new(&["#save"]);
    driver.scene = Some(r##"[{"target":"css:#save","tag":"button","text":"Save"}]"##.into());
    // Twice, because the authoring loop self-corrects once. Both are refused.
    let mut client = ScriptedClient::new(&[
        r##"[{"action":"step_complete"}]"##,
        r##"[{"action":"step_complete"}]"##,
    ]);
    let out = scratch("flowproof-premature-complete.trace.jsonl");

    let err = record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
        .expect_err("a step cannot be declared complete before it has done anything");

    let RecordError::NeedsClarification(clarification) = err else {
        panic!("expected a clarification the driving agent can act on");
    };
    assert_eq!(clarification.stage, ClarifyStage::Model);
    assert!(
        clarification
            .reason
            .contains("valid only when you are asked to CONTINUE"),
        "the refusal names the moment, not the vocabulary: {}",
        clarification.reason
    );
    assert!(
        !clarification.reason.contains("unknown action"),
        "and it is not lumped in with a typo: {}",
        clarification.reason
    );
    assert!(
        client.prompts[1].contains("valid only when you are asked to CONTINUE"),
        "the retry is told why, or it is the same question twice: {}",
        client.prompts[1]
    );

    assert!(
        driver.invoked.is_empty() && driver.typed.is_empty(),
        "and nothing was performed on the way to refusing"
    );
    assert!(
        !out.exists(),
        "no trace is left behind claiming the step was done"
    );
}
