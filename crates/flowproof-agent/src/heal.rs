//! Healing v1: re-author the trace from the spec against the live app, diff
//! it against the recorded trace, and PROPOSE the change — never mutate.
//!
//! Today re-authoring runs the deterministic rules; the LLM authoring agent
//! slots into the same seam later. The proposed trace lands next to the
//! original as `<name>.proposed.jsonl` and is only applied on explicit
//! request.

use std::path::{Path, PathBuf};

use flowproof_driver::AppDriver;
use flowproof_trace::format::{Header, Step};
use flowproof_trace::TraceLine;
use serde::{Deserialize, Serialize};

use crate::recorder::{record, RecordError};
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
    /// Human review surface: before/after per changed step, with the frames
    /// of both executions. Rendered FROM this structured report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff_html: Option<PathBuf>,
}

fn load_trace_parts(path: &Path) -> Result<(Option<Header>, Vec<Step>), HealError> {
    let contents = std::fs::read_to_string(path).map_err(|source| HealError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut header = None;
    let mut steps = Vec::new();
    for line in contents.lines().filter(|l| !l.trim().is_empty()) {
        match TraceLine::parse(line)? {
            TraceLine::Header(h) => header = Some(h),
            TraceLine::Step(step) => steps.push(step),
        }
    }
    Ok((header, steps))
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
    let old_steps = load_steps(trace_path)?;

    let proposal = proposed_path(trace_path);
    record(spec, driver, &proposal)?;
    let new_steps = load_steps(&proposal)?;

    let (steps_changed, steps_added, steps_removed) = diff_steps(&old_steps, &new_steps);
    let changed = !steps_changed.is_empty() || steps_added > 0 || steps_removed > 0;
    if !changed {
        std::fs::remove_file(&proposal).ok();
    }
    let mut report = HealReport {
        changed,
        steps_changed,
        steps_added,
        steps_removed,
        proposed_path: changed.then_some(proposal),
        diff_html: None,
    };
    if report.changed {
        report.diff_html = write_diff_html(&report, trace_path).ok();
    }
    Ok(report)
}

/// The step time-range → frame files of one execution's recording bundle.
/// Derivable purely from the structured data plus the content-named files.
fn frames_for_range(
    trace_dir: &Path,
    header: Option<&Header>,
    range: Option<&flowproof_trace::format::StepRecording>,
) -> Vec<String> {
    let (Some(header), Some(range)) = (header, range) else {
        return Vec::new();
    };
    let Some(recording) = &header.recording else {
        return Vec::new();
    };
    let dir = trace_dir.join(&recording.dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut frames: Vec<(u64, String)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let offset: u64 = name
                .strip_prefix("frame-")?
                .split('-')
                .next()?
                .parse()
                .ok()?;
            (offset >= range.start_ms && offset <= range.end_ms)
                .then(|| (offset, format!("{}/{}", recording.dir, name)))
        })
        .collect();
    frames.sort();
    frames.into_iter().map(|(_, path)| path).collect()
}

/// Minimal HTML escaping for text content.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn pretty(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn side_html(title: &str, step: &serde_json::Value, frames: &[String]) -> String {
    let imgs: String = frames
        .iter()
        .map(|f| {
            format!(
                "<a href=\"{f}\"><img src=\"{f}\" loading=\"lazy\" alt=\"frame\"></a>",
                f = escape(f)
            )
        })
        .collect();
    let frames_note = if frames.is_empty() {
        "<p class=\"meta\">no frames captured</p>".to_string()
    } else {
        format!("<div class=\"frames\">{imgs}</div>")
    };
    format!(
        "<div class=\"side\"><h4>{title}</h4>{frames_note}\
         <pre>{json}</pre></div>",
        title = escape(title),
        json = escape(&pretty(step)),
    )
}

/// Write the before/after review page next to the trace as
/// `<name>.heal.html`. Frames come from each execution's own recording
/// bundle (the original trace's and the proposal's), referenced relatively.
pub fn write_diff_html(report: &HealReport, trace_path: &Path) -> std::io::Result<PathBuf> {
    let trace_dir = trace_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let old_header = load_trace_parts(trace_path).ok().and_then(|(h, _)| h);
    let new_header = report
        .proposed_path
        .as_ref()
        .and_then(|p| load_trace_parts(p).ok())
        .and_then(|(h, _)| h);

    let mut sections = String::new();
    for change in &report.steps_changed {
        let old_range = serde_json::from_value::<Step>(change.old.clone())
            .ok()
            .and_then(|s| s.artifacts.recording);
        let new_range = serde_json::from_value::<Step>(change.new.clone())
            .ok()
            .and_then(|s| s.artifacts.recording);
        let old_frames = frames_for_range(&trace_dir, old_header.as_ref(), old_range.as_ref());
        let new_frames = frames_for_range(&trace_dir, new_header.as_ref(), new_range.as_ref());
        sections.push_str(&format!(
            "<section><h3><code>{id}</code> {intent}</h3>\
             <p class=\"meta\">changed: {fields}</p>\
             <div class=\"pair\">{before}{after}</div></section>\n",
            id = escape(&change.id),
            intent = escape(&change.intent),
            fields = escape(&change.fields.join(", ")),
            before = side_html("Before (recorded)", &change.old, &old_frames),
            after = side_html("After (proposed)", &change.new, &new_frames),
        ));
    }
    if report.steps_added > 0 || report.steps_removed > 0 {
        sections.push_str(&format!(
            "<p class=\"meta\">steps added: {}, removed: {}</p>",
            report.steps_added, report.steps_removed
        ));
    }

    let html = format!(
        "<!doctype html>\n<html><head><meta charset=\"utf-8\">\
         <title>flowproof heal review</title>\n<style>\
         body{{font:15px/1.5 system-ui,sans-serif;margin:2rem auto;max-width:72rem;\
         padding:0 1rem;color:#1f2328}}\
         .meta{{color:#59636e;font-size:.9rem}}\
         .pair{{display:flex;gap:1rem;align-items:flex-start}}\
         .side{{flex:1;min-width:0;border:1px solid #d1d9e0;border-radius:.4rem;padding:.6rem}}\
         .side h4{{margin:.1rem 0 .4rem}}\
         pre{{overflow-x:auto;background:#f6f8fa;padding:.5rem;border-radius:.3rem;\
         font-size:.8rem}}\
         .frames img{{max-width:100%;border:1px solid #d1d9e0;margin-bottom:.4rem}}\
         section{{margin-bottom:1.5rem}}\
         </style></head><body>\n<h1>Heal review</h1>\n\
         <p class=\"meta\">proposed changes to {trace}; apply with \
         <code>flowproof heal --apply</code>. Generated from the structured \
         heal report.</p>\n{sections}</body></html>\n",
        trace = escape(&trace_path.display().to_string()),
        sections = sections,
    );

    let out = {
        let stem = trace_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let base = stem.strip_suffix(".trace.jsonl").unwrap_or(&stem);
        trace_path.with_file_name(format!("{base}.heal.html"))
    };
    std::fs::write(&out, html)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use flowproof_driver::mock::MockAppDriver;

    use super::*;

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
        assert!(
            report.diff_html.is_none(),
            "no review page for healthy trace"
        );
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

        // The review page is written next to the trace, rendered from the
        // structured report: one before/after pair for the changed step.
        let page = report.diff_html.expect("review page written");
        assert_eq!(page, dir.join("calc.heal.html"));
        let html = std::fs::read_to_string(&page).expect("review page readable");
        assert!(html.contains("Before (recorded)"));
        assert!(html.contains("After (proposed)"));
        assert!(
            html.contains("oldPlusButton"),
            "shows the recorded selector"
        );
        assert!(html.contains("plusButton"), "shows the proposed selector");
        assert!(html.contains("Press plus"), "labels the step by intent");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn review_page_escapes_untrusted_trace_content() {
        let dir = std::env::temp_dir().join("flowproof-heal-escape");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let trace = dir.join("calc.trace.jsonl");
        record(&spec, &mut calc_mock(), &trace).expect("recording succeeds");

        // Selector payloads come from the app under test — hostile markup in
        // them must not become live HTML in the review page.
        let contents = std::fs::read_to_string(&trace).expect("trace readable");
        std::fs::write(
            &trace,
            contents.replace("plusButton", "<script>alert(1)</script>"),
        )
        .expect("trace rewritten");

        let report = heal(&spec, &mut calc_mock(), &trace).expect("heal runs");
        let page = report.diff_html.expect("review page written");
        let html = std::fs::read_to_string(&page).expect("review page readable");
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
