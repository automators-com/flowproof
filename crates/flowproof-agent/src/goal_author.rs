//! Outcome-only (goal-based) flow authoring: explore a live app toward a
//! `goal:` spec's stated outcome, bounded by an action budget, producing a
//! DRAFT `.flow.yaml` for human review — never a committed trace directly,
//! exactly like [`crate::doc_author`]'s draft-first shape.
//!
//! Forward-only, no rollback: a tried action that doesn't help is never
//! undone. The loop just asks the model for something different from
//! wherever it landed, bounded by `budget` and a repeated-failure give-up
//! heuristic (the same judgment call `crate::repair`'s loop makes: the same
//! failure recurring with no forward movement means more budget will not
//! fix it).
//!
//! Every action the loop tries — including ones later abandoned — is
//! captured by [`flowproof_driver::recording::RunRecorder`] exactly as a
//! normal `record` run captures its steps, so a human reviewing the draft
//! can watch the whole attempt, not just the steps that survived into it.

use std::path::{Path, PathBuf};

use flowproof_driver::app::AppDriver;
use flowproof_driver::recording::{Recording, RecordingOptions, RunRecorder};

use crate::author::{author_checks, author_steps, AuthorContext};
use crate::draft_assembly::{self, DraftLine};
use crate::llm::ModelClient;
use crate::recorder::{action_selector, resolved_assertion_holds, stage_context_and_launch};
use crate::rules::ResolvedAction;
use crate::spec::FlowSpec;
use crate::AgentError;

#[derive(Debug, thiserror::Error)]
pub enum GoalAuthorError {
    #[error("this spec has no `goal:` to explore toward")]
    NoGoal,
    #[error(transparent)]
    Record(#[from] crate::recorder::RecordError),
    #[error(transparent)]
    Driver(#[from] flowproof_driver::DriverError),
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error("model produced a draft that does not parse as a flow spec: {0}")]
    DraftInvalid(#[from] crate::spec::SpecError),
    #[error("could not carry the source spec's context into the draft: {0}")]
    HeadRender(#[from] serde_yaml::Error),
    #[error("io error at '{path}': {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
}

fn io_err(path: &Path, source: std::io::Error) -> GoalAuthorError {
    GoalAuthorError::Io {
        path: path.display().to_string(),
        source,
    }
}

/// The spec-level fields authoring already resolved — `url:`, `session:`,
/// `mock:`, `browser:`, `window:`, `login:` — rendered as raw YAML for
/// `draft_assembly::assemble`'s `extra_head`. Reuses `FlowSpec`'s own
/// `Serialize` rather than hand-writing each field, so nothing here can
/// drift from what the spec type actually carries; `name:`/`app:` (which
/// `assemble` writes itself) and the empty `steps: []` a cleared spec
/// serializes to are the only lines stripped back out.
///
/// Without this, a goal spec's `url:` (the only reason exploration knew
/// where to start) would silently vanish from the draft, and `flowproof
/// record` on it would fail with "the flow has no url:" — a real failure
/// this function exists to prevent, not a hypothetical one.
fn extra_head_yaml(spec: &FlowSpec) -> Result<String, GoalAuthorError> {
    let mut head = spec.clone();
    head.goal = None;
    head.steps = Vec::new();
    let full = serde_yaml::to_string(&head)?;
    let mut out = String::new();
    for line in full.lines() {
        if line.starts_with("name:") || line.starts_with("app:") || line == "steps: []" {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    Ok(out)
}

/// Small, hardcoded, defense-in-depth deny-list: labels an exploring loop
/// must never act on without a human writing the step by hand instead. Not
/// user-configurable in this POC — the desktop app's mandatory confirmation
/// gate is the primary safety control; this is a second layer behind it.
pub const DEFAULT_DENY_PATTERNS: &[&str] = &[
    "delete",
    "remove",
    "cancel order",
    "submit payment",
    "pay now",
    "confirm purchase",
    "checkout",
    "place order",
    "post",
    "approve",
    "reject",
    "sign out",
    "log out",
];

#[derive(Debug, Clone)]
pub struct GoalAuthorOptions {
    pub out: PathBuf,
    /// Maximum actions tried (invoked, deny-listed, or otherwise skipped)
    /// before giving up as [`GoalOutcome::BudgetExhausted`].
    pub budget: usize,
    pub recording: RecordingOptions,
}

impl Default for GoalAuthorOptions {
    fn default() -> Self {
        Self {
            out: PathBuf::from("draft.flow.yaml"),
            budget: 40,
            recording: RecordingOptions::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum GoalOutcome {
    Reached {
        actions_tried: usize,
    },
    BudgetExhausted {
        actions_tried: usize,
    },
    /// The same failure kept recurring with nothing new to try — treated
    /// like `crate::repair`'s own give-up judgment: more budget would not
    /// have helped either.
    NoProgress {
        actions_tried: usize,
        reason: String,
    },
}

pub struct GoalAuthorResult {
    pub flow: PathBuf,
    /// The drafted lines in file order, mirroring [`crate::doc_author::DocAuthorResult`].
    pub lines: Vec<DraftLine>,
    pub outcome: GoalOutcome,
    /// Absent when recording was disabled or the driver could not capture.
    pub recording: Option<Recording>,
    /// Where `recording`'s frames and `gif` actually live on disk, already
    /// resolved (bundle base + `recording.dir`) — `Recording.dir` alone is
    /// only "recording", relative to a bundle base the caller never sees
    /// otherwise, so a naive `flow.parent().join(recording.dir)` silently
    /// resolves to the wrong place (the mistake this field exists to
    /// prevent a caller from repeating).
    pub recording_dir: Option<PathBuf>,
}

pub fn author_from_goal<D: AppDriver, C: ModelClient>(
    spec: &FlowSpec,
    driver: &mut D,
    client: &mut C,
    opts: &GoalAuthorOptions,
) -> Result<GoalAuthorResult, GoalAuthorError> {
    let goal = spec.goal.as_deref().ok_or(GoalAuthorError::NoGoal)?;
    stage_context_and_launch(spec, driver)?;

    let bundle_base = opts
        .out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(format!(".flowproof/explorations/{}", uuid::Uuid::new_v4()));
    let mut recorder = opts
        .recording
        .enabled()
        .then(|| RunRecorder::with_options(&bundle_base, Vec::new(), opts.recording).ok())
        .flatten();

    let mut lines: Vec<DraftLine> = Vec::new();
    let mut history: Vec<String> = Vec::new();
    let mut last_signature: Option<String> = None;
    let mut repeats: u32 = 0;
    const MAX_REPEATS: u32 = 3;

    let outcome = loop {
        let actions_tried = history.len();
        if actions_tried >= opts.budget {
            break GoalOutcome::BudgetExhausted { actions_tried };
        }

        let scene = driver.scene()?.unwrap_or_default();
        let page_text = driver.surface_text().ok();

        // Is the goal reached yet? Read-only, no mutation, no wait — the
        // same live-scene grounding a normal check gets, evaluated with
        // resolved_assertion_holds's single-shot read instead of a step's
        // poll-until-timeout.
        let check_ctx = AuthorContext {
            flow_name: &spec.name,
            app: spec.app.id(),
            url: spec.url.as_deref(),
            prior_steps: &history,
            intent: goal,
            scene: &scene,
            captures: &[],
            today: None,
            page_text: page_text.as_deref(),
        };
        let (checks, _) = author_checks(client, &check_ctx)?;
        if !checks.is_empty()
            && checks
                .iter()
                .all(|a| resolved_assertion_holds(driver, a).ok().flatten() == Some(true))
        {
            lines.push(DraftLine::Assert(goal.to_string()));
            break GoalOutcome::Reached { actions_tried };
        }

        // Not reached: ask for the next thing to try. `prior_steps` is the
        // model's only memory across iterations — it carries both invoked
        // actions and the reasons earlier attempts didn't help, so the
        // model does not just propose the same rejected action again.
        let propose_ctx = AuthorContext {
            flow_name: &spec.name,
            app: spec.app.id(),
            url: spec.url.as_deref(),
            prior_steps: &history,
            intent: &format!(
                "Work toward this outcome, which has not been reached yet: {goal}. \
                 Propose the next action (or a short sequence, if they clearly belong \
                 together) that gets closer, from what the screen shows right now. Do \
                 not repeat something already listed under \"Steps already performed\" \
                 that did not help — try something different."
            ),
            scene: &scene,
            captures: &[],
            today: None,
            page_text: page_text.as_deref(),
        };
        let candidates = author_steps(client, &propose_ctx)?;

        let mut progressed = false;
        let mut round_signature: Option<String> = None;
        for action in candidates {
            if history.len() >= opts.budget {
                break;
            }
            let step_id = format!("explore-{:04}", history.len() + 1);
            match try_action(driver, &mut recorder, &step_id, &action) {
                Attempt::Invoked(text) => {
                    lines.push(DraftLine::Action(text.clone()));
                    history.push(text);
                    progressed = true;
                }
                Attempt::Skipped {
                    history_note,
                    flag_text,
                    signature,
                } => {
                    lines.push(DraftLine::Flagged(flag_text));
                    history.push(history_note);
                    round_signature = Some(signature);
                }
            }
        }

        if progressed {
            repeats = 0;
            last_signature = None;
        } else if round_signature.is_some() && round_signature == last_signature {
            repeats += 1;
            if repeats >= MAX_REPEATS {
                break GoalOutcome::NoProgress {
                    actions_tried: history.len(),
                    reason: format!(
                        "the same outcome kept recurring with nothing new to try: {}",
                        round_signature.unwrap_or_default()
                    ),
                };
            }
        } else {
            repeats = 0;
            last_signature = round_signature;
        }
    };

    let recording = recorder.and_then(|r| r.finish_with_driver(driver));
    let recording_dir = recording.as_ref().map(|r| bundle_base.join(&r.dir));

    let header = format!(
        "# DRAFT generated by `flowproof author-from-goal` exploring toward: {goal}.\n\
         # Review every step below before `flowproof record` - a flagged step\n\
         # (marked TODO) is either a deny-listed action this mode refused to\n\
         # perform, or one it tried and abandoned; watch the recorded attempt\n\
         # to see why before rewriting it by hand."
    );
    let extra_head = extra_head_yaml(spec)?;
    let yaml = draft_assembly::assemble(&header, &spec.name, spec.app.id(), &extra_head, &lines)?;
    std::fs::write(&opts.out, &yaml).map_err(|e| io_err(&opts.out, e))?;

    Ok(GoalAuthorResult {
        flow: opts.out.clone(),
        lines,
        outcome,
        recording,
        recording_dir,
    })
}

enum Attempt {
    Invoked(String),
    Skipped {
        /// What `prior_steps` remembers this attempt as, so the model does
        /// not propose it again.
        history_note: String,
        flag_text: String,
        /// Normalized failure kind, for the repeated-signature give-up check.
        signature: String,
    },
}

fn try_action<D: AppDriver>(
    driver: &mut D,
    recorder: &mut Option<RunRecorder>,
    step_id: &str,
    action: &ResolvedAction,
) -> Attempt {
    let desc = describe_action(action);
    if let Some(pattern) = deny_hit(action) {
        return Attempt::Skipped {
            history_note: format!("(avoided — matched the deny-list \"{pattern}\"): {desc}"),
            flag_text: format!(
                "exploration avoided a deny-listed action (matched \"{pattern}\"): {desc}"
            ),
            signature: format!("deny:{pattern}"),
        };
    }
    let Some(step_text) = render_action_as_step(action) else {
        return Attempt::Skipped {
            history_note: format!("(skipped — action kind not performed by this mode): {desc}"),
            flag_text: format!(
                "exploration proposed an action kind this mode does not perform yet: {desc}"
            ),
            signature: "unsupported-kind".to_string(),
        };
    };
    let Some(selector) = action_selector(action) else {
        return Attempt::Skipped {
            history_note: format!("(skipped — no live selector for): {step_text}"),
            flag_text: format!("exploration could not resolve a live target for: {step_text}"),
            signature: "unresolvable-target".to_string(),
        };
    };
    if let Some(rec) = recorder.as_mut() {
        rec.step_started(driver, step_id);
    }
    let result = match action {
        ResolvedAction::Press { .. } => driver.invoke(&selector),
        ResolvedAction::TypeText { text, .. } => driver.type_text(&selector, text),
        ResolvedAction::SelectOptions { values, .. } => driver.select_options(&selector, values),
        ResolvedAction::SetChecked { checked, .. } => driver.set_checked(&selector, *checked),
        _ => unreachable!("render_action_as_step already filtered to a supported kind"),
    };
    if let Some(rec) = recorder.as_mut() {
        rec.step_finished(driver);
    }
    match result {
        Ok(()) => Attempt::Invoked(step_text),
        Err(e) => Attempt::Skipped {
            history_note: format!("(tried and failed: {e}): {step_text}"),
            flag_text: format!("exploration tried and failed: {step_text} ({e})"),
            signature: format!("driver-error:{e}"),
        },
    }
}

/// Human-readable enough for a flagged draft step's "Observed:" text — the
/// reviewer reads this in a YAML comment, not a debugger. Falls back to
/// Rust's `Debug` only for a genuinely unexpected shape; every action kind
/// actually seen in practice (an assertion proposed where an action was
/// asked for, or an action kind this mode doesn't perform) gets real prose.
fn describe_action(action: &ResolvedAction) -> String {
    match action {
        ResolvedAction::AssertText { .. }
        | ResolvedAction::AssertPresence { .. }
        | ResolvedAction::AssertChecked { .. }
        | ResolvedAction::AssertCount { .. }
        | ResolvedAction::AssertEnabled { .. }
        | ResolvedAction::AssertAttribute { .. }
        | ResolvedAction::AssertStyle { .. }
        | ResolvedAction::AssertCaptured { .. }
        | ResolvedAction::AssertScreenshot { .. } => {
            "an assertion, proposed where an action toward the goal was asked for".to_string()
        }
        ResolvedAction::Upload { .. } => "a file upload".to_string(),
        ResolvedAction::Drag { .. } => "a drag".to_string(),
        ResolvedAction::ContextClick { .. } => "a right-click".to_string(),
        ResolvedAction::DoubleClick { .. } => "a double-click".to_string(),
        ResolvedAction::Hover { .. } => "a hover".to_string(),
        ResolvedAction::ClickAt { .. } => "a click at a specific point".to_string(),
        ResolvedAction::PressKey { key, .. } => format!("pressing the {key} key"),
        ResolvedAction::Navigate { path } => format!("navigating to {path}"),
        ResolvedAction::Reload => "reloading the page".to_string(),
        ResolvedAction::Capture { .. } => "capturing a value".to_string(),
        ResolvedAction::CaptureDownload { .. } => "capturing a download".to_string(),
        ResolvedAction::Clear { .. } => "clearing a field".to_string(),
        ResolvedAction::TypeFocused { .. } => "typing into the focused element".to_string(),
        // Press/TypeText/SelectOptions/SetChecked land here only when their
        // *target* wasn't simple enough to render (a table cell, a scoped
        // or framed anchor) — the common case is already handled before
        // describe_action is ever called.
        _ => format!("{action:?}"),
    }
}

/// The action's own human-visible label, when this mode knows how to read
/// one — used for both the deny-list check and step-grammar rendering.
/// `None` for target kinds this POC does not perform (`Nth`/`Cell`/
/// `Scoped`/`Framed`/`Surface`): a click on the *third* matching button or
/// one identified by table-row anchor is real automation the deterministic
/// rules grammar already expresses, but reusing that here would mean
/// reimplementing its rendering, so it is left as a follow-up rather than
/// attempted partially.
fn action_label(action: &ResolvedAction) -> Option<String> {
    match action {
        ResolvedAction::Press { label, .. } => Some(label.clone()),
        ResolvedAction::TypeText { target, .. }
        | ResolvedAction::SelectOptions { target, .. }
        | ResolvedAction::SetChecked { target, .. } => target_label(target),
        _ => None,
    }
}

fn target_label(target: &crate::rules::Target) -> Option<String> {
    match target {
        crate::rules::Target::Text(t) => Some(t.clone()),
        crate::rules::Target::AutomationId(id) => Some(id.clone()),
        crate::rules::Target::Css(css) => Some(css.clone()),
        _ => None,
    }
}

fn quoted_target(target: &crate::rules::Target) -> Option<String> {
    match target {
        crate::rules::Target::Text(t) => Some(format!("\"{}\"", t.replace('"', "'"))),
        crate::rules::Target::AutomationId(id) => Some(format!("\"id:{id}\"")),
        crate::rules::Target::Css(css) => Some(format!("\"css:{css}\"")),
        _ => None,
    }
}

fn deny_hit(action: &ResolvedAction) -> Option<&'static str> {
    let label = action_label(action)?.to_lowercase();
    DEFAULT_DENY_PATTERNS
        .iter()
        .find(|p| label.contains(**p))
        .copied()
}

/// Render a grounded action as a step in flowproof's own grammar
/// ([`draft_assembly::STEP_GRAMMAR_RULES`]), so the draft's `record` pass
/// can re-ground it deterministically without a model call. `None` for an
/// action kind or target this POC's exploration loop does not perform —
/// see [`action_label`].
fn render_action_as_step(action: &ResolvedAction) -> Option<String> {
    match action {
        ResolvedAction::Press { target, .. } => {
            Some(format!("Press the {} button", quoted_target(target)?))
        }
        ResolvedAction::TypeText { target, text } => Some(format!(
            "Type {text} into the {} field",
            quoted_target(target)?
        )),
        ResolvedAction::SelectOptions { target, values } => Some(format!(
            "Select {} from the {} field",
            values.join(", "),
            quoted_target(target)?
        )),
        ResolvedAction::SetChecked { target, checked } => Some(format!(
            "{} the {} checkbox",
            if *checked { "Check" } else { "Uncheck" },
            quoted_target(target)?
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flowproof_driver::mock::MockAppDriver;
    use std::collections::VecDeque;

    struct Scripted {
        replies: VecDeque<&'static str>,
    }

    impl Scripted {
        fn new(replies: &[&'static str]) -> Self {
            Self {
                replies: replies.iter().copied().collect(),
            }
        }
    }

    impl ModelClient for Scripted {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            Ok(self
                .replies
                .pop_front()
                .expect("test provided enough scripted replies")
                .to_string())
        }
        fn identity(&self) -> (String, String) {
            ("test".to_string(), "test".to_string())
        }
    }

    fn goal_spec(goal: &str) -> FlowSpec {
        FlowSpec::parse(&format!(
            "name: Explore\napp: web\nurl: https://e.test/x\ngoal: {goal}\n"
        ))
        .expect("goal spec parses")
    }

    fn opts() -> GoalAuthorOptions {
        let dir = std::env::temp_dir().join(format!(
            "flowproof-goal-author-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir creates");
        GoalAuthorOptions {
            out: dir.join("draft.flow.yaml"),
            budget: 10,
            recording: RecordingOptions {
                detail: flowproof_driver::recording::RecordingDetail::Off,
                video: false,
                highlight_cursor: false,
            },
        }
    }

    #[test]
    fn recording_dir_points_at_the_gif_that_actually_exists() {
        let spec = goal_spec("the page confirms greet");
        let mut driver = MockAppDriver::new(&["go"]).revealing("go", &["confirm"]);
        driver.frame = Some(image::RgbaImage::new(4, 4));
        driver.scene = Some(
            r#"[{"target":"id:go","tag":"button","text":"Go"},
                {"target":"id:confirm","tag":"span","text":"Confirmed"}]"#
                .to_string(),
        );
        let mut client = Scripted::new(&[
            r#"{"action":"assert_visible","target":"id:confirm","present":true}"#,
            r#"{"action":"click","target":"id:go"}"#,
            r#"{"action":"assert_visible","target":"id:confirm","present":true}"#,
        ]);
        let mut with_video = opts();
        with_video.recording = RecordingOptions {
            detail: flowproof_driver::recording::RecordingDetail::Full,
            video: true,
            highlight_cursor: false,
        };
        let result = author_from_goal(&spec, &mut driver, &mut client, &with_video)
            .expect("exploration succeeds");
        let dir = result
            .recording_dir
            .as_ref()
            .expect("recording was enabled");
        let gif = result.recording.as_ref().and_then(|r| r.gif.as_deref());
        let gif = gif.expect("video: true must assemble a gif");
        assert!(
            dir.join(gif).is_file(),
            "recording_dir joined with the reported gif name must be the real file, \
             got {}",
            dir.join(gif).display()
        );
        std::fs::remove_dir_all(result.flow.parent().expect("draft path has a parent")).ok();
    }

    #[test]
    fn reaches_the_goal_in_one_action() {
        let spec = goal_spec("the page confirms greet");
        // "confirm" is not in the elements list yet: it only exists once
        // "go" is invoked (revealing), so the goal-check genuinely reads
        // false the first time and true the second.
        let mut driver = MockAppDriver::new(&["go"]).revealing("go", &["confirm"]);
        driver.scene = Some(
            r#"[{"target":"id:go","tag":"button","text":"Go"},
                {"target":"id:confirm","tag":"span","text":"Confirmed"}]"#
                .to_string(),
        );
        let mut client = Scripted::new(&[
            // check: goal not yet reached ("confirm" does not exist yet)
            r#"{"action":"assert_visible","target":"id:confirm","present":true}"#,
            // propose: one action
            r#"{"action":"click","target":"id:go"}"#,
            // check: reached ("confirm" now exists, per `revealing`)
            r#"{"action":"assert_visible","target":"id:confirm","present":true}"#,
        ]);
        let result = author_from_goal(&spec, &mut driver, &mut client, &opts())
            .expect("exploration succeeds");
        assert!(matches!(result.outcome, GoalOutcome::Reached { .. }));
        assert!(result
            .lines
            .iter()
            .any(|l| matches!(l, DraftLine::Action(a) if a.contains("id:go"))));
        assert!(matches!(result.lines.last(), Some(DraftLine::Assert(_))));
        std::fs::remove_dir_all(result.flow.parent().expect("draft path has a parent")).ok();
    }

    #[test]
    fn deny_listed_actions_are_flagged_not_invoked() {
        let spec = goal_spec("the order is deleted");
        let mut driver = MockAppDriver::new(&["delete-btn"]);
        driver.scene = Some(
            r#"[{"target":"id:delete-btn","tag":"button","text":"Delete order"}]"#.to_string(),
        );
        // Every round proposes the same deny-listed action, and the check
        // never reads true (nothing else in the scene ever becomes real);
        // the give-up heuristic must fire well within the ten-action budget.
        let check = r#"{"action":"assert_visible","target":"id:delete-btn","present":false}"#;
        let propose = r#"{"action":"click","target":"id:delete-btn"}"#;
        let mut client = Scripted::new(&[
            check, propose, check, propose, check, propose, check, propose,
        ]);
        let result = author_from_goal(&spec, &mut driver, &mut client, &opts())
            .expect("exploration returns a partial draft, not an error");
        assert!(matches!(result.outcome, GoalOutcome::NoProgress { .. }));
        assert!(
            driver.invoked.is_empty(),
            "the deny-listed press must never reach the driver"
        );
        assert!(result
            .lines
            .iter()
            .any(|l| matches!(l, DraftLine::Flagged(f) if f.contains("deny-list"))));
        std::fs::remove_dir_all(result.flow.parent().expect("draft path has a parent")).ok();
    }

    #[test]
    fn budget_exhausts_when_progress_keeps_changing() {
        let spec = goal_spec("the page confirms greet");
        let mut driver = MockAppDriver::new(&["a", "b", "c"]);
        driver.scene = Some(
            r#"[{"target":"id:a","tag":"button","text":"A"},
                {"target":"id:b","tag":"button","text":"B"},
                {"target":"id:c","tag":"button","text":"C"},
                {"target":"id:done","tag":"span","text":"Done"}]"#
                .to_string(),
        );
        // "done" never appears (nothing reveals it), so every check reads
        // false and the loop keeps trying different, successful actions
        // until the budget itself ends it.
        let check = r#"{"action":"assert_visible","target":"id:done","present":true}"#;
        let mut replies = Vec::new();
        for id in ["a", "b", "c", "a", "b"] {
            replies.push(check);
            replies.push(match id {
                "a" => r#"{"action":"click","target":"id:a"}"#,
                "b" => r#"{"action":"click","target":"id:b"}"#,
                _ => r#"{"action":"click","target":"id:c"}"#,
            });
        }
        let scripted: Vec<&'static str> = replies;
        let mut client = Scripted::new(&scripted);
        let mut small_budget = opts();
        small_budget.budget = 5;
        let result = author_from_goal(&spec, &mut driver, &mut client, &small_budget)
            .expect("exploration returns a partial draft, not an error");
        assert!(matches!(
            result.outcome,
            GoalOutcome::BudgetExhausted { .. }
        ));
        std::fs::remove_dir_all(result.flow.parent().expect("draft path has a parent")).ok();
    }
}
