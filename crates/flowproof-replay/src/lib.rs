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
    #[error(transparent)]
    Secret(#[from] flowproof_trace::secret::MissingSecret),
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
            relation: None,
        },
        // A text anchor resolves by visible label (UIA Name / element
        // text / OCR line). `relation` rides along for pixels-only
        // drivers, which act NEXT TO the anchor, not on it.
        SelectorTier::TextAnchor => UiaSelector {
            name: get("text").or_else(|| get("name")),
            css: get("css"),
            nth,
            relation: get("relation"),
            ..UiaSelector::default()
        },
        // Visual matching needs the vision mode (not yet built); AI
        // relocation NEVER runs at replay time by design — it is the heal
        // workflow, which proposes a reviewable diff instead.
        SelectorTier::VisualTemplate | SelectorTier::AiRelocation => return None,
    };
    (!uia.is_empty()).then_some(uia)
}

/// On the run's first failure, enrich the failure `reason` and the run
/// bundle with what a human would look for next: nearest live text anchors
/// ("did you mean …?") when an anchored element wasn't found, and the
/// driver's debug bundle (DOM snapshot, console tail) written under
/// `<run_dir>/debug/`. Everything here is best-effort — diagnostics must
/// never turn one failure into two.
fn augment_failure<D: AppDriver>(
    driver: &mut D,
    step: &flowproof_trace::format::Step,
    run_dir: &Path,
    mut reason: String,
) -> String {
    // Both element-miss phrasings: direct resolution failure ("not
    // found") and the sync precondition timing out ("did not appear").
    if reason.contains("not found") || reason.contains("did not appear") {
        let wanted = step.selectors.iter().find_map(|s| {
            (s.tier == SelectorTier::TextAnchor)
                .then(|| s.payload.get("text").or_else(|| s.payload.get("name")))
                .flatten()
                .and_then(|v| v.as_str())
        });
        if let Some(wanted) = wanted {
            if let Ok(Some(scene)) = driver.scene() {
                let hints = nearest_anchor_hints(wanted, &scene);
                if !hints.is_empty() {
                    let list = hints
                        .iter()
                        .map(|h| format!("'{h}'"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    reason.push_str(&format!(" — did you mean {list}?"));
                }
            }
        }
    }
    if let Ok(Some(bundle)) = driver.debug_bundle() {
        let debug_dir = run_dir.join("debug");
        if std::fs::create_dir_all(&debug_dir).is_ok() {
            let mut wrote = Vec::new();
            if let Some(dom) = &bundle.dom_html {
                if std::fs::write(debug_dir.join("dom.html"), dom).is_ok() {
                    wrote.push("debug/dom.html");
                }
            }
            if !bundle.console.is_empty() {
                let text = bundle.console.join("\n") + "\n";
                if std::fs::write(debug_dir.join("console.log"), text).is_ok() {
                    wrote.push("debug/console.log");
                }
            }
            if !wrote.is_empty() {
                reason.push_str(&format!(" (captured: {})", wrote.join(", ")));
            }
        }
    }
    reason
}

/// The closest visible text anchors to `wanted`, from the driver's scene:
/// candidates whose (case-insensitive) edit distance is small relative to
/// the anchor's length, best first, at most three. Exact matches are
/// excluded — if the exact text is on screen, "not found" means something
/// else (ordinal, visibility), and a same-text hint would only confuse.
fn nearest_anchor_hints(wanted: &str, scene_json: &str) -> Vec<String> {
    let entries: Vec<serde_json::Value> = serde_json::from_str(scene_json).unwrap_or_default();
    let wanted_lower = wanted.to_lowercase();
    let budget = (wanted.chars().count() / 3).max(2);
    let mut scored: Vec<(usize, String)> = entries
        .iter()
        .flat_map(|e| {
            ["label", "text", "name"]
                .into_iter()
                .filter_map(|k| e[k].as_str())
        })
        .filter(|c| !c.is_empty() && c.to_lowercase() != wanted_lower)
        .map(|c| {
            (
                edit_distance(&wanted_lower, &c.to_lowercase()),
                c.to_string(),
            )
        })
        .filter(|(d, _)| *d <= budget)
        .collect();
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    scored.dedup_by(|a, b| a.1 == b.1);
    scored.into_iter().take(3).map(|(_, c)| c).collect()
}

/// Plain Levenshtein distance — tiny inputs (labels), no dependency needed.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
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

/// Spacing between the two rect samples of the stability gate — long
/// enough that a CSS transition moves the box between samples, short
/// enough that the fast path costs almost nothing.
const STABILITY_INTERVAL: Duration = Duration::from_millis(60);

/// An element can exist and still not be actionable: disabled while a
/// mutation is in flight, mid-animation, or under a toast/modal backdrop.
/// Gate element actions on enabled → stable → receives-events, polling to
/// the deadline — the flakiness class auto-waiting eliminates (issue #42).
/// Unknown answers (driver can't tell) satisfy the gate; the failure
/// message names the specific gate, which is what makes a flake
/// debuggable instead of mysterious.
fn wait_actionable<D: AppDriver>(
    driver: &mut D,
    target: &UiaSelector,
    timeout_ms: u64,
) -> Result<Result<(), String>, ReplayError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let gate = actionability_gate(driver, target)?;
        match gate {
            None => return Ok(Ok(())),
            Some(name) => {
                if Instant::now() >= deadline {
                    return Ok(Err(format!(
                        "element exists but is {name} after {timeout_ms}ms"
                    )));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
    }
}

/// One actionability pass: `None` = actionable, `Some(gate)` names the
/// first gate that failed.
fn actionability_gate<D: AppDriver>(
    driver: &mut D,
    target: &UiaSelector,
) -> Result<Option<&'static str>, ReplayError> {
    // Enabled: an Err means the driver has no enabled concept — satisfied.
    if !driver.element_enabled(target).unwrap_or(true) {
        return Ok(Some("disabled"));
    }
    // Stable: the bounding box must not move between two samples. None
    // (driver has no geometry) = satisfied.
    if let Some(first) = driver.element_rect(target)? {
        std::thread::sleep(STABILITY_INTERVAL);
        if driver.element_rect(target)? != Some(first) {
            return Ok(Some("unstable (still moving/animating)"));
        }
    }
    // Receives events at its center: None = driver can't tell, satisfied.
    if driver.element_receives_events(target)? == Some(false) {
        return Ok(Some("obscured (another element would receive the click)"));
    }
    Ok(None)
}

/// The auto-wait bound for the actionability gate: the step's recorded
/// existence precondition timeout when present, else the assert default.
fn actionable_timeout(step: &Step) -> u64 {
    step.sync
        .pre
        .iter()
        .find_map(|c| match c {
            Condition::ElementExists { timeout_ms, .. } => Some(*timeout_ms),
            _ => None,
        })
        .unwrap_or(DEFAULT_ASSERT_TIMEOUT_MS)
}

/// Extract the text expectation from an `element_state` expect object:
/// `(raw expectation, negated)`. None when it carries no text expectation.
fn text_expectation(expect: &serde_json::Value) -> Option<(&str, bool)> {
    if let Some(e) = expect.get("value_not_contains").and_then(|v| v.as_str()) {
        Some((e, true))
    } else if let Some(e) = expect.get("value_contains").and_then(|v| v.as_str()) {
        Some((e, false))
    } else {
        expect
            .get("value_equals")
            .and_then(|v| v.as_str())
            .map(|e| (e, false))
    }
}

/// Whether `text` satisfies the expectation — one predicate for every
/// provenance (element text, surface text, later OCR text).
fn text_matches(expect: &serde_json::Value, expected: &str, negated: bool, text: &str) -> bool {
    if negated {
        !text.contains(expected)
    } else if let Some(n) = expect.get("count").and_then(|v| v.as_u64()) {
        text.matches(expected).count() as u64 == n
    } else if expect.get("value_contains").is_some() {
        text.contains(expected)
    } else if expect.get("normalize").and_then(|v| v.as_str()) == Some("numeric") {
        matches!(
            (numeric_value(text), expected.parse::<f64>()),
            (Some(actual), Ok(wanted)) if actual == wanted
        )
    } else {
        text == expected
    }
}

/// Poll `read` until the text expectation in `expect` holds or `deadline`
/// passes. Provenance-agnostic: the caller decides what "read the text"
/// means (an element, the whole surface).
fn check_text_expectation<F>(
    expect: &serde_json::Value,
    deadline: Instant,
    rung: Option<usize>,
    mut read: F,
) -> Result<(Result<(), String>, Option<usize>), ReplayError>
where
    F: FnMut() -> Result<String, flowproof_driver::DriverError>,
{
    let Some((raw, negated)) = text_expectation(expect) else {
        return Ok((
            Err(format!("unsupported element_state expectation: {expect}")),
            rung,
        ));
    };
    let expected = match flowproof_trace::secret::resolve_refs(raw) {
        Ok(expected) => expected,
        Err(e) => return Ok((Err(e.to_string()), rung)),
    };
    loop {
        let text = read()?;
        if text_matches(expect, &expected, negated, &text) {
            return Ok((Ok(()), rung));
        }
        if Instant::now() >= deadline {
            let shown = if flowproof_trace::secret::has_refs(raw) {
                "<masked>"
            } else {
                text.as_str()
            };
            let verb = if negated { "no text" } else { "text" };
            return Ok((Err(format!("expected {verb} '{raw}', got '{shown}'")), rung));
        }
        std::thread::sleep(POLL_INTERVAL);
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
            let primary = selector_ref.unwrap_or(0);
            // Prefer the recorded rung, then fall through the rest of the
            // ladder — same degradation semantics as action targets. The
            // resolver runs INSIDE the poll loop: the target element may
            // legitimately still be appearing (a toast, a modal).
            let resolve = |driver: &mut D| -> Result<Option<(UiaSelector, usize)>, ReplayError> {
                let order =
                    std::iter::once(primary).chain((0..selectors.len()).filter(|&i| i != primary));
                for rung in order {
                    let Some(uia) = selectors.get(rung).and_then(selector_to_uia) else {
                        continue;
                    };
                    if driver.element_exists(&uia)? {
                        return Ok(Some((uia, rung)));
                    }
                }
                Ok(None)
            };
            // Assertions auto-wait: poll until the expectation holds or the
            // RECORDED timeout elapses — deterministic (bounded, and the
            // bound travels in the trace), no sleeps in specs.
            let timeout_ms = expect
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(DEFAULT_ASSERT_TIMEOUT_MS);
            let deadline = Instant::now() + Duration::from_millis(timeout_ms);

            // Surface-scoped: no selector to resolve — every adapter
            // answers `surface_text` its own way (page / window subtree /
            // OCR frame).
            if expect.get("scope").and_then(|v| v.as_str()) == Some("surface") {
                return check_text_expectation(expect, deadline, None, || driver.surface_text());
            }

            // Presence expectations: the element being there (or gone) IS
            // the assertion — no text involved.
            if let Some(wanted_present) = expect.get("element_present").and_then(|v| v.as_bool()) {
                loop {
                    let resolved = resolve(driver)?;
                    match (&resolved, wanted_present) {
                        (Some((_, rung)), true) => return Ok((Ok(()), Some(*rung))),
                        (None, false) => return Ok((Ok(()), None)),
                        _ => {}
                    }
                    if Instant::now() >= deadline {
                        let reason = if wanted_present {
                            "expected element to be visible, but it never appeared".to_string()
                        } else {
                            "expected element to be gone, but it is still on screen".to_string()
                        };
                        return Ok((Err(reason), resolved.map(|(_, rung)| rung)));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Enabled/disabled expectations: resolve the element, ask the
            // driver for its interactive state, poll until it matches.
            if let Some(wanted_enabled) = expect.get("enabled").and_then(|v| v.as_bool()) {
                let mut last: Option<bool> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver)? {
                        let enabled = driver.element_enabled(&uia)?;
                        if enabled == wanted_enabled {
                            return Ok((Ok(()), Some(rung)));
                        }
                        last = Some(enabled);
                    }
                    if Instant::now() >= deadline {
                        let state = |e: bool| if e { "enabled" } else { "disabled" };
                        let shown = match last {
                            Some(e) => state(e).to_string(),
                            None => "<element not found>".to_string(),
                        };
                        return Ok((
                            Err(format!(
                                "expected element to be {}, got {shown}",
                                state(wanted_enabled)
                            )),
                            None,
                        ));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            let Some((raw, negated)) = text_expectation(expect) else {
                return Ok((
                    Err(format!("unsupported element_state expectation: {expect}")),
                    None,
                ));
            };
            // Expectations may reference `${VAR}` secrets: resolve for the
            // comparison only — messages keep the raw reference, and the
            // live text is masked too, so a failure never leaks the value.
            let expected = match flowproof_trace::secret::resolve_refs(raw) {
                Ok(expected) => expected,
                Err(e) => return Ok((Err(e.to_string()), None)),
            };
            let mut last: Option<(String, usize)> = None;
            loop {
                if let Some((uia, rung)) = resolve(driver)? {
                    let text = driver.read_text(&uia)?;
                    if text_matches(expect, &expected, negated, &text) {
                        return Ok((Ok(()), Some(rung)));
                    }
                    last = Some((text, rung));
                }
                if Instant::now() >= deadline {
                    let (rung, shown) = match &last {
                        Some((text, rung)) => {
                            let shown = if flowproof_trace::secret::has_refs(raw) {
                                "<masked>"
                            } else {
                                text.as_str()
                            };
                            (Some(*rung), shown)
                        }
                        None => (None, "<element not found>"),
                    };
                    let verb = if negated {
                        "no element text"
                    } else {
                        "element text"
                    };
                    return Ok((Err(format!("expected {verb} '{raw}', got '{shown}'")), rung));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
        // Out-of-band: the posted record / the API response, not the pixel.
        // The trace stores the connection NAME and raw `${VAR}`-bearing
        // query/url; both resolve here, at the moment of use.
        Assertion::Sql {
            connection,
            query,
            expect,
        } => {
            let equals = expect
                .as_ref()
                .and_then(|e| e.get("equals"))
                .and_then(|v| v.as_str());
            let probe = flowproof_driver::oob::OobProbe::Sql {
                connection: connection.clone(),
                query: flowproof_trace::secret::resolve_refs(query)?,
                equals: match equals {
                    Some(e) => Some(flowproof_trace::secret::resolve_refs(e)?),
                    None => None,
                },
            };
            poll_oob(&probe, oob_timeout(expect.as_ref()))
        }
        Assertion::Api {
            request,
            status,
            expect,
        } => {
            let probe = flowproof_driver::oob::OobProbe::Api {
                method: request.method.clone(),
                url: flowproof_trace::secret::resolve_refs(&request.url)?,
                // Trace carries raw ${VAR} refs in body leaves and header
                // values; the probe gets the resolved data.
                body: match &request.body {
                    Some(b) => Some(flowproof_trace::secret::resolve_refs_in_json(b)?),
                    None => None,
                },
                headers: request
                    .headers
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), flowproof_trace::secret::resolve_refs(v)?)))
                    .collect::<Result<_, flowproof_trace::secret::MissingSecret>>()?,
                status: *status,
                // Resolved like `equals` above: the trace carries the raw
                // ${VAR}; only the live probe sees the value.
                body_contains: match expect
                    .as_ref()
                    .and_then(|e| e.get("body_contains"))
                    .and_then(|v| v.as_str())
                {
                    Some(needle) => Some(flowproof_trace::secret::resolve_refs(needle)?),
                    None => None,
                },
            };
            poll_oob(&probe, oob_timeout(expect.as_ref()))
        }
        other => Ok((
            Err(format!(
                "assertion kind not supported in this slice: {other:?}"
            )),
            None,
        )),
    }
}

fn oob_timeout(expect: Option<&serde_json::Value>) -> u64 {
    expect
        .and_then(|e| e.get("timeout_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or(DEFAULT_ASSERT_TIMEOUT_MS)
}

/// Auto-wait an out-of-band probe like any other assertion.
fn poll_oob(
    probe: &flowproof_driver::oob::OobProbe,
    timeout_ms: u64,
) -> Result<(Result<(), String>, Option<usize>), ReplayError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match flowproof_driver::oob::check(probe)? {
            Ok(()) => return Ok((Ok(()), None)),
            Err(reason) => {
                if Instant::now() >= deadline {
                    return Ok((Err(reason), None));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
        }
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
    base_url: &str,
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
        // Mid-flow navigation: `url` (relative paths resolve against the
        // flow's origin; `${VAR}` refs resolve now) or `reload: true`.
        Action::Launch(params) => {
            if params.get("reload").and_then(|v| v.as_bool()) == Some(true) {
                driver.reload()?;
                (Ok(()), StepMatch::default())
            } else if let Some(url) = params.get("url").and_then(|v| v.as_str()) {
                match flowproof_trace::secret::resolve_refs(url) {
                    Ok(path) => {
                        driver.navigate(&flowproof_driver::absolute_url(&path, base_url))?;
                        (Ok(()), StepMatch::default())
                    }
                    Err(e) => (Err(e.to_string()), StepMatch::default()),
                }
            } else {
                (
                    Err("launch step without url or reload".to_string()),
                    StepMatch::default(),
                )
            }
        }
        Action::Click(_) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        driver.invoke(&target)?;
                        (Ok(()), matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
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
                if let Err(reason) = wait_actionable(driver, &target, actionable_timeout(step))? {
                    return Ok((Err(reason), matched));
                }
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
                    // Portable primary modifier: the same trace presses
                    // Meta on macOS and Ctrl everywhere else.
                    flowproof_trace::format::KeyModifier::Mod => {
                        if cfg!(target_os = "macos") {
                            flowproof_driver::KeyMod::Meta
                        } else {
                            flowproof_driver::KeyMod::Ctrl
                        }
                    }
                })
                .collect();
            driver.press_key(&params.key, &mods)?;
            (Ok(()), StepMatch::default())
        }
        Action::Upload(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                // No actionability gate: file inputs are conventionally
                // hidden behind styled buttons (Playwright's setInputFiles
                // does not require visibility either).
                driver.set_files(&target, std::slice::from_ref(&params.path))?;
                (Ok(()), matched)
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        Action::RightClick(_) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        driver.context_click(&target)?;
                        (Ok(()), matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
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
        let raw = header
            .app
            .url
            .clone()
            .ok_or_else(|| ReplayError::UnknownApp("web trace without url".into()))?;
        flowproof_driver::AppTarget {
            // `${VAR}` refs in the recorded URL resolve at every replay.
            command: flowproof_trace::secret::resolve_refs(&raw)?,
            window_name: String::new(),
        }
    } else if header.app.name == "sap" {
        // The header's `url` carries the SAP Logon connection description
        // (may be a `${VAR}` ref); absent = attach to the running session.
        let raw = header.app.url.clone().unwrap_or_default();
        flowproof_driver::AppTarget {
            command: flowproof_trace::secret::resolve_refs(&raw)?,
            window_name: "SAP".into(),
        }
    } else if header.app.name == "vision" {
        // Pixels mode re-attaches to the window recorded in the header.
        let raw =
            header.app.window_title.clone().ok_or_else(|| {
                ReplayError::UnknownApp("vision trace without window title".into())
            })?;
        flowproof_driver::AppTarget {
            command: String::new(),
            window_name: flowproof_trace::secret::resolve_refs(&raw)?,
        }
    } else if header.app.name == "api" {
        // Out-of-band only: NoOpDriver::launch ignores this.
        flowproof_driver::AppTarget {
            command: String::new(),
            window_name: String::new(),
        }
    } else {
        resolve_app(&header.app.name)
            .ok_or_else(|| ReplayError::UnknownApp(header.app.name.clone()))?
    };
    // Session state travels in the header (values may be `${VAR}` refs):
    // stage it so the driver applies it before the page loads — replays
    // authenticate exactly like the recording did.
    if let Some(setup) = &header.session {
        let (cookies, local_storage) = setup.resolved()?;
        driver.stage_session(flowproof_driver::WebSession {
            cookies,
            local_storage,
        })?;
    }
    // Mock rules travel in the header: replays intercept exactly what the
    // recording intercepted, or the two executions test different things.
    if !header.mock.is_empty() {
        driver.stage_mocks(
            header
                .mock
                .iter()
                .map(|m| {
                    flowproof_driver::WebMock::from_rule_parts(
                        &m.url_contains,
                        m.method.as_deref(),
                        m.status,
                        m.content_type.as_deref(),
                        m.body.as_ref(),
                    )
                })
                .collect(),
        )?;
    }
    // The browser shape travels in the header too: a flow recorded on an
    // emulated phone viewport must not replay on a desktop one.
    if let Some(browser) = &header.browser {
        if !browser.is_empty() {
            driver.stage_browser(flowproof_driver::WebBrowserConfig::from_setup_parts(
                browser
                    .viewport
                    .as_ref()
                    .map(|v| (v.width, v.height, v.device_scale_factor, v.mobile, v.touch)),
                browser.user_agent.as_deref(),
                &browser.args,
            ))?;
        }
    }
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
        let (outcome, matched) = execute_step(driver, step, &target.command)?;
        let duration_ms = step_started.elapsed().as_millis() as u64;
        if let Some(rec) = recorder.as_mut() {
            rec.step_finished(driver);
        }
        let mut result = match outcome {
            Ok(()) => StepResult::passed(step, started_ms, duration_ms),
            Err(reason) => {
                failed = true;
                // First failure: capture what the app actually looked like
                // (DOM + console into the run bundle) and suggest nearest
                // text anchors — the questions a human asks first, answered
                // without a re-run. Best-effort by design.
                let reason = augment_failure(driver, step, &run_dir, reason);
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

#[cfg(test)]
mod failure_hint_tests {
    use super::*;

    #[test]
    fn edit_distance_is_levenshtein() {
        assert_eq!(edit_distance("save", "save"), 0);
        assert_eq!(edit_distance("save", "sale"), 1);
        assert_eq!(edit_distance("save", "safes"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
    }

    #[test]
    fn hints_rank_close_labels_and_skip_exact_and_far() {
        let scene = r#"[
            {"label": "Save changes", "tag": "button"},
            {"text": "Sace change", "tag": "button"},
            {"label": "Delete everything"},
            {"label": "Save change"}
        ]"#;
        // Exact-equal candidates are excluded; far ones filtered; rest
        // best-first.
        let hints = nearest_anchor_hints("Save change", scene);
        assert_eq!(hints, vec!["Sace change", "Save changes"]);
        assert!(nearest_anchor_hints("Save change", "not json").is_empty());
    }

    #[test]
    fn hints_are_case_insensitive_and_capped_at_three() {
        let scene = r#"[
            {"label": "LOGIN"}, {"label": "Logins"}, {"label": "Log in"},
            {"label": "Loginn"}, {"label": "Logging"}
        ]"#;
        let hints = nearest_anchor_hints("login", scene);
        assert_eq!(hints.len(), 3, "top three only: {hints:?}");
        // "LOGIN" differs only by case = exact match, excluded.
        assert!(!hints.iter().any(|h| h == "LOGIN"), "{hints:?}");
    }
}
