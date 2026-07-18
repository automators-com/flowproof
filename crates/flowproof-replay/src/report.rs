//! Run results and the JSON artifact written for each replay.

use std::path::{Path, PathBuf};

use flowproof_trace::format::Step;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub id: String,
    pub intent: String,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub duration_ms: u64,
}

impl StepResult {
    pub fn passed(step: &Step, duration_ms: u64) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Passed,
            detail: None,
            duration_ms,
        }
    }

    pub fn failed(step: &Step, duration_ms: u64, reason: String) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Failed,
            detail: Some(reason),
            duration_ms,
        }
    }

    pub fn skipped(step: &Step) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Skipped,
            detail: Some("previous step failed".into()),
            duration_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub name: String,
    pub trace_id: String,
    pub passed: bool,
    pub steps: Vec<StepResult>,
    pub duration_ms: u64,
}

impl RunReport {
    /// Write `result.json` (plus a `report.html` rendering of it) into a
    /// fresh run directory under `<base>/.flowproof/runs/<timestamp>/` and
    /// return the JSON file path. The JSON is the primary artifact; the
    /// HTML is generated FROM it for human review.
    pub fn write(&self, base: &Path) -> std::io::Result<PathBuf> {
        let run_id = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let dir = base.join(".flowproof").join("runs").join(run_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("result.json");
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        std::fs::write(dir.join("report.html"), self.to_html())?;
        Ok(path)
    }

    /// Render a self-contained HTML report (inline CSS, no external
    /// resources) from this structured result.
    pub fn to_html(&self) -> String {
        let (verdict, color) = if self.passed {
            ("PASS", "#1a7f37")
        } else {
            ("FAIL", "#cf222e")
        };
        let mut rows = String::new();
        for step in &self.steps {
            let (badge, badge_color) = match step.status {
                StepStatus::Passed => ("PASS", "#1a7f37"),
                StepStatus::Failed => ("FAIL", "#cf222e"),
                StepStatus::Skipped => ("SKIP", "#6e7781"),
            };
            rows.push_str(&format!(
                "<tr><td><code>{}</code></td>\
                 <td><span class=\"badge\" style=\"background:{badge_color}\">{badge}</span></td>\
                 <td>{}</td><td>{}</td><td class=\"num\">{} ms</td></tr>\n",
                escape(&step.id),
                escape(&step.intent),
                step.detail.as_deref().map(escape).unwrap_or_default(),
                step.duration_ms,
            ));
        }
        format!(
            "<!doctype html>\n<html><head><meta charset=\"utf-8\">\
             <title>flowproof: {name}</title>\n<style>\
             body{{font:15px/1.5 system-ui,sans-serif;margin:2rem auto;max-width:56rem;\
             padding:0 1rem;color:#1f2328}}\
             .verdict{{display:inline-block;padding:.3rem .9rem;border-radius:.4rem;\
             color:#fff;font-weight:700;background:{color}}}\
             .badge{{display:inline-block;padding:.1rem .5rem;border-radius:.3rem;\
             color:#fff;font-size:.8rem;font-weight:600}}\
             table{{border-collapse:collapse;width:100%;margin-top:1rem}}\
             th,td{{text-align:left;padding:.45rem .6rem;border-bottom:1px solid #d1d9e0}}\
             .num{{text-align:right;white-space:nowrap}}\
             .meta{{color:#59636e;font-size:.9rem}}\
             </style></head><body>\n\
             <h1>{name}</h1>\n\
             <p><span class=\"verdict\">{verdict}</span></p>\n\
             <p class=\"meta\">trace {trace_id} &middot; {duration} ms &middot; \
             generated from result.json</p>\n\
             <table><thead><tr><th>Step</th><th>Status</th><th>Intent</th>\
             <th>Detail</th><th>Duration</th></tr></thead><tbody>\n{rows}\
             </tbody></table>\n</body></html>\n",
            name = escape(&self.name),
            trace_id = escape(&self.trace_id),
            duration = self.duration_ms,
        )
    }
}

/// Minimal HTML escaping for text content and attribute values.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn html_report_renders_and_escapes() {
        let report = RunReport {
            name: "Add <two> numbers".into(),
            trace_id: "t-1".into(),
            passed: false,
            duration_ms: 42,
            steps: vec![
                StepResult {
                    id: "s0001".into(),
                    intent: "Type 5 & smile".into(),
                    status: StepStatus::Passed,
                    detail: None,
                    duration_ms: 10,
                },
                StepResult {
                    id: "s0002".into(),
                    intent: "display shows 8".into(),
                    status: StepStatus::Failed,
                    detail: Some("expected element text '8', got '<blank>'".into()),
                    duration_ms: 5,
                },
            ],
        };
        let html = report.to_html();
        assert!(html.contains("Add &lt;two&gt; numbers"));
        assert!(html.contains("Type 5 &amp; smile"));
        assert!(html.contains("got '&lt;blank&gt;'"));
        assert!(html.contains(">FAIL<"));
        assert!(!html.contains("<two>"), "raw input must never reach HTML");
        assert!(!html.contains("http"), "report must be self-contained");
    }

    #[test]
    fn write_emits_json_and_html_side_by_side() {
        let report = RunReport {
            name: "x".into(),
            trace_id: "t".into(),
            passed: true,
            duration_ms: 1,
            steps: vec![],
        };
        let base = std::env::temp_dir().join("flowproof-report-write");
        std::fs::create_dir_all(&base).expect("temp dir");
        let json_path = report.write(&base).expect("write succeeds");
        assert!(json_path.ends_with("result.json"));
        assert!(json_path.with_file_name("report.html").exists());
        std::fs::remove_dir_all(&base).ok();
    }
}
