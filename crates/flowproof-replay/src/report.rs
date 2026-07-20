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
    /// Offset from run start — with `duration_ms` this is the step→time
    /// mapping into the run's recording.
    #[serde(default)]
    pub started_ms: u64,
    pub duration_ms: u64,
    /// Which selector-ladder tier resolved this step's target
    /// (`native_id`, `structural`, `text_anchor`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector_tier: Option<String>,
    /// True when the target matched on a fallback rung instead of the
    /// recorded primary selector — the step still ran, but the app has
    /// drifted and the trace should be healed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
}

impl StepResult {
    pub fn passed(step: &Step, started_ms: u64, duration_ms: u64) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Passed,
            detail: None,
            started_ms,
            duration_ms,
            selector_tier: None,
            degraded: false,
        }
    }

    pub fn failed(step: &Step, started_ms: u64, duration_ms: u64, reason: String) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Failed,
            detail: Some(reason),
            started_ms,
            duration_ms,
            selector_tier: None,
            degraded: false,
        }
    }

    pub fn skipped(step: &Step) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Skipped,
            detail: Some("previous step failed".into()),
            started_ms: 0,
            duration_ms: 0,
            selector_tier: None,
            degraded: false,
        }
    }

    /// A skipped step with an explicit reason — used for flow-level skips
    /// (no trace recorded, skip conditions), where nothing ran at all.
    pub fn skipped_with_reason(id: &str, intent: &str, reason: &str) -> Self {
        Self {
            id: id.to_string(),
            intent: intent.to_string(),
            status: StepStatus::Skipped,
            detail: Some(reason.to_string()),
            started_ms: 0,
            duration_ms: 0,
            selector_tier: None,
            degraded: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub name: String,
    pub trace_id: String,
    pub passed: bool,
    /// True when any step resolved via a fallback selector rung: the run
    /// passed, but the app drifted from the trace — schedule a heal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub degraded: bool,
    pub steps: Vec<StepResult>,
    pub duration_ms: u64,
    /// The run's recording bundle: format, frame refs, and per-step time
    /// ranges — the complete step→time mapping, embedded (no sidecar).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<flowproof_driver::Recording>,
}

impl RunReport {
    /// A synthetic report for a flow that never ran (no trace recorded,
    /// skip condition). `passed: true` — a skip is not a failure, matching
    /// JUnit semantics — with one skipped step carrying the reason, so the
    /// merged suite junit counts it visibly instead of losing it.
    pub fn skipped(name: &str, reason: &str) -> Self {
        Self {
            name: name.to_string(),
            trace_id: "skipped".into(),
            passed: true,
            degraded: false,
            steps: vec![StepResult::skipped_with_reason(
                "s0001",
                "flow skipped",
                reason,
            )],
            duration_ms: 0,
            recording: None,
        }
    }

    /// Write `result.json` (plus a `report.html` rendering and a
    /// `junit.xml` for CI systems) into the run directory `run_trace`
    /// created — the same bundle that holds the recording. Returns the JSON
    /// file path. The JSON is the primary artifact; the HTML and JUnit
    /// files are generated FROM it for human review and CI ingestion.
    pub fn write_into(&self, run_dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(run_dir)?;
        let path = run_dir.join("result.json");
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        std::fs::write(run_dir.join("report.html"), self.to_html())?;
        std::fs::write(run_dir.join("junit.xml"), self.to_junit_xml())?;
        Ok(path)
    }

    /// Render the run as JUnit XML — the lingua franca of CI test
    /// reporting (Jenkins, GitLab, Azure DevOps, Buildkite all ingest it),
    /// so flowproof slots into an existing test stack without a plugin.
    /// One `<testsuite>` per run, one `<testcase>` per step.
    pub fn to_junit_xml(&self) -> String {
        Self::suite_junit_xml(std::iter::once(self))
    }

    /// Merge several runs into ONE JUnit document — the artifact a suite
    /// run (`flowproof run <dir>`) hands to CI: one `<testsuite>` per flow.
    pub fn suite_junit_xml<'a>(reports: impl IntoIterator<Item = &'a RunReport>) -> String {
        let mut suites = String::new();
        let (mut tests, mut failures, mut skipped, mut time) = (0usize, 0usize, 0usize, 0f64);
        for report in reports {
            tests += report.steps.len();
            failures += report
                .steps
                .iter()
                .filter(|s| s.status == StepStatus::Failed)
                .count();
            skipped += report
                .steps
                .iter()
                .filter(|s| s.status == StepStatus::Skipped)
                .count();
            time += report.duration_ms as f64 / 1000.0;
            suites.push_str(&report.junit_testsuite());
        }
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <testsuites name=\"flowproof\" tests=\"{tests}\" failures=\"{failures}\" \
             skipped=\"{skipped}\" time=\"{time:.3}\">\n\
             {suites}</testsuites>\n",
        )
    }

    /// One `<testsuite>` fragment for this run.
    fn junit_testsuite(&self) -> String {
        let failures = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Failed)
            .count();
        let skipped = self
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Skipped)
            .count();
        let time = self.duration_ms as f64 / 1000.0;
        let mut cases = String::new();
        for step in &self.steps {
            let case_open = format!(
                "    <testcase classname=\"{}\" name=\"{} {}\" time=\"{:.3}\"",
                xml_escape(&self.name),
                xml_escape(&step.id),
                xml_escape(&step.intent),
                step.duration_ms as f64 / 1000.0,
            );
            match step.status {
                StepStatus::Passed => cases.push_str(&format!("{case_open}/>\n")),
                StepStatus::Failed => cases.push_str(&format!(
                    "{case_open}>\n      <failure message=\"{}\"/>\n    </testcase>\n",
                    xml_escape(step.detail.as_deref().unwrap_or("step failed")),
                )),
                // The reason travels into junit: gated/traceless coverage
                // must be measurable, not invisible.
                StepStatus::Skipped => match step.detail.as_deref() {
                    Some(reason) => cases.push_str(&format!(
                        "{case_open}>\n      <skipped message=\"{}\"/>\n    </testcase>\n",
                        xml_escape(reason),
                    )),
                    None => cases.push_str(&format!(
                        "{case_open}>\n      <skipped/>\n    </testcase>\n"
                    )),
                },
            }
        }
        format!(
            "\x20\x20<testsuite name=\"{name}\" tests=\"{tests}\" failures=\"{failures}\" \
             skipped=\"{skipped}\" time=\"{time:.3}\">\n\
             {cases}\x20\x20</testsuite>\n",
            name = xml_escape(&self.name),
            tests = self.steps.len(),
        )
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
            let mut detail = step.detail.as_deref().map(escape).unwrap_or_default();
            if step.degraded {
                let tier = step.selector_tier.as_deref().unwrap_or("fallback");
                detail.push_str(&format!(
                    "<span class=\"meta\">matched via {} fallback — heal the trace</span>",
                    escape(tier)
                ));
            }
            rows.push_str(&format!(
                "<tr><td><code>{}</code></td>\
                 <td><span class=\"badge\" style=\"background:{badge_color}\">{badge}</span></td>\
                 <td>{}</td><td>{}</td><td class=\"num\">{} ms</td></tr>\n",
                escape(&step.id),
                escape(&step.intent),
                detail,
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
             .frames img{{max-width:20rem;margin:.4rem .4rem 0 0;border:1px solid #d1d9e0}}\
             .whole-run{{max-width:100%;border:1px solid #d1d9e0;border-radius:.4rem}}\
             details{{margin:.5rem 0}}summary{{cursor:pointer}}\
             </style></head><body>\n\
             <h1>{name}</h1>\n\
             <p><span class=\"verdict\">{verdict}</span>{degraded}</p>\n\
             <p class=\"meta\">trace {trace_id} &middot; {duration} ms &middot; \
             generated from result.json</p>\n\
             <table><thead><tr><th>Step</th><th>Status</th><th>Intent</th>\
             <th>Detail</th><th>Duration</th></tr></thead><tbody>\n{rows}\
             </tbody></table>\n{viewer}</body></html>\n",
            name = escape(&self.name),
            trace_id = escape(&self.trace_id),
            duration = self.duration_ms,
            degraded = if self.degraded {
                " <span class=\"verdict\" style=\"background:#9a6700\">DEGRADED</span>"
            } else {
                ""
            },
            viewer = self.viewer_html(),
        )
    }

    /// The step-synchronized filmstrip viewer: for each step, its captured
    /// frames, referenced relatively inside the same run bundle. Driven
    /// entirely by the structured timeline — jumping to a step is a click,
    /// never manual scrubbing.
    fn viewer_html(&self) -> String {
        let Some(recording) = &self.recording else {
            return String::new();
        };
        let mut sections = String::from(
            "<h2>Recording</h2>\n<p class=\"meta\">step-synchronized keyframes; \
             sensitive regions are masked before frames are written</p>\n",
        );
        if let Some(gif) = &recording.gif {
            sections.push_str(&format!(
                "<p><img class=\"whole-run\" src=\"{dir}/{gif}\" \
                 alt=\"the whole run as an animated recording\"></p>\n\
                 <p class=\"meta\">the whole run, paced like the real \
                 execution — per-step frames below</p>\n",
                dir = escape(&recording.dir),
                gif = escape(gif),
            ));
        }
        for timing in &recording.steps {
            let intent = self
                .steps
                .iter()
                .find(|s| s.id == timing.id)
                .map(|s| s.intent.as_str())
                .unwrap_or("");
            let mut imgs = String::new();
            for frame in recording
                .frames
                .iter()
                .filter(|f| f.offset_ms >= timing.start_ms && f.offset_ms <= timing.end_ms)
            {
                imgs.push_str(&format!(
                    "<a href=\"{dir}/{file}\"><img src=\"{dir}/{file}\" \
                     alt=\"frame at {offset} ms\" loading=\"lazy\"></a>",
                    dir = escape(&recording.dir),
                    file = escape(&frame.file),
                    offset = frame.offset_ms,
                ));
            }
            let note = match &timing.frames_dropped {
                Some(reason) => format!(
                    "<p class=\"meta\">some frames were dropped (fail-closed {}).</p>",
                    escape(reason)
                ),
                None if imgs.is_empty() => "<p class=\"meta\">no frames captured.</p>".into(),
                None => String::new(),
            };
            sections.push_str(&format!(
                "<details><summary><code>{id}</code> {intent} \
                 <span class=\"meta\">({start}–{end} ms)</span></summary>\
                 <div class=\"frames\">{imgs}</div>{note}</details>\n",
                id = escape(&timing.id),
                intent = escape(intent),
                start = timing.start_ms,
                end = timing.end_ms,
            ));
        }
        sections
    }
}

/// Minimal HTML escaping for text content and attribute values.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// XML attribute/text escaping for the JUnit rendering.
fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
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
            degraded: false,
            duration_ms: 42,
            steps: vec![
                StepResult {
                    id: "s0001".into(),
                    intent: "Type 5 & smile".into(),
                    status: StepStatus::Passed,
                    detail: None,
                    started_ms: 0,
                    duration_ms: 10,
                    selector_tier: Some("native_id".into()),
                    degraded: false,
                },
                StepResult {
                    id: "s0002".into(),
                    intent: "display shows 8".into(),
                    status: StepStatus::Failed,
                    detail: Some("expected element text '8', got '<blank>'".into()),
                    started_ms: 10,
                    duration_ms: 5,
                    selector_tier: None,
                    degraded: false,
                },
            ],
            recording: None,
        };
        let html = report.to_html();
        assert!(html.contains("Add &lt;two&gt; numbers"));
        assert!(html.contains("Type 5 &amp; smile"));
        assert!(html.contains("got '&lt;blank&gt;'"));
        assert!(html.contains(">FAIL<"));
        assert!(!html.contains("<two>"), "raw input must never reach HTML");
        assert!(!html.contains("http"), "report must be self-contained");
        assert!(
            !html.contains("DEGRADED"),
            "healthy run shows no drift chip"
        );
    }

    #[test]
    fn degraded_steps_are_flagged_in_html_and_json() {
        let report = RunReport {
            name: "Write a note".into(),
            trace_id: "t-2".into(),
            passed: true,
            degraded: true,
            duration_ms: 20,
            steps: vec![StepResult {
                id: "s0001".into(),
                intent: "Type hello".into(),
                status: StepStatus::Passed,
                detail: None,
                started_ms: 0,
                duration_ms: 20,
                selector_tier: Some("structural".into()),
                degraded: true,
            }],
            recording: None,
        };
        let html = report.to_html();
        assert!(html.contains("DEGRADED"));
        assert!(html.contains("matched via structural fallback"));

        let json = serde_json::to_value(&report).expect("serializes");
        assert_eq!(json["degraded"], serde_json::Value::Bool(true));
        assert_eq!(json["steps"][0]["selector_tier"], "structural");
        assert_eq!(json["steps"][0]["degraded"], serde_json::Value::Bool(true));

        // The fields stay off the wire entirely for healthy runs.
        let healthy = RunReport {
            degraded: false,
            steps: vec![StepResult {
                selector_tier: None,
                degraded: false,
                ..report.steps[0].clone()
            }],
            ..report
        };
        let json = serde_json::to_value(&healthy).expect("serializes");
        assert!(json.get("degraded").is_none());
        assert!(json["steps"][0].get("degraded").is_none());
    }

    #[test]
    fn whole_run_gif_is_embedded_when_present() {
        let recording = flowproof_driver::Recording {
            format: "filmstrip/1".into(),
            dir: "recording".into(),
            frames: vec![],
            steps: vec![],
            gif: Some("recording.gif".into()),
        };
        let report = RunReport {
            name: "x".into(),
            trace_id: "t".into(),
            passed: true,
            degraded: false,
            duration_ms: 1,
            steps: vec![],
            recording: Some(recording),
        };
        let html = report.to_html();
        assert!(html.contains("src=\"recording/recording.gif\""));

        // Without a rendered GIF the viewer degrades to frames only.
        let mut report = report;
        report.recording.as_mut().expect("recording").gif = None;
        assert!(!report.to_html().contains("recording.gif"));
    }

    #[test]
    fn write_emits_json_html_and_junit_side_by_side() {
        let report = RunReport {
            name: "x".into(),
            trace_id: "t".into(),
            passed: true,
            degraded: false,
            duration_ms: 1,
            steps: vec![],
            recording: None,
        };
        let base = std::env::temp_dir().join("flowproof-report-write");
        std::fs::create_dir_all(&base).expect("temp dir");
        let json_path = report.write_into(&base).expect("write succeeds");
        assert!(json_path.ends_with("result.json"));
        assert!(json_path.with_file_name("report.html").exists());
        assert!(json_path.with_file_name("junit.xml").exists());
        std::fs::remove_dir_all(&base).ok();
    }

    fn junit_fixture() -> RunReport {
        RunReport {
            name: "Add <two> & \"quote\"".into(),
            trace_id: "t-1".into(),
            passed: false,
            degraded: false,
            duration_ms: 1234,
            steps: vec![
                StepResult {
                    id: "s0001".into(),
                    intent: "Type 5".into(),
                    status: StepStatus::Passed,
                    detail: None,
                    started_ms: 0,
                    duration_ms: 30,
                    selector_tier: None,
                    degraded: false,
                },
                StepResult {
                    id: "s0002".into(),
                    intent: "display shows <8>".into(),
                    status: StepStatus::Failed,
                    detail: Some("expected '8', got '<blank>'".into()),
                    started_ms: 30,
                    duration_ms: 25,
                    selector_tier: None,
                    degraded: false,
                },
                StepResult {
                    id: "s0003".into(),
                    intent: "Press equals".into(),
                    status: StepStatus::Skipped,
                    detail: Some("previous step failed".into()),
                    started_ms: 0,
                    duration_ms: 0,
                    selector_tier: None,
                    degraded: false,
                },
            ],
            recording: None,
        }
    }

    #[test]
    fn junit_xml_carries_counts_verdicts_and_escapes() {
        let xml = junit_fixture().to_junit_xml();
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("tests=\"3\" failures=\"1\" skipped=\"1\""));
        assert!(xml.contains("time=\"1.234\""));
        assert!(xml.contains("<testsuite name=\"Add &lt;two&gt; &amp; &quot;quote&quot;\""));
        assert!(xml.contains("name=\"s0001 Type 5\" time=\"0.030\"/>"));
        assert!(xml.contains(
            "<failure message=\"expected &apos;8&apos;, got &apos;&lt;blank&gt;&apos;\"/>"
        ));
        // Skip reasons travel into junit as the message attribute.
        assert!(xml.contains("<skipped message=\"previous step failed\"/>"));
        assert!(!xml.contains("<blank>"), "raw input must never reach XML");
    }

    #[test]
    fn suite_junit_merges_runs_and_sums_counts() {
        let mut second = junit_fixture();
        second.name = "Second flow".into();
        second.passed = true;
        second.steps.truncate(1);
        let xml = RunReport::suite_junit_xml([&junit_fixture(), &second]);
        assert!(
            xml.contains("<testsuites name=\"flowproof\" tests=\"4\" failures=\"1\" skipped=\"1\"")
        );
        assert_eq!(xml.matches("<testsuite name=").count(), 2);
        assert!(xml.contains("<testsuite name=\"Second flow\""));
        // Still exactly one document root.
        assert_eq!(xml.matches("<testsuites ").count(), 1);
    }

    #[test]
    fn junit_xml_is_well_formed() {
        // A hand-rolled serializer earns a real well-formedness check:
        // every opened element must close, attributes must stay quoted.
        let xml = junit_fixture().to_junit_xml();
        let opens = xml.matches("<testcase").count();
        let self_closed = xml.matches("/>").count();
        let closes = xml.matches("</testcase>").count();
        assert_eq!(opens, 3);
        assert_eq!(closes, 2, "failed + skipped cases have bodies");
        assert!(
            self_closed >= 3,
            "passed case + failure + skipped self-close"
        );
        assert_eq!(xml.matches("<testsuite ").count(), 1);
        assert_eq!(xml.matches("</testsuite>").count(), 1);
        assert_eq!(xml.matches("<testsuites ").count(), 1);
        assert_eq!(xml.matches("</testsuites>").count(), 1);
        assert_eq!(xml.matches('"').count() % 2, 0, "attribute quotes balance");
    }
}
