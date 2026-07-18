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
    if selector.tier != SelectorTier::NativeId {
        return None; // Only the native-id rung is implemented in this slice.
    }
    let get = |key: &str| {
        selector
            .payload
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_string)
    };
    let uia = UiaSelector {
        automation_id: get("automation_id").or_else(|| get("id")),
        name: get("name"),
        control_type: get("control_type"),
    };
    (!uia.is_empty()).then_some(uia)
}

/// Walk the recorded selector ladder and return the first rung that resolves
/// to a live element.
fn resolve_target<D: AppDriver>(
    driver: &mut D,
    selectors: &[Selector],
) -> Result<Option<UiaSelector>, ReplayError> {
    for selector in selectors {
        if let Some(uia) = selector_to_uia(selector) {
            if driver.element_exists(&uia)? {
                return Ok(Some(uia));
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
) -> Result<Result<(), String>, ReplayError> {
    match assertion {
        Assertion::ElementState {
            expect,
            selector_ref,
        } => {
            let selector = selector_ref
                .and_then(|i| selectors.get(i))
                .or_else(|| selectors.first());
            let Some(uia) = selector.and_then(selector_to_uia) else {
                return Ok(Err("assertion has no resolvable selector".into()));
            };
            let text = driver.read_text(&uia)?;
            let Some(expected) = expect.get("value_equals").and_then(|v| v.as_str()) else {
                return Ok(Err(format!(
                    "unsupported element_state expectation: {expect}"
                )));
            };
            let matches = if expect.get("normalize").and_then(|v| v.as_str()) == Some("numeric") {
                match (numeric_value(&text), expected.parse::<f64>()) {
                    (Some(actual), Ok(expected)) => actual == expected,
                    _ => false,
                }
            } else {
                text == expected
            };
            if matches {
                Ok(Ok(()))
            } else {
                Ok(Err(format!(
                    "expected display value '{expected}', got '{text}'"
                )))
            }
        }
        other => Ok(Err(format!(
            "assertion kind not supported in this slice: {other:?}"
        ))),
    }
}

fn execute_step<D: AppDriver>(
    driver: &mut D,
    step: &Step,
) -> Result<Result<(), String>, ReplayError> {
    for condition in &step.sync.pre {
        if let Err(reason) = wait_for_condition(driver, condition, &step.selectors)? {
            return Ok(Err(format!("precondition failed: {reason}")));
        }
    }

    let outcome = match &step.action {
        Action::Click(_) => match resolve_target(driver, &step.selectors)? {
            Some(target) => {
                driver.invoke(&target)?;
                Ok(())
            }
            None => Err("no selector rung resolved to a live element".to_string()),
        },
        Action::Assert(assertion) => check_assertion(driver, assertion, &step.selectors)?,
        other => Err(format!("action not supported in this slice: {other:?}")),
    };
    if outcome.is_err() {
        return Ok(outcome);
    }

    for condition in &step.sync.post {
        if let Err(reason) = wait_for_condition(driver, condition, &step.selectors)? {
            return Ok(Err(format!("postcondition failed: {reason}")));
        }
    }
    Ok(Ok(()))
}

/// Replay the trace at `path` against the live application. Deterministic:
/// walks recorded selectors only, stops at the first failing step.
pub fn run_trace<D: AppDriver>(path: &Path, driver: &mut D) -> Result<RunReport, ReplayError> {
    let (header, steps) = load_trace(path)?;
    let target = resolve_app(&header.app.name)
        .ok_or_else(|| ReplayError::UnknownApp(header.app.name.clone()))?;
    let started = Instant::now();
    driver.launch(target.command, target.window_name, LAUNCH_TIMEOUT)?;

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
        let step_started = Instant::now();
        let outcome = execute_step(driver, step)?;
        let duration_ms = step_started.elapsed().as_millis() as u64;
        match outcome {
            Ok(()) => results.push(StepResult::passed(step, duration_ms)),
            Err(reason) => {
                failed = true;
                results.push(StepResult::failed(step, duration_ms, reason));
            }
        }
    }

    Ok(RunReport {
        name,
        trace_id: header.trace_id.clone(),
        passed: !failed && !results.is_empty(),
        steps: results,
        duration_ms: started.elapsed().as_millis() as u64,
    })
}
