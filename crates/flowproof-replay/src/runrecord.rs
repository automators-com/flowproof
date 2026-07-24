//! The persisted run record: one structured artifact per `flowproof run`,
//! read back by `flowproof audit` and diffed across runs by `--since`.
//!
//! The record is a pure OUTPUT. It never travels in a trace, it is never an
//! input to replay, and it never affects replay determinism: a flow that uses
//! no controls runs and records byte-identical whether or not this file is
//! written. Secrets never enter it either - it carries control ids, verdicts,
//! variable NAMES, and corpus/exclusion descriptors, never a resolved value.
//!
//! Layout, under the suite (or single spec's) directory:
//!
//! ```text
//! .flowproof/
//!   runs/<run-id>/report.json   # this record
//!   runs/<run-id>/junit.xml     # the existing merged junit, beside it
//!   artifacts/<sha256>          # existing content-addressed blobs
//! ```

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::report::RunReport;

/// The relative location of the run records under a suite directory.
pub const RUNS_SUBDIR: &str = "runs";
/// The record file inside a run directory. Its presence is what marks a
/// directory as a suite run record, distinct from the per-flow `result.json`
/// bundles replay writes beside a trace - retention only ever prunes the
/// former.
pub const RECORD_FILE: &str = "report.json";

/// One control's verdict. Three states kept distinct so a record can never
/// launder "we could not check" into "it held".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlVerdict {
    Pass,
    Fail,
    /// The platform could not enforce or observe the lane, or the flow never
    /// ran (a missing trace, a skipped or errored flow).
    CapabilityError,
}

impl ControlVerdict {
    /// Read a control's verdict from the flow's replay report, reusing the
    /// existing per-flow verdict logic rather than re-deriving it. A skipped
    /// or errored flow never ran, so its control is a capability error, never
    /// a silent pass; a failure whose reason names an unenforceable or
    /// unobservable lane is a capability error too. Returns the verdict and
    /// the first failing step's detail (value-free by construction upstream).
    pub fn from_run_report(report: &RunReport) -> (Self, Option<String>) {
        let reason = report.steps.iter().find_map(|s| s.detail.clone());
        // `skipped`/`errored` are the synthetic reports for a flow that never
        // produced a real verdict (no trace, driver fault, failing hook).
        if report.trace_id == "skipped" || report.trace_id == "errored" {
            return (Self::CapabilityError, reason);
        }
        if report.passed {
            return (Self::Pass, None);
        }
        let verdict = if reason.as_deref().is_some_and(is_capability_error) {
            Self::CapabilityError
        } else {
            Self::Fail
        };
        (verdict, reason)
    }

    /// Read a control's verdict from an agent flow's replay outcome, which is
    /// a bare `Result` rather than a step report. Same three-state mapping.
    pub fn from_outcome(outcome: &Result<(), String>) -> (Self, Option<String>) {
        match outcome {
            Ok(()) => (Self::Pass, None),
            Err(e) if is_capability_error(e) => (Self::CapabilityError, Some(e.clone())),
            Err(e) => (Self::Fail, Some(e.clone())),
        }
    }
}

/// Whether a replay failure is really a capability error (the lane could not
/// be enforced or observed) rather than a control that failed. Mirrors the
/// egress honesty wording so a "not contained" run reads as capability-error,
/// and a missing trace (surfaced as "no trace recorded") likewise.
pub fn is_capability_error(message: &str) -> bool {
    message.contains("not contained")
        || message.contains("cannot certify")
        || message.contains("not enforced")
        || message.contains("no trace recorded")
}

/// A flow's top-level status in the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowStatus {
    Pass,
    Fail,
    Degraded,
}

impl FlowStatus {
    /// Fold a replay report into a flow status: a failure is `fail`, a pass
    /// that needed a fallback selector is `degraded`, else `pass`.
    pub fn from_run_report(report: &RunReport) -> Self {
        if !report.passed {
            Self::Fail
        } else if report.degraded {
            Self::Degraded
        } else {
            Self::Pass
        }
    }
}

/// Where a control's proof lives: the trace (a path relative to the suite, or
/// a content-addressed `artifacts/sha256:…` pointer) and any egress
/// destinations containment blocked during the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    pub trace: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked: Vec<String>,
}

/// One control's row in the record. Present only for a flow that carries a
/// `control:` block.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRecord {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub verdict: ControlVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Which control lanes the flow asserted (`egress`, `secret_leak`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lanes: Vec<String>,
    pub evidence: Evidence,
    /// The `${VAR}` names an `assert_no_secret_leak` flow checked - NAMES,
    /// never values.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets_checked: Vec<String>,
    /// What the secret scan actually covered, so nobody mistakes it for a
    /// proof about channels the engine never saw.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub corpus: Vec<String>,
    /// The corpus exclusions, echoed so the record is honest about its gaps.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<String>,
}

/// One flow's row in the record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowRecord {
    pub flow: String,
    pub status: FlowStatus,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<ControlRecord>,
}

/// The run's environment stamp - just the OS today.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunEnv {
    pub os: String,
}

impl RunEnv {
    /// The current host's environment stamp.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
        }
    }
}

/// The structured record of one `flowproof run`: every flow it ran, folded
/// with each flow's `control:` verdict. The stable artifact `audit` reads and
/// `--since` diffs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub started_at: String,
    pub flowproof_version: String,
    pub env: RunEnv,
    pub flows: Vec<FlowRecord>,
}

impl RunRecord {
    /// Write this record to `<suite_dir>/.flowproof/runs/<run_id>/report.json`,
    /// then prune the run history to the most recent `keep`. Returns the path
    /// written and the run-ids pruned (so the caller can log them). The run-id
    /// must already be minted into `self.run_id`.
    pub fn write(&self, suite_dir: &Path, keep: usize) -> std::io::Result<(PathBuf, Vec<String>)> {
        let run_dir = runs_dir(suite_dir).join(&self.run_id);
        std::fs::create_dir_all(&run_dir)?;
        let path = run_dir.join(RECORD_FILE);
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        let pruned = prune_runs(suite_dir, keep)?;
        Ok((path, pruned))
    }

    /// Parse a record from a `report.json` path.
    pub fn load_file(path: &Path) -> std::io::Result<Self> {
        let raw = std::fs::read_to_string(path)?;
        serde_json::from_str(&raw)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// The most recent record under a suite directory, or `None` when no run
    /// has been recorded there yet.
    pub fn latest(suite_dir: &Path) -> std::io::Result<Option<Self>> {
        match record_run_ids(suite_dir)?.pop() {
            Some(id) => Self::load(suite_dir, &id).map(Some),
            None => Ok(None),
        }
    }

    /// A specific record by run-id.
    pub fn load(suite_dir: &Path, run_id: &str) -> std::io::Result<Self> {
        Self::load_file(&runs_dir(suite_dir).join(run_id).join(RECORD_FILE))
    }

    /// Every `control:`-bearing flow's control row, in flow order.
    pub fn controls(&self) -> impl Iterator<Item = (&FlowRecord, &ControlRecord)> {
        self.flows
            .iter()
            .filter_map(|f| f.control.as_ref().map(|c| (f, c)))
    }
}

/// `<suite_dir>/.flowproof/runs`.
fn runs_dir(suite_dir: &Path) -> PathBuf {
    suite_dir.join(".flowproof").join(RUNS_SUBDIR)
}

/// The run-ids of the suite records under `suite_dir`, ascending. A directory
/// counts only when it holds a `report.json`, so the per-flow `result.json`
/// bundles replay writes beside a trace are never mistaken for run records.
/// Run-ids sort chronologically because they lead with a fixed-width RFC3339
/// stamp, so a plain string sort is a time sort.
pub fn record_run_ids(suite_dir: &Path) -> std::io::Result<Vec<String>> {
    let dir = runs_dir(suite_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        // No runs directory yet is not an error: it means nothing has run.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut ids: Vec<String> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join(RECORD_FILE).is_file())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    ids.sort();
    Ok(ids)
}

/// Prune the suite's run records to the most recent `keep`, by run-id order
/// (which is chronological). Returns the run-ids removed. Only directories
/// holding a `report.json` are considered, so per-flow bundles are untouched.
pub fn prune_runs(suite_dir: &Path, keep: usize) -> std::io::Result<Vec<String>> {
    let ids = record_run_ids(suite_dir)?;
    if ids.len() <= keep {
        return Ok(Vec::new());
    }
    let cutoff = ids.len() - keep;
    let mut pruned = Vec::new();
    for id in &ids[..cutoff] {
        std::fs::remove_dir_all(runs_dir(suite_dir).join(id))?;
        pruned.push(id.clone());
    }
    Ok(pruned)
}

/// One control's identity and verdict, for the added/removed sides of a diff.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlRef {
    pub id: String,
    pub verdict: ControlVerdict,
}

/// A control whose verdict changed between two runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlChange {
    pub id: String,
    pub old: ControlVerdict,
    pub new: ControlVerdict,
}

/// The diff between two run records, folded by `control.id`: controls added in
/// the newer run, controls removed (present in the older, gone in the newer -
/// coverage that shrank), and controls whose verdict changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunDiff {
    /// The older run-id (`--since`).
    pub base: String,
    /// The newer run-id (the latest).
    pub head: String,
    pub added: Vec<ControlRef>,
    pub removed: Vec<ControlRef>,
    pub changed: Vec<ControlChange>,
}

impl RunDiff {
    /// Diff `base` (older) against `head` (newer) by control id. Added =
    /// present in head, absent in base. Removed = present in base, absent in
    /// head. Changed = present in both with a different verdict. All three
    /// lists are sorted by id for a stable, reviewable output.
    pub fn between(base: &RunRecord, head: &RunRecord) -> Self {
        let base_map = control_map(base);
        let head_map = control_map(head);

        let mut added: Vec<ControlRef> = head_map
            .iter()
            .filter(|(id, _)| !base_map.contains_key(id.as_str()))
            .map(|(id, v)| ControlRef {
                id: id.clone(),
                verdict: *v,
            })
            .collect();
        let mut removed: Vec<ControlRef> = base_map
            .iter()
            .filter(|(id, _)| !head_map.contains_key(id.as_str()))
            .map(|(id, v)| ControlRef {
                id: id.clone(),
                verdict: *v,
            })
            .collect();
        let mut changed: Vec<ControlChange> = base_map
            .iter()
            .filter_map(|(id, old)| {
                head_map
                    .get(id)
                    .filter(|new| *new != old)
                    .map(|new| ControlChange {
                        id: id.clone(),
                        old: *old,
                        new: *new,
                    })
            })
            .collect();
        added.sort_by(|a, b| a.id.cmp(&b.id));
        removed.sort_by(|a, b| a.id.cmp(&b.id));
        changed.sort_by(|a, b| a.id.cmp(&b.id));

        Self {
            base: base.run_id.clone(),
            head: head.run_id.clone(),
            added,
            removed,
            changed,
        }
    }

    /// Whether the diff shows a regression CI should fail on: a control was
    /// removed (coverage shrank) or a control's verdict changed to `fail`.
    pub fn is_regression(&self) -> bool {
        !self.removed.is_empty() || self.changed.iter().any(|c| c.new == ControlVerdict::Fail)
    }

    /// Whether there is any difference at all.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }
}

/// id -> verdict for every control-bearing flow in a record. Control ids are
/// unique per suite (enforced at load), so a map is faithful.
fn control_map(record: &RunRecord) -> std::collections::BTreeMap<String, ControlVerdict> {
    record
        .controls()
        .map(|(_, c)| (c.id.clone(), c.verdict))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{StepResult, StepStatus};

    fn passing_report(name: &str) -> RunReport {
        RunReport {
            name: name.into(),
            trace_id: "t-1".into(),
            passed: true,
            degraded: false,
            steps: vec![StepResult {
                id: "s0001".into(),
                intent: "do a thing".into(),
                status: StepStatus::Passed,
                detail: None,
                started_ms: 0,
                duration_ms: 1,
                selector_tier: None,
                degraded: false,
            }],
            duration_ms: 1,
            recording: None,
        }
    }

    fn control(id: &str, verdict: ControlVerdict) -> FlowRecord {
        FlowRecord {
            flow: format!("flows/{id}.flow.yaml"),
            status: FlowStatus::Pass,
            degraded: false,
            control: Some(ControlRecord {
                id: id.into(),
                title: None,
                verdict,
                reason: None,
                lanes: vec![],
                evidence: Evidence {
                    trace: format!("flows/{id}.trace.jsonl"),
                    blocked: vec![],
                },
                secrets_checked: vec![],
                corpus: vec![],
                excluded: vec![],
            }),
        }
    }

    fn record(run_id: &str, flows: Vec<FlowRecord>) -> RunRecord {
        RunRecord {
            run_id: run_id.into(),
            started_at: "2026-07-24T11:06:18Z".into(),
            flowproof_version: "0.4.1".into(),
            env: RunEnv { os: "linux".into() },
            flows,
        }
    }

    #[test]
    fn verdict_from_report_maps_the_three_states() {
        let (v, _) = ControlVerdict::from_run_report(&passing_report("ok"));
        assert_eq!(v, ControlVerdict::Pass);

        let mut failed = passing_report("bad");
        failed.passed = false;
        failed.steps[0].status = StepStatus::Failed;
        failed.steps[0].detail = Some("expected '8', got '<blank>'".into());
        let (v, reason) = ControlVerdict::from_run_report(&failed);
        assert_eq!(v, ControlVerdict::Fail);
        assert_eq!(reason.as_deref(), Some("expected '8', got '<blank>'"));

        // A "not contained" failure is a capability error, not a fail.
        let mut capp = failed.clone();
        capp.steps[0].detail = Some("agent egress was not contained on this host".into());
        let (v, _) = ControlVerdict::from_run_report(&capp);
        assert_eq!(v, ControlVerdict::CapabilityError);

        // A skipped flow never ran: capability error, never a silent pass,
        // even though a skipped report carries `passed: true`.
        let skipped = RunReport::skipped("gated", "no trace recorded - flowproof record x");
        let (v, _) = ControlVerdict::from_run_report(&skipped);
        assert_eq!(v, ControlVerdict::CapabilityError);

        // An errored flow likewise.
        let errored = RunReport::errored("broke", "driver transport fault");
        let (v, _) = ControlVerdict::from_run_report(&errored);
        assert_eq!(v, ControlVerdict::CapabilityError);
    }

    #[test]
    fn verdict_from_outcome_maps_agent_results() {
        assert_eq!(
            ControlVerdict::from_outcome(&Ok(())).0,
            ControlVerdict::Pass
        );
        assert_eq!(
            ControlVerdict::from_outcome(&Err("undeclared egress attempted: …".into())).0,
            ControlVerdict::Fail
        );
        assert_eq!(
            ControlVerdict::from_outcome(&Err("egress not contained on this host".into())).0,
            ControlVerdict::CapabilityError
        );
    }

    #[test]
    fn diff_detects_added_removed_and_verdict_changed() {
        let base = record(
            "2026-07-24T11-00-00Z-aaaa",
            vec![
                control("keep.same", ControlVerdict::Pass),
                control("regressed", ControlVerdict::Pass),
                control("dropped", ControlVerdict::Pass),
            ],
        );
        let head = record(
            "2026-07-24T12-00-00Z-bbbb",
            vec![
                control("keep.same", ControlVerdict::Pass),
                control("regressed", ControlVerdict::Fail),
                control("brand.new", ControlVerdict::Pass),
            ],
        );
        let diff = RunDiff::between(&base, &head);

        assert_eq!(diff.base, "2026-07-24T11-00-00Z-aaaa");
        assert_eq!(diff.head, "2026-07-24T12-00-00Z-bbbb");
        // Added: only the control that appears in head and not base.
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].id, "brand.new");
        // Removed: id present in the older record, absent in the newer.
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].id, "dropped");
        // Changed: same id, different verdict, old -> new preserved.
        assert_eq!(diff.changed.len(), 1);
        assert_eq!(diff.changed[0].id, "regressed");
        assert_eq!(diff.changed[0].old, ControlVerdict::Pass);
        assert_eq!(diff.changed[0].new, ControlVerdict::Fail);
        // A removed control and a fail regression are both CI-failing.
        assert!(diff.is_regression());
    }

    #[test]
    fn diff_of_identical_records_is_empty() {
        let flows = vec![control("a", ControlVerdict::Pass)];
        let base = record("2026-07-24T11-00-00Z-aaaa", flows.clone());
        let head = record("2026-07-24T12-00-00Z-bbbb", flows);
        let diff = RunDiff::between(&base, &head);
        assert!(diff.is_empty());
        assert!(!diff.is_regression());
    }

    #[test]
    fn removed_control_is_detected_even_when_nothing_else_changes() {
        let base = record(
            "2026-07-24T11-00-00Z-aaaa",
            vec![
                control("a", ControlVerdict::Pass),
                control("b", ControlVerdict::Pass),
            ],
        );
        let head = record(
            "2026-07-24T12-00-00Z-bbbb",
            vec![control("a", ControlVerdict::Pass)],
        );
        let diff = RunDiff::between(&base, &head);
        assert!(diff.added.is_empty());
        assert!(diff.changed.is_empty());
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].id, "b");
        assert!(diff.is_regression(), "a removed control shrinks coverage");
    }

    #[test]
    fn write_read_roundtrips_and_retention_prunes_to_ten() {
        let base = std::env::temp_dir().join(format!("flowproof-runrec-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        std::fs::create_dir_all(&base).expect("temp dir");

        // Write 13 records with lexicographically ordered ids.
        for i in 0..13 {
            let rec = record(
                &format!("2026-07-24T11-00-{i:02}Z-{i:04x}"),
                vec![control("a", ControlVerdict::Pass)],
            );
            rec.write(&base, 10).expect("write");
        }
        // Retention kept exactly the most recent 10.
        let ids = record_run_ids(&base).expect("list");
        assert_eq!(ids.len(), 10, "retention prunes to 10: {ids:?}");
        // The three oldest were the ones pruned.
        assert!(!ids.iter().any(|id| id.contains("11-00-00")));
        assert!(!ids.iter().any(|id| id.contains("11-00-02")));
        assert!(ids.iter().any(|id| id.contains("11-00-12")));

        // The latest reads back byte-faithful.
        let latest = RunRecord::latest(&base).expect("io").expect("some");
        assert_eq!(latest.run_id, "2026-07-24T11-00-12Z-000c");
        assert_eq!(latest.flows.len(), 1);
        assert_eq!(latest.flows[0].control.as_ref().expect("control").id, "a");

        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn latest_is_none_when_no_run_recorded() {
        let base =
            std::env::temp_dir().join(format!("flowproof-runrec-empty-{}", std::process::id()));
        std::fs::remove_dir_all(&base).ok();
        assert!(RunRecord::latest(&base).expect("io").is_none());
    }

    /// The record must never carry a resolved secret VALUE - only names,
    /// verdicts, and descriptors. Build a fully-populated control record whose
    /// secret is named `${DB_PASSWORD}`, serialize it, and assert the actual
    /// value never appears anywhere in the JSON.
    #[test]
    fn a_record_never_carries_a_secret_value() {
        const SECRET_VALUE: &str = "hunter2-super-secret-db-password";
        let rec = RunRecord {
            run_id: "2026-07-24T11-06-18Z-a1b2".into(),
            started_at: "2026-07-24T11:06:18Z".into(),
            flowproof_version: "0.4.1".into(),
            env: RunEnv { os: "linux".into() },
            flows: vec![FlowRecord {
                flow: "flows/no-leak.flow.yaml".into(),
                status: FlowStatus::Pass,
                degraded: false,
                control: Some(ControlRecord {
                    id: "sec.portal.no-db-password-leak".into(),
                    title: Some("The DB password never surfaces".into()),
                    verdict: ControlVerdict::Pass,
                    reason: None,
                    lanes: vec!["secret_leak".into()],
                    evidence: Evidence {
                        trace: "flows/no-leak.trace.jsonl".into(),
                        blocked: vec![],
                    },
                    // The NAME travels, never the value.
                    secrets_checked: vec!["${DB_PASSWORD}".into()],
                    corpus: vec!["model-boundary trajectory".into()],
                    excluded: vec!["server logs".into()],
                }),
            }],
        };
        let json = serde_json::to_string(&rec).expect("serializes");
        assert!(
            json.contains("${DB_PASSWORD}"),
            "the variable name is recorded"
        );
        assert!(
            !json.contains(SECRET_VALUE),
            "a resolved secret value must never enter the record"
        );
    }
}
