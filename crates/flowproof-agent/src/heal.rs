//! Healing v1: re-author the trace from the spec against the live app, diff
//! it against the recorded trace, and PROPOSE the change — never mutate.
//!
//! Today re-authoring runs the deterministic rules; the LLM authoring agent
//! slots into the same seam later. The proposed trace lands next to the
//! original as `<name>.proposed.jsonl` and is only applied on explicit
//! request.

use std::path::{Path, PathBuf};

use flowproof_driver::AppDriver;
use flowproof_trace::format::Step;
use flowproof_trace::TraceLine;
use serde::{Deserialize, Serialize};

use crate::recorder::{record_with_author, Author, RecordError};
use crate::spec::FlowSpec;

#[derive(Debug, thiserror::Error)]
pub enum HealError {
    #[error("cannot re-record flow: {0}")]
    Record(#[from] RecordError),
    #[error("cannot read trace {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid trace: {0}")]
    Trace(#[from] flowproof_trace::TraceError),
}

/// One step whose recorded form no longer matches what re-authoring
/// produces against the live app.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepChange {
    pub id: String,
    pub intent: String,
    /// Which parts differ: `action`, `selectors`, `intent` (subset).
    pub fields: Vec<String>,
    pub old: serde_json::Value,
    pub new: serde_json::Value,
}

/// Outcome of a heal pass. `changed == false` means the trace is already
/// healthy — the proposed file is only written when there is a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealReport {
    pub changed: bool,
    pub steps_changed: Vec<StepChange>,
    pub steps_added: usize,
    pub steps_removed: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_path: Option<PathBuf>,
}

fn load_steps(path: &Path) -> Result<Vec<Step>, HealError> {
    let contents = std::fs::read_to_string(path).map_err(|source| HealError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut steps = Vec::new();
    for line in contents.lines().filter(|l| !l.trim().is_empty()) {
        if let TraceLine::Step(step) = TraceLine::parse(line)? {
            steps.push(step);
        }
    }
    Ok(steps)
}

fn diff_steps(old: &[Step], new: &[Step]) -> (Vec<StepChange>, usize, usize) {
    let mut changes = Vec::new();
    for (old_step, new_step) in old.iter().zip(new.iter()) {
        let mut fields = Vec::new();
        if old_step.intent != new_step.intent {
            fields.push("intent".to_string());
        }
        if old_step.action != new_step.action {
            fields.push("action".to_string());
        }
        if old_step.selectors != new_step.selectors {
            fields.push("selectors".to_string());
        }
        if !fields.is_empty() {
            changes.push(StepChange {
                id: old_step.id.clone(),
                intent: old_step.intent.clone(),
                fields,
                old: serde_json::to_value(old_step).unwrap_or_default(),
                new: serde_json::to_value(new_step).unwrap_or_default(),
            });
        }
    }
    let added = new.len().saturating_sub(old.len());
    let removed = old.len().saturating_sub(new.len());
    (changes, added, removed)
}

/// Default proposed-trace path: `calc.trace.jsonl` → `calc.proposed.jsonl`.
pub fn proposed_path(trace: &Path) -> PathBuf {
    let stem = trace
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = stem.strip_suffix(".trace.jsonl").unwrap_or(&stem);
    trace.with_file_name(format!("{base}.proposed.jsonl"))
}

/// Re-author `spec` against the live app, diff against the trace at
/// `trace_path`, and write a proposed trace if anything changed. The
/// original trace is never modified; apply by copying the proposal over it
/// (the CLI's `--apply` does exactly that, explicitly).
pub fn heal<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    trace_path: &Path,
) -> Result<HealReport, HealError> {
    heal_with_author(spec, driver, trace_path, Author::Auto)
}

/// [`heal`] with an explicit authoring mode (the CLI's `--author`).
pub fn heal_with_author<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    trace_path: &Path,
    author: Author,
) -> Result<HealReport, HealError> {
    let old_steps = load_steps(trace_path)?;

    let proposal = proposed_path(trace_path);
    record_with_author(spec, driver, &proposal, author)?;
    let new_steps = load_steps(&proposal)?;

    let (steps_changed, steps_added, steps_removed) = diff_steps(&old_steps, &new_steps);
    let changed = !steps_changed.is_empty() || steps_added > 0 || steps_removed > 0;
    if !changed {
        std::fs::remove_file(&proposal).ok();
    }
    Ok(HealReport {
        changed,
        steps_changed,
        steps_added,
        steps_removed,
        proposed_path: changed.then_some(proposal),
    })
}

#[cfg(test)]
mod tests {
    use flowproof_driver::mock::MockAppDriver;

    use super::*;
    use crate::recorder::record;

    const CALC_SPEC: &str = "\
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
";

    const CALC_ELEMENTS: [&str; 5] = [
        "num5Button",
        "num3Button",
        "plusButton",
        "equalButton",
        "CalculatorResults",
    ];

    fn calc_mock() -> MockAppDriver {
        MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8")
    }

    #[test]
    fn healthy_trace_needs_no_healing() {
        let dir = std::env::temp_dir().join("flowproof-heal-healthy");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let trace = dir.join("calc.trace.jsonl");
        record(&spec, &mut calc_mock(), &trace).expect("recording succeeds");

        let report = heal(&spec, &mut calc_mock(), &trace).expect("heal runs");
        assert!(!report.changed, "report: {report:?}");
        assert!(report.proposed_path.is_none());
        assert!(!proposed_path(&trace).exists(), "no proposal left behind");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn outdated_selector_produces_a_reviewable_proposal() {
        let dir = std::env::temp_dir().join("flowproof-heal-outdated");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let trace = dir.join("calc.trace.jsonl");
        record(&spec, &mut calc_mock(), &trace).expect("recording succeeds");

        // Simulate an outdated trace: the plus button was recorded under an
        // old automation id that no longer exists.
        let contents = std::fs::read_to_string(&trace).expect("trace readable");
        std::fs::write(&trace, contents.replace("plusButton", "oldPlusButton"))
            .expect("trace rewritten");

        let report = heal(&spec, &mut calc_mock(), &trace).expect("heal runs");
        assert!(report.changed);
        assert_eq!(report.steps_changed.len(), 1);
        let change = &report.steps_changed[0];
        assert_eq!(change.intent, "Press plus");
        assert_eq!(change.fields, vec!["selectors".to_string()]);
        assert!(change.old.to_string().contains("oldPlusButton"));
        assert!(change.new.to_string().contains("plusButton"));

        // The original trace is untouched; the proposal sits beside it.
        assert!(std::fs::read_to_string(&trace)
            .expect("trace readable")
            .contains("oldPlusButton"));
        let proposal = report.proposed_path.expect("proposal written");
        assert!(std::fs::read_to_string(proposal)
            .expect("proposal readable")
            .contains("\"plusButton\""));
        std::fs::remove_dir_all(&dir).ok();
    }
}
