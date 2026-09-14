//! Plan 007's autonomous repair loop: given a `record` failure, ask the
//! configured authoring model for a minimal `.flow.yaml` edit, apply it, and
//! let the caller rerun. This module is deliberately driver-agnostic — it
//! never runs a flow itself, only diagnoses a [`RecordError`] and proposes or
//! applies a single-step YAML patch. The caller (the CLI, today) owns the
//! rerun loop because rebuilding a live driver is app/surface plumbing this
//! crate does not otherwise touch.
//!
//! What this module will never do: edit anything other than the `steps:`
//! entry it was asked to patch, or touch a file that is not the flow spec
//! itself. There is no code-editing capability here, by construction.
//!
//! Two apply paths exist deliberately: [`apply_patch`] can rewrite a whole
//! step, while [`widen_timeout`] can only change a step's `within Ns` wait
//! window. The narrower one exists so a load-timing race — the target was
//! right, the page just hadn't settled — gets fixed without giving the
//! model a chance to also change what the step is waiting for.

use crate::llm::ModelClient;
use crate::recorder::RecordError;
use crate::spec::FlowSpec;
use crate::AgentError;

/// Bounds on the repair loop. The caller stops after `max_attempts` failed
/// reruns, or earlier if [`FailureContext::category`] repeats with no
/// progress — see plan 007's "engine-gap stop condition".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairOptions {
    pub max_attempts: usize,
}

impl Default for RepairOptions {
    fn default() -> Self {
        Self { max_attempts: 3 }
    }
}

/// A best-effort classification of a [`RecordError`] into plan 007's
/// diagnosis taxonomy, plus whatever the error can name about which step
/// failed. `failing_intent` is `None` for errors that are not about one
/// specific step (e.g. a bad spec), in which case repair cannot proceed.
#[derive(Debug, Clone)]
pub struct FailureContext {
    pub category: &'static str,
    pub failing_intent: Option<String>,
    pub detail: String,
}

/// Classify a [`RecordError`] for the repair loop. This mirrors the
/// taxonomy in plan 007 as closely as the current error shapes allow — most
/// `RecordError` variants do not carry a step index today, only the intent
/// text, so matching a step back to the spec is done by intent text
/// (see [`find_step_index`]).
pub fn diagnose(err: &RecordError) -> FailureContext {
    match err {
        RecordError::ElementNotFound { intent, selector } => FailureContext {
            category: "missing-target",
            failing_intent: Some(intent.clone()),
            detail: format!("element for step '{intent}' not found: [{selector}]"),
        },
        RecordError::AssertMismatch {
            intent,
            expected,
            actual,
        } => FailureContext {
            category: "wrong-wait-signal",
            failing_intent: Some(intent.clone()),
            detail: format!("assertion '{intent}' does not hold: expected {expected}, {actual}"),
        },
        RecordError::Dialog { intent, reason } => FailureContext {
            category: "occluded-target",
            failing_intent: Some(intent.clone()),
            detail: format!("step '{intent}': {reason}"),
        },
        RecordError::NeedsClarification(clarification) => FailureContext {
            category: "ambiguous-step",
            failing_intent: Some(clarification.step.clone()),
            detail: clarification.reason.clone(),
        },
        RecordError::Driver(driver_err) => FailureContext {
            category: "page-not-ready",
            failing_intent: None,
            detail: driver_err.to_string(),
        },
        // The previous step's target hadn't actually settled when the next
        // step already moved on — a load-timing race, not a wrong text
        // target. Distinct from the generic engine-gap bucket so the loop
        // can offer "widen the timeout" instead of guessing new step text.
        RecordError::Agent(AgentError::PreviousStepIncomplete { step, evidence }) => {
            FailureContext {
                category: "page-not-ready-transient",
                failing_intent: Some(step.clone()),
                detail: evidence.clone(),
            }
        }
        other => FailureContext {
            category: "engine-gap",
            failing_intent: None,
            detail: other.to_string(),
        },
    }
}

/// A model-proposed edit to exactly one `steps:` entry.
///
/// Exactly one of `step_yaml`, `widen_timeout_seconds`, or `engine_gap`
/// should be set. `widen_timeout_seconds` is the case where the model judges
/// the step's target correct but the page hadn't settled yet — a load-timing
/// race rather than a wrong-text problem — so the fix is a longer wait on
/// the same step, not different wording.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ProposedPatch {
    /// `None` means either an engine gap or a timeout-widen — see the other
    /// two fields.
    pub step_yaml: Option<String>,
    pub rationale: String,
    #[serde(default)]
    pub engine_gap: Option<String>,
    #[serde(default)]
    pub widen_timeout_seconds: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("repair needs a specific failing step, but this failure does not name one: {0}")]
    NoFailingStep(String),
    #[error("no step in the flow matches the failing intent '{0}'")]
    StepNotFound(String),
    #[error("model did not return a usable patch: {0}")]
    BadModelOutput(String),
    #[error(transparent)]
    Model(#[from] AgentError),
    #[error("flow yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

/// Ask the model for a minimal fix to the failing step. `flow_yaml` is the
/// spec's raw source; only structured, redacted context is sent — no
/// screenshots, no raw business data, per plan 007's model-input policy.
pub fn propose_patch<C: ModelClient + ?Sized>(
    client: &mut C,
    flow_yaml: &str,
    ctx: &FailureContext,
    completed_steps: &[String],
    attempt: usize,
    previous_attempts: &[String],
) -> Result<ProposedPatch, RepairError> {
    let failing_intent = ctx
        .failing_intent
        .as_ref()
        .ok_or_else(|| RepairError::NoFailingStep(ctx.detail.clone()))?;

    let system = "You are a cautious support engineer repairing a Flowproof \
        .flow.yaml test file after a failed live recording. You may propose \
        an edit to exactly ONE step of the `steps:` list — the one that \
        failed. You must never remove or weaken a business-outcome assertion \
        just to make the step pass. If the failure looks like a Flowproof \
        engine limitation rather than something a flow edit can fix, say so \
        instead of proposing a patch. Reply with ONLY a JSON object, no \
        markdown fences, no prose: \
        {\"step_yaml\": string or null, \"rationale\": string, \"engine_gap\": string or null, \
        \"widen_timeout_seconds\": number or null}. \
        Exactly ONE of step_yaml, engine_gap, or widen_timeout_seconds should be \
        non-null. \
        step_yaml must be exactly the VALUE of one `steps:` list item (either \
        a bare string step or a single-key mapping) — do NOT include the \
        leading `- ` list marker itself, only what would follow it. \
        widen_timeout_seconds: if the error shows the step's target text or \
        condition is CORRECT but the page simply hadn't finished loading or \
        settling yet (e.g. the error says a previous step's result was \
        'still loading', or the expected text appears in the DOM but the \
        step's wait window was too short) — this is a load-timing race, not \
        a wrong target — set widen_timeout_seconds to a larger number of \
        seconds for the SAME wait condition, and leave step_yaml null. Only \
        use this when the target itself is confirmed correct; if the target \
        text genuinely never appears anywhere in the captured DOM, that is a \
        wrong-target problem — use step_yaml instead. Set step_yaml and \
        widen_timeout_seconds to null and engine_gap to a short explanation \
        if this is not fixable by editing the flow.";

    let mut user = format!(
        "Flow file:\n```yaml\n{flow_yaml}\n```\n\n\
         Failing step: {failing_intent}\n\
         Diagnosis category: {}\n\
         Error detail: {}\n\
         Steps completed before the failure: {:?}\n\
         Repair attempt: {} of budget\n",
        ctx.category, ctx.detail, completed_steps, attempt
    );
    if !previous_attempts.is_empty() {
        user.push_str(&format!(
            "Previous attempts on this same failure that did NOT fix it: {:?}\n\
             Propose something materially different, or set engine_gap if \
             nothing left is worth trying.\n",
            previous_attempts
        ));
    }

    let raw = client.complete(system, &user).map_err(RepairError::Model)?;
    parse_patch(&raw)
}

fn parse_patch(raw: &str) -> Result<ProposedPatch, RepairError> {
    let trimmed = strip_code_fence(raw.trim());
    serde_json::from_str(trimmed)
        .map_err(|e| RepairError::BadModelOutput(format!("{e}: {trimmed}")))
}

fn strip_code_fence(s: &str) -> &str {
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
}

/// Find the index of the spec step whose rendered intent matches
/// `failing_intent`, so the raw YAML's `steps[i]` can be replaced.
pub fn find_step_index(spec: &FlowSpec, failing_intent: &str) -> Option<usize> {
    spec.steps.iter().position(|s| s.intent() == failing_intent)
}

/// Apply a proposed patch to the flow's raw YAML source, replacing the
/// `steps` entry at `step_index` with `patch.step_yaml`. Returns the new
/// full document text. This is a whole-document round-trip through
/// `serde_yaml::Value` rather than a text-level line edit, so it does not
/// preserve comments or formatting elsewhere in the file — an accepted
/// trade for a correct, generic single-step replacement (see plan 007's
/// open questions).
pub fn apply_patch(
    flow_yaml: &str,
    step_index: usize,
    step_yaml: &str,
) -> Result<String, RepairError> {
    let mut doc: serde_yaml::Value = serde_yaml::from_str(flow_yaml)?;
    let mut replacement: serde_yaml::Value = serde_yaml::from_str(step_yaml)?;
    // A model occasionally includes the leading `- ` list marker in its
    // answer despite being asked for the bare item — that parses as a
    // one-element sequence rather than the step itself, which would nest a
    // list inside `steps:` instead of replacing an entry. Unwrap it rather
    // than trust prompt wording alone to prevent it.
    if let serde_yaml::Value::Sequence(items) = &replacement {
        if items.len() == 1 {
            replacement = items[0].clone();
        }
    }
    let steps = doc
        .get_mut("steps")
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| {
            RepairError::BadModelOutput("flow yaml has no `steps:` sequence".to_string())
        })?;
    if step_index >= steps.len() {
        return Err(RepairError::StepNotFound(format!(
            "index {step_index} out of range ({} steps)",
            steps.len()
        )));
    }
    steps[step_index] = replacement;
    serde_yaml::to_string(&doc).map_err(RepairError::Yaml)
}

/// Hard ceiling on a model-widened timeout. Without this, a model that
/// mistakes a genuinely broken target for a timing race could quietly turn
/// a flaky test into one with a multi-minute wait rather than a real fix.
const MAX_WIDENED_TIMEOUT_SECONDS: u64 = 120;

fn timeout_pattern() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"within \d+s").expect("static regex is valid"))
}

/// Widen the `within Ns` wait window on the step at `step_index`, leaving
/// every other word of the step untouched. This is deliberately narrower
/// than [`apply_patch`]: a timing-race fix should not be able to change
/// *what* the step waits for, only *how long*.
pub fn widen_timeout(
    flow_yaml: &str,
    step_index: usize,
    new_timeout_secs: u64,
) -> Result<String, RepairError> {
    let capped = new_timeout_secs.min(MAX_WIDENED_TIMEOUT_SECONDS);
    let mut doc: serde_yaml::Value = serde_yaml::from_str(flow_yaml)?;
    let steps = doc
        .get_mut("steps")
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| {
            RepairError::BadModelOutput("flow yaml has no `steps:` sequence".to_string())
        })?;
    if step_index >= steps.len() {
        return Err(RepairError::StepNotFound(format!(
            "index {step_index} out of range ({} steps)",
            steps.len()
        )));
    }
    let rewrite = |s: &mut String| {
        if timeout_pattern().is_match(s) {
            *s = timeout_pattern()
                .replace(s, format!("within {capped}s"))
                .into_owned();
            true
        } else {
            false
        }
    };
    let widened = match &mut steps[step_index] {
        serde_yaml::Value::String(s) => rewrite(s),
        serde_yaml::Value::Mapping(map) => {
            let mut any = false;
            for (_, v) in map.iter_mut() {
                if let serde_yaml::Value::String(s) = v {
                    any |= rewrite(s);
                }
            }
            any
        }
        _ => false,
    };
    if !widened {
        return Err(RepairError::BadModelOutput(
            "step has no `within Ns` wait window to widen".to_string(),
        ));
    }
    serde_yaml::to_string(&doc).map_err(RepairError::Yaml)
}

/// One iteration of the repair loop, for reporting purposes.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairAttempt {
    pub attempt: usize,
    pub category: String,
    pub failing_intent: Option<String>,
    pub error_detail: String,
    pub rationale: Option<String>,
    pub applied: bool,
}

/// Why the repair loop stopped.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum RepairOutcome {
    Passed,
    EngineGap { reason: String },
    BudgetExhausted { last_error: String },
    NotApplicable { reason: String },
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairReport {
    pub attempts: Vec<RepairAttempt>,
    pub outcome: RepairOutcome,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_patch_json_with_code_fence() {
        let raw = "```json\n{\"step_yaml\": \"Click \\\"Submit\\\"\", \"rationale\": \"use the visible label\", \"engine_gap\": null}\n```";
        let patch = parse_patch(raw).expect("patch parses");
        assert_eq!(patch.step_yaml.as_deref(), Some("Click \"Submit\""));
        assert!(patch.engine_gap.is_none());
    }

    #[test]
    fn parses_engine_gap_patch() {
        let raw = r#"{"step_yaml": null, "rationale": "no fix", "engine_gap": "split buttons unsupported"}"#;
        let patch = parse_patch(raw).expect("patch parses");
        assert!(patch.step_yaml.is_none());
        assert_eq!(
            patch.engine_gap.as_deref(),
            Some("split buttons unsupported")
        );
    }

    #[test]
    fn bad_model_output_is_reported_not_panicked() {
        let err = parse_patch("not json at all").expect_err("bad json is rejected");
        assert!(matches!(err, RepairError::BadModelOutput(_)));
    }

    #[test]
    fn apply_patch_replaces_only_the_named_step() {
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Click \"Old\"\n  - Click \"Keep\"\n";
        let patched = apply_patch(flow, 0, "Click \"New\"").expect("patch applies");
        let doc: serde_yaml::Value = serde_yaml::from_str(&patched).expect("patched yaml parses");
        let steps = doc["steps"].as_sequence().expect("steps is a sequence");
        assert_eq!(steps[0].as_str(), Some("Click \"New\""));
        assert_eq!(steps[1].as_str(), Some("Click \"Keep\""));
    }

    #[test]
    fn apply_patch_unwraps_an_accidental_leading_list_marker() {
        // A model sometimes answers with `- Click "New"` (including the `- `
        // marker) despite being told not to; that parses as a one-element
        // sequence rather than the step itself. It must be unwrapped, not
        // nested into the flow's `steps:` list.
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Click \"Old\"\n";
        let patched = apply_patch(flow, 0, "- Click \"New\"").expect("patch applies");
        let doc: serde_yaml::Value = serde_yaml::from_str(&patched).expect("patched yaml parses");
        let steps = doc["steps"].as_sequence().expect("steps is a sequence");
        assert_eq!(steps[0].as_str(), Some("Click \"New\""));
    }

    #[test]
    fn apply_patch_out_of_range_errors() {
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Click \"Only\"\n";
        let err =
            apply_patch(flow, 5, "Click \"New\"").expect_err("out of range index is rejected");
        assert!(matches!(err, RepairError::StepNotFound(_)));
    }

    #[test]
    fn widen_timeout_replaces_only_the_wait_window() {
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Wait until page shows Home within 30s\n";
        let patched = widen_timeout(flow, 0, 60).expect("widen applies");
        let doc: serde_yaml::Value = serde_yaml::from_str(&patched).expect("patched yaml parses");
        let steps = doc["steps"].as_sequence().expect("steps is a sequence");
        assert_eq!(
            steps[0].as_str(),
            Some("Wait until page shows Home within 60s")
        );
    }

    #[test]
    fn widen_timeout_is_capped() {
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Wait until page shows Home within 30s\n";
        let patched = widen_timeout(flow, 0, 99_999).expect("widen applies");
        assert!(patched.contains("within 120s"), "got: {patched}");
    }

    #[test]
    fn widen_timeout_errors_when_step_has_no_wait_window() {
        let flow = "name: demo\napp: web\nurl: https://example.com\nsteps:\n  - Click \"Save\"\n";
        let err = widen_timeout(flow, 0, 60).expect_err("no timeout to widen");
        assert!(matches!(err, RepairError::BadModelOutput(_)));
    }

    #[test]
    fn diagnose_maps_previous_step_incomplete_to_transient_page_not_ready() {
        let err = RecordError::Agent(AgentError::PreviousStepIncomplete {
            step: "Wait until page shows Purchasing Organization within 30s".to_string(),
            evidence: "still loading".to_string(),
        });
        let ctx = diagnose(&err);
        assert_eq!(ctx.category, "page-not-ready-transient");
        assert_eq!(
            ctx.failing_intent.as_deref(),
            Some("Wait until page shows Purchasing Organization within 30s")
        );
    }

    #[test]
    fn diagnose_maps_element_not_found_to_missing_target() {
        let err = RecordError::ElementNotFound {
            intent: "Click \"Submit\"".to_string(),
            selector: "text=Submit".to_string(),
        };
        let ctx = diagnose(&err);
        assert_eq!(ctx.category, "missing-target");
        assert_eq!(ctx.failing_intent.as_deref(), Some("Click \"Submit\""));
    }
}
