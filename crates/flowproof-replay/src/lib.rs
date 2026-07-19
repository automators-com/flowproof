//! Deterministic replay of recorded traces. No LLM calls happen here, ever:
//! replay walks the selector ladder recorded in the trace and fails with a
//! structured report when a step cannot be resolved. Healing (which may call
//! a model) is a separate, explicit workflow that produces a reviewable diff.

pub mod report;

use std::path::Path;
use std::time::{Duration, Instant};

use flowproof_driver::{numeric_value, resolve_app, AppDriver, UiaSelector};
use flowproof_trace::format::{Action, Assertion, Condition, Header, Selector, Step};
use flowproof_trace::{SelectorTier, TraceLine};

pub use report::{RunReport, StepResult, StepStatus};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Auto-wait bound for asserts in traces recorded before timeouts existed.
const DEFAULT_ASSERT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("cannot read trace {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid trace: {0}")]
    Trace(#[from] flowproof_trace::TraceError),
    #[error("trace has no header line")]
    MissingHeader,
    #[error("unknown app '{0}' in trace header")]
    UnknownApp(String),
    #[error("driver error: {0}")]
    Driver(#[from] flowproof_driver::DriverError),
}

/// Parse a trace file into its header and steps.
pub fn load_trace(path: &Path) -> Result<(Header, Vec<Step>), ReplayError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ReplayError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut lines = contents.lines().filter(|l| !l.trim().is_empty());
    let header = match lines.next() {
        Some(line) => match TraceLine::parse(line)? {
            TraceLine::Header(header) => header,
            TraceLine::Step(_) => return Err(ReplayError::MissingHeader),
        },
        None => return Err(ReplayError::MissingHeader),
    };
    let mut steps = Vec::new();
    for line in lines {
        match TraceLine::parse(line)? {
            TraceLine::Step(step) => steps.push(step),
            TraceLine::Header(_) => return Err(ReplayError::MissingHeader),
        }
    }
    Ok((header, steps))
}

fn selector_to_uia(selector: &Selector) -> Option<UiaSelector> {
    let get = |key: &str| {
        selector
            .payload
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let nth = selector
        .payload
        .get("nth")
        .and_then(|v| v.as_u64())
        .map(|n| n as u32);
    let uia = match selector.tier {
        // Both deterministic element-property tiers share the same driver
        // query surface; they differ in what the payload anchors on.
        SelectorTier::NativeId | SelectorTier::Structural => UiaSelector {
            automation_id: get("automation_id").or_else(|| get("id")),
            name: get("name"),
            control_type: get("control_type"),
            css: get("css"),
            nth,
        },
        // A text anchor resolves by visible label (UIA Name / element text).
        SelectorTier::TextAnchor => UiaSelector {
            name: get("text").or_else(|| get("name")),
            css: get("css"),
            nth,
            ..UiaSelector::default()
        },
        // Visual matching needs the vision mode (not yet built); AI
        // relocation NEVER runs at replay time by design — it is the heal
        // workflow, which proposes a reviewable diff instead.
        SelectorTier::VisualTemplate | SelectorTier::AiRelocation => return None,
    };
    (!uia.is_empty()).then_some(uia)
}

/// Walk the recorded selector ladder and return the first rung that resolves
/// to a live element, with its index — index > 0 means the primary selector
/// no longer matches and the run is degraded (the app drifted; heal).
fn resolve_target<D: AppDriver>(
    driver: &mut D,
    selectors: &[Selector],
) -> Result<Option<(UiaSelector, usize)>, ReplayError> {
    for (rung, selector) in selectors.iter().enumerate() {
        if let Some(uia) = selector_to_uia(selector) {
            if driver.element_exists(&uia)? {
                return Ok(Some((uia, rung)));
            }
        }
    }
    Ok(None)
}

fn wait_for_condition<D: AppDriver>(
    driver: &mut D,
    condition: &Condition,
    selectors: &[Selector],
) -> Result<Result<(), String>, ReplayError> {
    match condition {
        Condition::ElementExists {
            timeout_ms,
            selector_ref,
        } => {
            let targets: Vec<&Selector> = match selector_ref {
                Some(i) => selectors.get(*i).into_iter().collect(),
                None => selectors.iter().collect(),
            };
            // A targetless step (key press, focused typing) has nothing to
            // wait for.
            if targets.is_empty() {
                return Ok(Ok(()));
            }
            let deadline = Instant::now() + Duration::from_millis(*timeout_ms);
            loop {
                for selector in &targets {
                    if let Some(uia) = selector_to_uia(selector) {
                        if driver.element_exists(&uia)? {
                            return Ok(Ok(()));
                        }
                    }
                }
                if Instant::now() >= deadline {
                    return Ok(Err(format!("element did not appear within {timeout_ms}ms")));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
        // Other condition kinds are recorded but not yet evaluated in this
        // slice; treat them as satisfied rather than silently failing runs.
        _ => Ok(Ok(())),
    }
}

fn check_assertion<D: AppDriver>(
    driver: &mut D,
    assertion: &Assertion,
    selectors: &[Selector],
) -> Result<(Result<(), String>, Option<usize>), ReplayError> {
    match assertion {
        Assertion::ElementState {
            expect,
            selector_ref,
        } => {
            // Prefer the recorded rung, then fall through the rest of the
            // ladder — same degradation semantics as action targets.
            let primary = selector_ref.unwrap_or(0);
            let order =
                std::iter::once(primary).chain((0..selectors.len()).filter(|&i| i != primary));
            let mut resolved = None;
            for rung in order {
                let Some(uia) = selectors.get(rung).and_then(selector_to_uia) else {
                    continue;
                };
                if driver.element_exists(&uia)? {
                    resolved = Some((uia, rung));
                    break;
                }
            }
            let Some((uia, rung)) = resolved else {
                return Ok((
                    Err("no selector rung resolved to a live element".into()),
                    None,
                ));
            };
            let raw = if let Some(e) = expect.get("value_contains").and_then(|v| v.as_str()) {
                e
            } else if let Some(e) = expect.get("value_equals").and_then(|v| v.as_str()) {
                e
            } else {
                return Ok((
                    Err(format!("unsupported element_state expectation: {expect}")),
                    Some(rung),
                ));
            };
            // Expectations may reference `${VAR}` secrets: resolve for the
            // comparison only — messages keep the raw reference, and the
            // live text is masked too, so a failure never leaks the value.
            let expected = match flowproof_trace::secret::resolve_refs(raw) {
                Ok(expected) => expected,
                Err(e) => return Ok((Err(e.to_string()), Some(rung))),
            };
            // Assertions auto-wait: poll until the expectation holds or the
            // RECORDED timeout elapses — deterministic (bounded, and the
            // bound travels in the trace), no sleeps in specs.
            let timeout_ms = expect
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_ASSERT_TIMEOUT_MS);
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);
            let mut text = driver.read_text(&uia)?;
            loop {
                let matches = if expect.get("value_contains").is_some() {
                    text.contains(&expected)
                } else if expect.get("normalize").and_then(|v| v.as_str()) == Some("numeric") {
                    matches!(
                        (numeric_value(&text), expected.parse::<f64>()),
                        (Some(actual), Ok(wanted)) if actual == wanted
                    )
                } else {
                    text == expected
                };
                if matches {
                    return Ok((Ok(()), Some(rung)));
                }
                if Instant::now() >= deadline {
                    let shown = if flowproof_trace::secret::has_refs(raw) {
                        "<masked>"
                    } else {
                        text.as_str()
                    };
                    return Ok((
                        Err(format!("expected element text '{raw}', got '{shown}'")),
                        Some(rung),
                    ));
                }
                std::thread::sleep(POLL_INTERVAL);
                text = driver.read_text(&uia)?;
            }
        }
        other => Ok((
            Err(format!(
                "assertion kind not supported in this slice: {other:?}"
            )),
            None,
        )),
    }
}

/// How a step's target was found: which ladder tier matched, and whether
/// that was a fallback below the recorded primary rung (drift signal).
#[derive(Debug, Clone, Copy, Default)]
struct StepMatch {
    tier: Option<SelectorTier>,
    degraded: bool,
}

impl StepMatch {
    fn from_rung(selectors: &[Selector], rung: Option<usize>, primary: usize) -> Self {
        Self {
            tier: rung.and_then(|r| selectors.get(r)).map(|s| s.tier),
            degraded: rung.is_some_and(|r| r != primary),
        }
    }
}

fn execute_step<D: AppDriver>(
    driver: &mut D,
    step: &Step,
) -> Result<(Result<(), String>, StepMatch), ReplayError> {
    for condition in &step.sync.pre {
        if let Err(reason) = wait_for_condition(driver, condition, &step.selectors)? {
            return Ok((
                Err(format!("precondition failed: {reason}")),
                StepMatch::default(),
            ));
        }
    }

    let (outcome, matched) = match &step.action {
        Action::Click(_) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                driver.invoke(&target)?;
                (Ok(()), StepMatch::from_rung(&step.selectors, Some(rung), 0))
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        // An empty selector list means "type into the focused element".
        Action::TypeText(params) if step.selectors.is_empty() => {
            match flowproof_trace::secret::resolve_refs(&params.text) {
                Ok(value) => {
                    driver.type_focused(&value)?;
                    (Ok(()), StepMatch::default())
                }
                Err(e) => (Err(e.to_string()), StepMatch::default()),
            }
        }
        Action::TypeText(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                // The trace stores `${VAR}` secret references, never values;
                // they resolve from the environment at the moment of typing.
                match flowproof_trace::secret::resolve_refs(&params.text) {
                    Ok(value) => {
                        // `replace: true` marks fill semantics: clear the
                        // current value first (`Clear the … field` records
                        // this with an empty text).
                        let replace =
                            params.extra.get("replace").and_then(|v| v.as_bool()) == Some(true);
                        if replace {
                            driver.clear_text(&target)?;
                        }
                        if !value.is_empty() {
                            driver.type_text(&target, &value)?;
                        }
                        (Ok(()), matched)
                    }
                    Err(e) => (Err(e.to_string()), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        Action::PressKey(params) => {
            let mods: Vec<flowproof_driver::KeyMod> = params
                .modifiers
                .iter()
                .map(|m| match m {
                    flowproof_trace::format::KeyModifier::Ctrl => flowproof_driver::KeyMod::Ctrl,
                    flowproof_trace::format::KeyModifier::Alt => flowproof_driver::KeyMod::Alt,
                    flowproof_trace::format::KeyModifier::Shift => flowproof_driver::KeyMod::Shift,
                    flowproof_trace::format::KeyModifier::Win => flowproof_driver::KeyMod::Meta,
                })
                .collect();
            driver.press_key(&params.key, &mods)?;
            (Ok(()), StepMatch::default())
        }
        Action::Assert(assertion) => {
            let (outcome, rung) = check_assertion(driver, assertion, &step.selectors)?;
            let primary = match assertion {
                Assertion::ElementState { selector_ref, .. } => selector_ref.unwrap_or(0),
                _ => 0,
            };
            (
                outcome,
                StepMatch::from_rung(&step.selectors, rung, primary),
            )
        }
        other => (
            Err(format!("action not supported in this slice: {other:?}")),
            StepMatch::default(),
        ),
    };
    if outcome.is_err() {
        return Ok((outcome, matched));
    }

    for condition in &step.sync.post {
        if let Err(reason) = wait_for_condition(driver, condition, &step.selectors)? {
            return Ok((Err(format!("postcondition failed: {reason}")), matched));
        }
    }
    Ok((Ok(()), matched))
}

/// Replay the trace at `path` against the live application. Deterministic:
/// walks recorded selectors only, stops at the first failing step. Creates
/// the run's self-contained artifact directory up front so the recording
/// bundle and the reports land together; returns it alongside the report.
pub fn run_trace<D: AppDriver>(
    path: &Path,
    driver: &mut D,
) -> Result<(RunReport, std::path::PathBuf), ReplayError> {
    let (header, steps) = load_trace(path)?;

    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let run_id = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
    let run_dir = base.join(".flowproof").join("runs").join(run_id);
    std::fs::create_dir_all(&run_dir).map_err(|source| ReplayError::Io {
        path: run_dir.display().to_string(),
        source,
    })?;

    // Redaction rules travel in the trace; replays mask identically without
    // the spec. Fail closed: if any recorded rule cannot be understood, no
    // frames are captured at all rather than risking an unmasked frame.
    let rules: Option<Vec<flowproof_driver::RedactionRule>> = header
        .redaction
        .iter()
        .map(|value| serde_json::from_value(value.clone()).ok())
        .collect();
    let mut recorder =
        rules.and_then(|rules| flowproof_driver::RunRecorder::new(&run_dir, rules).ok());
    let target = if header.app.name == "web" {
        flowproof_driver::AppTarget {
            command: header
                .app
                .url
                .clone()
                .ok_or_else(|| ReplayError::UnknownApp("web trace without url".into()))?,
            window_name: String::new(),
        }
    } else {
        resolve_app(&header.app.name)
            .ok_or_else(|| ReplayError::UnknownApp(header.app.name.clone()))?
    };
    let started = Instant::now();
    driver.launch(&target.command, &target.window_name, LAUNCH_TIMEOUT)?;

    let name = header
        .spec
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| header.app.name.clone());
    let mut results = Vec::with_capacity(steps.len());
    let mut failed = false;
    for step in &steps {
        if failed {
            results.push(StepResult::skipped(step));
            continue;
        }
        if let Some(rec) = recorder.as_mut() {
            rec.step_started(driver, &step.id);
        }
        let step_started = Instant::now();
        let started_ms = started.elapsed().as_millis() as u64;
        let (outcome, matched) = execute_step(driver, step)?;
        let duration_ms = step_started.elapsed().as_millis() as u64;
        if let Some(rec) = recorder.as_mut() {
            rec.step_finished(driver);
        }
        let mut result = match outcome {
            Ok(()) => StepResult::passed(step, started_ms, duration_ms),
            Err(reason) => {
                failed = true;
                StepResult::failed(step, started_ms, duration_ms, reason)
            }
        };
        result.selector_tier = matched.tier.map(|t| t.name().to_string());
        result.degraded = matched.degraded;
        results.push(result);
    }

    let degraded = results.iter().any(|s| s.degraded);
    let report = RunReport {
        name,
        trace_id: header.trace_id.clone(),
        passed: !failed && !results.is_empty(),
        degraded,
        steps: results,
        duration_ms: started.elapsed().as_millis() as u64,
        recording: recorder.and_then(flowproof_driver::RunRecorder::finish),
    };
    Ok((report, run_dir))
}
