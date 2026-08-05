//! Deterministic replay of recorded traces. No LLM calls happen here, ever:
//! replay walks the selector ladder recorded in the trace and fails with a
//! structured report when a step cannot be resolved. Healing (which may call
//! a model) is a separate, explicit workflow that produces a reviewable diff.

pub mod report;
pub mod runrecord;

use std::path::Path;
use std::time::{Duration, Instant};

use flowproof_driver::{numeric_value, resolve_app, AppDriver, UiaSelector};
use flowproof_trace::format::{Action, Assertion, Condition, Header, Selector, Step};
use flowproof_trace::{SelectorTier, TraceLine};

pub use report::{RunReport, StepResult, StepStatus};
pub use runrecord::{
    ControlRecord, ControlVerdict, Evidence, FlowRecord, FlowStatus, RunDiff, RunEnv, RunRecord,
};

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(200);
/// Auto-wait bound for asserts in traces recorded before timeouts existed.
const DEFAULT_ASSERT_TIMEOUT_MS: u64 = 10_000;

/// The `assert_no_secret_leak` scan to run on this replay, derived from the
/// SPEC (the trace stores no secret-leak steps; the feature is additive). A
/// replay re-observes the same corpus as record and scans it by the same
/// shared mechanism, so an unchanged system replays the same verdict. Empty
/// `assertions` means the flow does not use the feature: no corpus is
/// captured and replay behaves exactly as it always has.
#[derive(Debug, Clone, Default)]
pub struct SecretScan {
    pub assertions: Vec<flowproof_trace::secret_scan::LeakAssertion>,
}

impl SecretScan {
    /// A flow that does not assert `assert_no_secret_leak`: no capture, no
    /// scan, behaviour identical to before the feature existed.
    pub fn disabled() -> Self {
        Self::default()
    }

    fn enabled(&self) -> bool {
        !self.assertions.is_empty()
    }
}

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
    // A missing key is an EMPTY list, not an error: every trace written
    // before conjunction existed simply has no extra anchors.
    let get_list = |key: &str| -> Vec<String> {
        selector
            .payload
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
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
            // A `kind: "cell"` structural payload decodes back into the
            // cell query the web adapter resolves by header text + row
            // anchor, carrying whatever hints record time harvested.
            cell: (get("kind").as_deref() == Some("cell")).then(|| flowproof_driver::CellQuery {
                column: get("column_text").unwrap_or_default(),
                anchor: get("row_anchor").unwrap_or_default(),
                // Absent in every trace written before conjunction, which
                // decodes to the single-anchor behaviour unchanged.
                also: get_list("row_anchor_also"),
                column_field: get("column_field"),
                row_id: get("row_id"),
            }),
            // A `kind: "scoped"` structural payload decodes into the
            // container query. Its inner keys are PREFIXED, so the
            // `automation_id`/`css` reads above see nothing: an engine that
            // predates this rung gets an EMPTY selector, skips the rung and
            // fails loudly, instead of resolving `inner_text` page-wide and
            // passing on the wrong element.
            // A `kind: "framed"` structural payload decodes into the frame
            // query. Its inner keys are PREFIXED for the same reason the
            // scoped rung prefixes them: an engine that predates this rung
            // sees an EMPTY selector and fails loudly instead of resolving
            // the inner target against the MAIN document.
            frame: (get("kind").as_deref() == Some("framed")).then(|| {
                flowproof_driver::FrameQuery {
                    frame: get("frame").unwrap_or_default(),
                    inner_css: get("inner_css"),
                    inner_id: get("inner_id"),
                    inner_text: get("inner_text"),
                }
            }),
            scope: (get("kind").as_deref() == Some("scoped")).then(|| {
                flowproof_driver::ScopeQuery {
                    container: get("container").unwrap_or_default(),
                    anchor: get("container_anchor").unwrap_or_default(),
                    also: get_list("anchor_also"),
                    inner_css: get("inner_css"),
                    inner_id: get("inner_id"),
                    inner_text: get("inner_text").or_else(|| get("inner_name")),
                    container_id: get("container_id"),
                }
            }),
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
    // A scoped target that timed out has one miss worth naming: the anchor
    // IS on the surface, but it sits in no container the closed `item` list
    // covers. "not found" would send the author looking for a typo; this
    // says what to write instead.
    for selector in &step.selectors {
        let Some(uia) = selector_to_uia(selector) else {
            continue;
        };
        let Some(scope) = uia.scope.clone() else {
            continue;
        };
        if let Ok(Some(hints)) = driver.scope_hints(&uia) {
            if hints.anchor_without_container {
                reason.push_str(&format!(
                    " - '{}' is visible but sits in no list item - name the container with \
                     \"css:…\"",
                    scope.anchor
                ));
            }
        }
        break;
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

/// An element can exist and still not be actionable: disabled while a
/// mutation is in flight, mid-animation, or under a toast/modal backdrop.
/// Gate element actions on enabled → stable → receives-events, polling to
/// the deadline — the flakiness class auto-waiting eliminates (issue #42).
/// Unknown answers (driver can't tell) satisfy the gate; the failure
/// message names the specific gate, which is what makes a flake
/// debuggable instead of mysterious. The pass itself is the driver's
/// [`AppDriver::actionability_gate`], so a driver that answers all three
/// questions in one round trip can.
fn wait_actionable<D: AppDriver>(
    driver: &mut D,
    target: &UiaSelector,
    timeout_ms: u64,
) -> Result<Result<(), String>, ReplayError> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        let gate = driver.actionability_gate(target)?;
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
    // An emptiness check carries no expected text; text_matches reads the
    // `value_empty` flag directly. Return an empty needle so the poll loop
    // still runs.
    if expect.get("value_empty").is_some() {
        return Some(("", false));
    }
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
///
/// Case-insensitive FALLBACK, mirroring element anchors: an exact match
/// always wins; when it misses, lowercased comparison decides.
///
/// The fallback is deliberately **widening-only**, so it can never turn a
/// passing trace into a failing one. That means the NEGATIVE form does not
/// get it: `page does not show friends` passed against a rendered "FRIENDS"
/// before the fallback existed, and mirroring the positive form there would
/// start failing it. Symmetry is the lesser property - a recorded trace
/// that passed must keep passing. Counts widen the same way: a nonzero
/// case-sensitive count IS the count, and the lowercased count is consulted
/// only when the case-sensitive one found nothing.
fn text_matches(expect: &serde_json::Value, expected: &str, negated: bool, text: &str) -> bool {
    if let Some(want_empty) = expect.get("value_empty").and_then(|v| v.as_bool()) {
        return text.trim().is_empty() == want_empty;
    }
    let (text_ci, expected_ci) = (text.to_lowercase(), expected.to_lowercase());
    if negated {
        !text.contains(expected)
    } else if let Some(n) = expect.get("count").and_then(|v| v.as_u64()) {
        let sensitive = text.matches(expected).count() as u64;
        sensitive == n || (sensitive == 0 && text_ci.matches(&expected_ci).count() as u64 == n)
    } else if expect.get("value_contains").is_some() {
        text.contains(expected) || text_ci.contains(&expected_ci)
    } else if expect.get("normalize").and_then(|v| v.as_str()) == Some("numeric") {
        matches!(
            (numeric_value(text), expected.parse::<f64>()),
            (Some(actual), Ok(wanted)) if actual == wanted
        )
    } else {
        text == expected || text_ci == expected_ci
    }
}

/// Poll `read` until the text expectation in `expect` holds or `deadline`
/// passes. Provenance-agnostic: the caller decides what "read the text"
/// means (an element, the whole surface).
/// Judge an assertion whose target sits inside a same-origin iframe.
///
/// The three probe states stay distinct all the way to the verdict:
/// a frame that has not rendered yet keeps polling (it may still appear), a
/// cross-origin frame stops immediately with its own message (waiting cannot
/// fix a same-origin-policy wall, and reporting it as "absent" would be a
/// lie that passes a `does not show` assertion), and a reached frame is
/// judged on what is actually inside it.
fn check_framed_expectation<D: AppDriver>(
    driver: &mut D,
    expect: &serde_json::Value,
    query: &flowproof_driver::FrameQuery,
    deadline: Instant,
    rung: usize,
) -> Result<(Result<(), String>, Option<usize>), ReplayError> {
    use flowproof_driver::FrameProbe;
    let inner = query
        .inner_css
        .as_deref()
        .or(query.inner_id.as_deref())
        .or(query.inner_text.as_deref())
        .unwrap_or("the target");
    let wanted_present = expect.get("element_present").and_then(|v| v.as_bool());
    let text_want = text_expectation(expect);
    let expected = match text_want {
        Some((raw, _)) => match flowproof_trace::secret::resolve_refs(raw) {
            Ok(resolved) => Some(resolved),
            Err(e) => return Ok((Err(e.to_string()), Some(rung))),
        },
        None => None,
    };
    let mut fault: Option<flowproof_driver::DriverError> = None;
    let mut last: Option<String> = None;
    let mut missing_frame: Option<Vec<String>> = None;
    let mut read_ok = false;
    loop {
        match tolerate(driver.probe_frame(query), &mut fault)? {
            // A cross-origin frame is a hard stop, not a failed
            // expectation: the flow asked for something this engine
            // cannot honestly answer.
            Some(FrameProbe::CrossOrigin) => {
                return Err(ReplayError::Driver(flowproof_driver::DriverError::Browser(
                    flowproof_driver::frame_miss(&query.frame, &FrameProbe::CrossOrigin),
                )));
            }
            Some(FrameProbe::NoFrame { available }) => {
                read_ok = true;
                missing_frame = Some(available);
            }
            Some(FrameProbe::Ready { present, text }) => {
                read_ok = true;
                missing_frame = None;
                if let Some(want) = wanted_present {
                    if present == want {
                        return Ok((Ok(()), Some(rung)));
                    }
                } else if let (Some((_, negated)), Some(expected)) = (text_want, expected.as_ref())
                {
                    // A target that is not in the frame has no text to
                    // judge: an absent element must not satisfy a `does
                    // not show` assertion by accident.
                    if present && text_matches(expect, expected, negated, &text) {
                        return Ok((Ok(()), Some(rung)));
                    }
                    last = present.then_some(text);
                } else {
                    return Ok((
                        Err(format!("unsupported iframe expectation: {expect}")),
                        Some(rung),
                    ));
                }
            }
            None => {}
        }
        if Instant::now() >= deadline {
            if !read_ok {
                return Err(exhausted(fault));
            }
            if let Some(available) = missing_frame {
                return Ok((
                    Err(flowproof_driver::frame_miss(
                        &query.frame,
                        &FrameProbe::NoFrame { available },
                    )),
                    Some(rung),
                ));
            }
            let reason = match (wanted_present, text_want) {
                (Some(true), _) => format!(
                    "expected '{inner}' inside iframe '{}', but it never appeared there",
                    query.frame
                ),
                (Some(false), _) => format!(
                    "expected '{inner}' to be gone from iframe '{}', but it is still there",
                    query.frame
                ),
                (None, Some((raw, negated))) => {
                    let verb = if negated { "no text" } else { "text" };
                    match &last {
                        // Named as INSIDE the frame, so a passing-looking
                        // page element outside it cannot be mistaken for
                        // the thing that was read.
                        Some(text) => {
                            let shown = if flowproof_trace::secret::has_refs(raw) {
                                "<masked>"
                            } else {
                                text.as_str()
                            };
                            format!(
                                "expected {verb} '{raw}' inside iframe '{}', got '{shown}'",
                                query.frame
                            )
                        }
                        None => {
                            format!("'{inner}' was never found inside iframe '{}'", query.frame)
                        }
                    }
                }
                (None, None) => format!("unsupported iframe expectation: {expect}"),
            };
            return Ok((Err(reason), Some(rung)));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

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
    let mut fault: Option<flowproof_driver::DriverError> = None;
    let mut last_text: Option<String> = None;
    loop {
        // A `None` here is a transport fault: the app was never asked, so
        // nothing was learned about it. Keep polling within the budget.
        if let Some(text) = tolerate(read(), &mut fault)? {
            if text_matches(expect, &expected, negated, &text) {
                return Ok((Ok(()), rung));
            }
            last_text = Some(text);
        }
        if Instant::now() >= deadline {
            let Some(text) = last_text else {
                // The deadline expired without a single successful read:
                // this is an infrastructure failure, not a failed
                // expectation. Reporting it as "expected X, got ''" would
                // send a caller healing a trace that is not broken.
                return Err(exhausted(fault));
            };
            // A count assertion fails on the NUMBER of occurrences, so the
            // number found is the one fact that fixes the step. Dumping the
            // surface text instead buries it in a page-sized haystack, which
            // is exactly what makes an off-by-one count unfixable from CI
            // output alone.
            if let Some(n) = expect.get("count").and_then(|v| v.as_u64()) {
                let found = flowproof_driver::text_occurrences(&expected, &text);
                return Ok((
                    Err(format!("expected text '{raw}' {n} times, found {found}")),
                    rung,
                ));
            }
            let shown = if flowproof_trace::secret::has_refs(raw) {
                "<masked>"
            } else {
                text.as_str()
            };
            if let Some(want_empty) = expect.get("value_empty").and_then(|v| v.as_bool()) {
                let msg = if want_empty {
                    format!("expected the target to be empty, but it shows '{shown}'")
                } else {
                    "expected the target to be non-empty, but it is empty".to_string()
                };
                return Ok((Err(msg), rung));
            }
            let verb = if negated { "no text" } else { "text" };
            return Ok((Err(format!("expected {verb} '{raw}', got '{shown}'")), rung));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `page url` expectation carried by an `element_state`, if any:
/// `(expected, exact)` where exact distinguishes `is` from `contains`.
fn url_expectation(expect: &serde_json::Value) -> Option<(&str, bool)> {
    expect
        .get("url_equals")
        .and_then(|v| v.as_str())
        .map(|e| (e, true))
        .or_else(|| {
            expect
                .get("url_contains")
                .and_then(|v| v.as_str())
                .map(|e| (e, false))
        })
}

/// Poll the surface's URL until the expectation holds or `deadline` passes.
/// Mirrors [`check_text_expectation`]: transport faults are misses, a
/// budget that expires with no reading at all is a driver error, and the
/// message keeps the RAW expectation so a `${VAR}` never leaks.
/// `{"cookie": "<name>", "cookie_fact": "..."}` - the cookie assertions.
fn cookie_expectation(expect: &serde_json::Value) -> Option<(&str, &str)> {
    let name = expect.get("cookie").and_then(|v| v.as_str())?;
    let fact = expect
        .get("cookie_fact")
        .and_then(|v| v.as_str())
        .unwrap_or("exists");
    Some((name, fact))
}

/// Judge a cookie assertion. Auto-waits like the url pair: a cookie lands
/// with a login RESPONSE, which races the navigation that preceded it.
/// Value-free throughout - the probe carries no value to compare or print.
fn check_cookie_expectation<D: AppDriver>(
    driver: &mut D,
    name: &str,
    fact: &str,
    deadline: Instant,
) -> Result<(Result<(), String>, Option<usize>), ReplayError> {
    let mut fault: Option<flowproof_driver::DriverError> = None;
    let mut last: Option<String> = None;
    loop {
        if let Some(probe) = tolerate(driver.probe_cookie(name), &mut fault)? {
            match flowproof_driver::cookie_verdict(name, fact, &probe) {
                Ok(()) => {
                    // Passing `is secure` over plain http certifies nothing:
                    // browsers exempt localhost. The step passes, and the run
                    // says why it may not mean what it looks like.
                    if let Some(url) = tolerate(driver.current_url(), &mut fault)? {
                        if let Some(warning) =
                            flowproof_driver::secure_over_http_warning(fact, &url)
                        {
                            eprintln!("{warning}");
                        }
                    }
                    return Ok((Ok(()), None));
                }
                Err(reason) => last = Some(reason),
            }
        }
        if Instant::now() >= deadline {
            let Some(reason) = last else {
                return Err(exhausted(fault));
            };
            return Ok((Err(reason), None));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// `title_equals` / `title_contains`, the document-title siblings of the url
/// pair. `true` means exact.
fn title_expectation(expect: &serde_json::Value) -> Option<(&str, bool)> {
    expect
        .get("title_equals")
        .and_then(|v| v.as_str())
        .map(|e| (e, true))
        .or_else(|| {
            expect
                .get("title_contains")
                .and_then(|v| v.as_str())
                .map(|e| (e, false))
        })
}

/// Judge a page-title assertion. Auto-waits like the url pair, and for the
/// same reason: an SPA sets `document.title` after the route commits, so a
/// single read would be racy by construction. An empty title is reported as
/// empty rather than as a bare '' the reader has to decode.
fn check_title_expectation<F>(
    raw: &str,
    exact: bool,
    deadline: Instant,
    mut read: F,
) -> Result<(Result<(), String>, Option<usize>), ReplayError>
where
    F: FnMut() -> Result<String, flowproof_driver::DriverError>,
{
    let expected = match flowproof_trace::secret::resolve_refs(raw) {
        Ok(expected) => expected,
        Err(e) => return Ok((Err(e.to_string()), None)),
    };
    let mut fault: Option<flowproof_driver::DriverError> = None;
    let mut last: Option<String> = None;
    loop {
        if let Some(title) = tolerate(read(), &mut fault)? {
            let hit = if exact {
                title.trim() == expected.trim()
            } else {
                flowproof_driver::text_contains(&title, &expected)
            };
            if hit {
                return Ok((Ok(()), None));
            }
            last = Some(title);
        }
        if Instant::now() >= deadline {
            let Some(title) = last else {
                return Err(exhausted(fault));
            };
            let verb = if exact {
                "page title"
            } else {
                "page title containing"
            };
            if title.trim().is_empty() {
                return Ok((
                    Err(format!(
                        "expected {verb} '{raw}', but the page title is empty"
                    )),
                    None,
                ));
            }
            let shown = if flowproof_trace::secret::has_refs(raw) {
                "<masked>".to_string()
            } else {
                title
            };
            return Ok((Err(format!("expected {verb} '{raw}', got '{shown}'")), None));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

fn check_url_expectation<F>(
    expect: &serde_json::Value,
    raw: &str,
    exact: bool,
    deadline: Instant,
    mut read: F,
) -> Result<(Result<(), String>, Option<usize>), ReplayError>
where
    F: FnMut() -> Result<String, flowproof_driver::DriverError>,
{
    let _ = expect;
    let expected = match flowproof_trace::secret::resolve_refs(raw) {
        Ok(expected) => expected,
        Err(e) => return Ok((Err(e.to_string()), None)),
    };
    let mut fault: Option<flowproof_driver::DriverError> = None;
    let mut last: Option<String> = None;
    loop {
        if let Some(url) = tolerate(read(), &mut fault)? {
            if flowproof_driver::url_matches(&expected, exact, &url) {
                return Ok((Ok(()), None));
            }
            last = Some(url);
        }
        if Instant::now() >= deadline {
            let Some(url) = last else {
                return Err(exhausted(fault));
            };
            let shown = if flowproof_trace::secret::has_refs(raw) {
                "<masked>".to_string()
            } else {
                url
            };
            let verb = if exact { "url" } else { "url containing" };
            return Ok((Err(format!("expected {verb} '{raw}', got '{shown}'")), None));
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One reading inside an auto-wait poll loop. A [`DriverError::Transport`]
/// fault is a MISS (`Ok(None)`) rather than an error: the assertion's
/// contract is "this holds within N seconds", and a dead socket on one
/// poll is an observation about the harness, not about the app. Every
/// other driver error still propagates and fails the step.
fn tolerate<T>(
    result: Result<T, flowproof_driver::DriverError>,
    fault: &mut Option<flowproof_driver::DriverError>,
) -> Result<Option<T>, ReplayError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(e) if e.is_transient() => {
            *fault = Some(e);
            Ok(None)
        }
        Err(e) => Err(e.into()),
    }
}

/// The wait budget expired with no successful reading at all - surface the
/// transport fault that kept eating the polls.
fn exhausted(fault: Option<flowproof_driver::DriverError>) -> ReplayError {
    match fault {
        Some(e) => ReplayError::Driver(e),
        // Unreachable in practice: a loop only ends with no reading when a
        // fault was tolerated. Kept total rather than panicking.
        None => ReplayError::Driver(flowproof_driver::DriverError::Transport(
            "the assertion never completed a reading".into(),
        )),
    }
}

fn check_assertion<D: AppDriver>(
    driver: &mut D,
    assertion: &Assertion,
    selectors: &[Selector],
    captures: &std::collections::HashMap<String, String>,
    api_corpus: &mut Vec<(String, String)>,
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
            let resolve = |driver: &mut D,
                           fault: &mut Option<flowproof_driver::DriverError>|
             -> Result<Option<(UiaSelector, usize)>, ReplayError> {
                let order =
                    std::iter::once(primary).chain((0..selectors.len()).filter(|&i| i != primary));
                for rung in order {
                    let Some(uia) = selectors.get(rung).and_then(selector_to_uia) else {
                        continue;
                    };
                    // A transport fault here is a miss, not an answer: the
                    // rung was never actually tested against the app.
                    if tolerate(driver.element_exists(&uia), fault)?.unwrap_or(false) {
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
            // `page url is|contains` reads the surface's LOCATION rather
            // than its text. Same poll, same recorded bound: an SPA
            // redirect lands asynchronously, so this must auto-wait like
            // everything else.
            if let Some((raw, exact)) = url_expectation(expect) {
                return check_url_expectation(expect, raw, exact, deadline, || {
                    driver.current_url()
                });
            }
            // A cookie is a fact about the surface, not a reading of its
            // text: same poll, same recorded bound.
            if let Some((name, fact)) = cookie_expectation(expect) {
                let (name, fact) = (name.to_string(), fact.to_string());
                return check_cookie_expectation(driver, &name, &fact, deadline);
            }
            // The title is another reading of the same surface, on the same
            // poll and the same recorded bound.
            if let Some((raw, exact)) = title_expectation(expect) {
                return check_title_expectation(raw, exact, deadline, || driver.page_title());
            }
            if expect.get("scope").and_then(|v| v.as_str()) == Some("surface") {
                return check_text_expectation(expect, deadline, None, || driver.surface_text());
            }

            // Framed: the target lives in a same-origin iframe, which is a
            // separate document. It gets its own reader rather than the
            // ordinary ladder, and the frame is a HARD FENCE - a miss
            // inside the frame never falls back to a same-named element on
            // the page outside it.
            if let Some(query) = selectors
                .get(primary)
                .and_then(selector_to_uia)
                .and_then(|uia| uia.frame.clone())
            {
                return check_framed_expectation(driver, expect, &query, deadline, primary);
            }

            // Count expectations: how MANY match, which the ordinal every
            // adapter already implements can answer without a new driver
            // capability. `wanted + 1` questions decide it; counting
            // further is paid only to describe a failure.
            if let Some(wanted) = expect.get("element_count").and_then(|v| v.as_u64()) {
                let wanted = wanted as usize;
                let mut last = Err(String::new());
                // Did the app ever actually answer? If every poll was a
                // transport fault, nothing was learned and the run errors
                // rather than reporting a count it never saw.
                let mut read_ok = false;
                loop {
                    let mut fault: Option<flowproof_driver::DriverError> = None;
                    let uia = selectors
                        .get(primary)
                        .and_then(selector_to_uia)
                        .ok_or_else(|| {
                            ReplayError::UnknownApp("count without a selector".into())
                        })?;
                    match tolerate(
                        flowproof_driver::count_matching(driver, &uia, wanted + 1),
                        &mut fault,
                    )? {
                        Some(found) if found == wanted => {
                            return Ok((Ok(()), Some(primary)));
                        }
                        Some(found) => {
                            read_ok = true;
                            last = Err(format!(
                                "expected {wanted} matching elements, found {found}"
                            ));
                        }
                        // A transport fault says nothing about the app, so
                        // it is a miss inside the budget, not an answer.
                        None => {}
                    }
                    if Instant::now() >= deadline {
                        if !read_ok {
                            return Err(exhausted(fault));
                        }
                        // Count further now, so the failure names what was
                        // there rather than only what was not.
                        let mut ignored = None;
                        if let Some(actual) = tolerate(
                            flowproof_driver::count_matching(
                                driver,
                                &uia,
                                flowproof_driver::COUNT_DIAGNOSTIC_CAP,
                            ),
                            &mut ignored,
                        )? {
                            let actual = if actual >= flowproof_driver::COUNT_DIAGNOSTIC_CAP {
                                format!("{actual} or more")
                            } else {
                                actual.to_string()
                            };
                            last = Err(format!(
                                "expected {wanted} matching elements, found {actual}"
                            ));
                        }
                        return Ok((last, Some(primary)));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Presence expectations: the element being there (or gone) IS
            // the assertion — no text involved.
            if let Some(wanted_present) = expect.get("element_present").and_then(|v| v.as_bool()) {
                let mut read_ok = false;
                // Whether the last completed poll found the element in the
                // DOM but not rendered - the difference between "your
                // selector is wrong" and "your app never showed it".
                let mut hidden = false;
                loop {
                    // Scoped per iteration: only THIS poll's fault decides
                    // whether "gone" was actually observed.
                    let mut fault: Option<flowproof_driver::DriverError> = None;
                    let resolved = resolve(driver, &mut fault)?;
                    // Resolving is only half of `is visible`: a hidden
                    // element answers every selector, so a presence-only
                    // check makes the assertion unfailable. A surface with
                    // no notion of rendered-ness keeps the presence answer.
                    let visible = match &resolved {
                        Some((uia, _)) => tolerate(driver.element_visible(uia), &mut fault)?
                            .map(|v| v.unwrap_or(true)),
                        None => Some(false),
                    };
                    read_ok |= fault.is_none();
                    if fault.is_none() {
                        hidden = resolved.is_some() && visible == Some(false);
                    }
                    match (&resolved, visible, wanted_present) {
                        (Some((_, rung)), Some(true), true) => {
                            return Ok((Ok(()), Some(*rung)));
                        }
                        // "gone" must be proven by a reading that happened,
                        // not by a fault that prevented one. Rendered-ness
                        // counts: a `display:none` element IS gone to the
                        // user, which is what the assertion is about.
                        (_, Some(false), false) if fault.is_none() => {
                            return Ok((Ok(()), resolved.map(|(_, rung)| rung)));
                        }
                        _ => {}
                    }
                    if Instant::now() >= deadline {
                        if !read_ok {
                            return Err(exhausted(fault));
                        }
                        let reason = if wanted_present && hidden {
                            "expected element to be visible, but it is present and not rendered"
                                .to_string()
                        } else if wanted_present {
                            "expected element to be visible, but it never appeared".to_string()
                        } else {
                            "expected element to be gone, but it is still on screen".to_string()
                        };
                        return Ok((Err(reason), resolved.map(|(_, rung)| rung)));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Computed assertion: compare against a value captured earlier
            // in THIS flow. The captured value lives only in memory - the
            // trace stores the name, never the number.
            if let Some(name) = expect.get("capture").and_then(|v| v.as_str()) {
                let offset = expect.get("offset").and_then(|v| v.as_f64());
                let Some(captured) = captures.get(name) else {
                    let mut names: Vec<&str> = captures.keys().map(String::as_str).collect();
                    names.sort_unstable();
                    let scope = if names.is_empty() {
                        "no captures in scope".to_string()
                    } else {
                        format!("in scope: {}", names.join(", "))
                    };
                    // An unknown capture is a spec error, not an app
                    // failure: the flow cannot be judged at all.
                    return Err(ReplayError::Driver(flowproof_driver::DriverError::Browser(
                        format!("capture '{name}' was never remembered ({scope})"),
                    )));
                };
                let mut last: Option<String> = None;
                let mut fault: Option<flowproof_driver::DriverError> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                        if let Some(text) = tolerate(driver.read_text(&uia), &mut fault)? {
                            match flowproof_driver::capture_matches(captured, offset, &text) {
                                Ok(true) => return Ok((Ok(()), Some(rung))),
                                Ok(false) => last = Some(text),
                                // A non-numeric side cannot become numeric
                                // by waiting: fail now, saying which side.
                                Err(why) => return Ok((Err(why), Some(rung))),
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        if last.is_none() && fault.is_some() {
                            return Err(exhausted(fault));
                        }
                        let wanted = match offset {
                            None => format!("capture '{name}' ('{captured}')"),
                            Some(o) => format!(
                                "capture '{name}' ('{captured}') {} {}",
                                if o < 0.0 { "-" } else { "+" },
                                o.abs()
                            ),
                        };
                        let shown = last.unwrap_or_else(|| "<element not found>".to_string());
                        return Ok((Err(format!("expected {wanted}, got '{shown}'")), None));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Checkbox state: same poll shape as enabled/disabled, but a
            // driver answer of None means "not a checkbox", which is a
            // different failure from "checked when it should not be" and
            // must be reported as such.
            if let Some(wanted) = expect.get("checked").and_then(|v| v.as_bool()) {
                let mut last: Option<Option<bool>> = None;
                let mut fault: Option<flowproof_driver::DriverError> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                        if let Some(seen) = tolerate(driver.element_checked(&uia), &mut fault)? {
                            if seen == Some(wanted) {
                                return Ok((Ok(()), Some(rung)));
                            }
                            last = Some(seen);
                        }
                    }
                    if Instant::now() >= deadline {
                        if last.is_none() && fault.is_some() {
                            return Err(exhausted(fault));
                        }
                        let state = |c: bool| if c { "checked" } else { "not checked" };
                        let shown = match last {
                            Some(Some(c)) => state(c).to_string(),
                            Some(None) => "not a checkbox".to_string(),
                            None => "<element not found>".to_string(),
                        };
                        return Ok((
                            Err(format!("expected checkbox {}, got {shown}", state(wanted))),
                            None,
                        ));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Enabled/disabled expectations: resolve the element, ask the
            // driver for its interactive state, poll until it matches.
            if let Some(wanted_enabled) = expect.get("enabled").and_then(|v| v.as_bool()) {
                let mut last: Option<bool> = None;
                let mut fault: Option<flowproof_driver::DriverError> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                        if let Some(enabled) = tolerate(driver.element_enabled(&uia), &mut fault)? {
                            if enabled == wanted_enabled {
                                return Ok((Ok(()), Some(rung)));
                            }
                            last = Some(enabled);
                        }
                    }
                    if Instant::now() >= deadline {
                        if last.is_none() && fault.is_some() {
                            return Err(exhausted(fault));
                        }
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

            // Attribute assertion: EXACT, case-sensitive value comparison
            // (no text ladder) or presence. Web-only at execution; a non-web
            // adapter's `element_attribute` refuses with a reason. Checked
            // before the text branch because it also uses `value_equals`.
            if let Some(name) = expect.get("attribute").and_then(|v| v.as_str()) {
                let present = expect.get("present").and_then(|v| v.as_bool());
                let value_raw = expect.get("value_equals").and_then(|v| v.as_str());
                let negate = expect.get("negate").and_then(|v| v.as_bool()) == Some(true);
                // A value form may carry a `${VAR}` secret: resolve for the
                // comparison only, keep the raw reference in messages.
                let wanted = match value_raw {
                    Some(raw) => match flowproof_trace::secret::resolve_refs(raw) {
                        Ok(v) => Some(v),
                        Err(e) => return Ok((Err(e.to_string()), None)),
                    },
                    None => None,
                };
                let mut last: Option<Option<String>> = None;
                let mut fault: Option<flowproof_driver::DriverError> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                        if let Some(attr) =
                            tolerate(driver.element_attribute(&uia, name), &mut fault)?
                        {
                            let holds = match present {
                                Some(want) => attr.is_some() == want,
                                None => flowproof_driver::attribute_value_matches(
                                    wanted.as_deref().unwrap_or_default(),
                                    negate,
                                    attr.as_deref(),
                                ),
                            };
                            if holds {
                                return Ok((Ok(()), Some(rung)));
                            }
                            last = Some(attr);
                        }
                    }
                    if Instant::now() >= deadline {
                        if last.is_none() && fault.is_some() {
                            return Err(exhausted(fault));
                        }
                        let word = |p: bool| if p { "present" } else { "absent" };
                        let msg = match (present, &last) {
                            (Some(want), Some(attr)) => format!(
                                "expected attribute `{name}` {}, got {}",
                                word(want),
                                word(attr.is_some())
                            ),
                            (None, Some(attr)) => {
                                let raw = value_raw.unwrap_or_default();
                                let want_phrase = if negate {
                                    format!("attribute `{name}` not '{raw}'")
                                } else {
                                    format!("attribute `{name}` = '{raw}'")
                                };
                                let got = match attr {
                                    None => format!("the element has no `{name}` attribute"),
                                    Some(v) => {
                                        let shown = if flowproof_trace::secret::has_refs(raw) {
                                            "<masked>".to_string()
                                        } else {
                                            format!("'{v}'")
                                        };
                                        format!("attribute `{name}` = {shown}")
                                    }
                                };
                                format!("expected {want_phrase}, got {got}")
                            }
                            (_, None) => {
                                format!(
                                    "expected attribute `{name}`, but the element was not found"
                                )
                            }
                        };
                        return Ok((Err(msg), None));
                    }
                    std::thread::sleep(POLL_INTERVAL);
                }
            }

            // Computed-style assertion: colors compare canonically, keywords
            // case-insensitively (see `style_matches`, shared with record).
            // Web-only at execution.
            if let Some(prop) = expect.get("style").and_then(|v| v.as_str()) {
                let raw = expect
                    .get("value_equals")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let negate = expect.get("negate").and_then(|v| v.as_bool()) == Some(true);
                let wanted = match flowproof_trace::secret::resolve_refs(raw) {
                    Ok(v) => v,
                    Err(e) => return Ok((Err(e.to_string()), None)),
                };
                let mut last: Option<String> = None;
                let mut fault: Option<flowproof_driver::DriverError> = None;
                loop {
                    if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                        if let Some(actual) =
                            tolerate(driver.element_computed_style(&uia, prop), &mut fault)?
                        {
                            match flowproof_driver::style_matches(prop, &wanted, negate, &actual) {
                                Ok(true) => return Ok((Ok(()), Some(rung))),
                                Ok(false) => last = Some(actual),
                                // Unparseable computed / non-color expected:
                                // waiting cannot fix it, so fail now.
                                Err(why) => return Ok((Err(why), Some(rung))),
                            }
                        }
                    }
                    if Instant::now() >= deadline {
                        if last.is_none() && fault.is_some() {
                            return Err(exhausted(fault));
                        }
                        let want = if negate {
                            format!("style {prop} is not '{raw}'")
                        } else {
                            format!("style {prop} is '{raw}'")
                        };
                        let shown = last
                            .map(|s| format!("computed {prop} '{s}'"))
                            .unwrap_or_else(|| "<element not found>".to_string());
                        return Ok((Err(format!("expected {want}, got {shown}")), None));
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
            let mut fault: Option<flowproof_driver::DriverError> = None;
            loop {
                if let Some((uia, rung)) = resolve(driver, &mut fault)? {
                    if let Some(text) = tolerate(driver.read_text(&uia), &mut fault)? {
                        if text_matches(expect, &expected, negated, &text) {
                            return Ok((Ok(()), Some(rung)));
                        }
                        last = Some((text, rung));
                    }
                }
                if Instant::now() >= deadline {
                    if last.is_none() && fault.is_some() {
                        return Err(exhausted(fault));
                    }
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
            let (verdict, rung, _) = poll_oob(&probe, oob_timeout(expect.as_ref()))?;
            Ok((verdict, rung))
        }
        Assertion::Api {
            request,
            status,
            expect,
        } => {
            let probe = flowproof_driver::oob::OobProbe::Api {
                count: expect
                    .as_ref()
                    .and_then(|e| e.get("count"))
                    .and_then(|v| v.as_u64())
                    .map(flowproof_driver::oob::ArrayCount::Exactly)
                    .or_else(|| {
                        expect
                            .as_ref()
                            .and_then(|e| e.get("count_at_least"))
                            .and_then(|v| v.as_u64())
                            .map(flowproof_driver::oob::ArrayCount::AtLeast)
                    }),
                retry: expect
                    .as_ref()
                    .and_then(|e| e.get("retry"))
                    .and_then(|v| v.as_bool()),
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
                body_json: expect
                    .as_ref()
                    .and_then(|e| e.get("body_json"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                // A `${VAR}` in `equals` resolves here at probe time, exactly
                // like `body_contains`: the trace carries only the raw ref.
                equals: match expect.as_ref().and_then(|e| e.get("equals")) {
                    Some(serde_json::Value::String(s)) => Some(serde_json::Value::String(
                        flowproof_trace::secret::resolve_refs(s)?,
                    )),
                    Some(other) => Some(other.clone()),
                    None => None,
                },
                // The header name is literal; a `${VAR}` in the value predicate
                // resolves here at probe time, exactly like `body_contains`.
                header: expect
                    .as_ref()
                    .and_then(|e| e.get("header"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                header_equals: match expect
                    .as_ref()
                    .and_then(|e| e.get("header_equals"))
                    .and_then(|v| v.as_str())
                {
                    Some(want) => Some(flowproof_trace::secret::resolve_refs(want)?),
                    None => None,
                },
                header_contains: match expect
                    .as_ref()
                    .and_then(|e| e.get("header_contains"))
                    .and_then(|v| v.as_str())
                {
                    Some(want) => Some(flowproof_trace::secret::resolve_refs(want)?),
                    None => None,
                },
            };
            let (verdict, rung, body) = poll_oob(&probe, oob_timeout(expect.as_ref()))?;
            // The response body joins the corpus a secret-leak scan reads, held
            // in memory for this run only and re-observed identically at record.
            if let Some(text) = body {
                api_corpus.push(("an assert_api response body".to_string(), text));
            }
            Ok((verdict, rung))
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

/// An out-of-band probe's outcome: the assertion verdict, the selector rung
/// it matched (always `None` for OOB probes), and the probe's response body
/// (an `Api` probe's response text, the corpus a secret-leak scan reads;
/// `None` otherwise).
type ProbeOutcome = (Result<(), String>, Option<usize>, Option<String>);

/// Auto-wait an out-of-band probe like any other assertion.
fn poll_oob(
    probe: &flowproof_driver::oob::OobProbe,
    timeout_ms: u64,
) -> Result<ProbeOutcome, ReplayError> {
    // A mutation is sent ONCE: re-firing it to wait for convergence would
    // deliver the write again on every tick (see oob::is_retryable).
    let retryable = flowproof_driver::oob::is_retryable(probe);
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match flowproof_driver::oob::check(probe)? {
            Ok(body) => return Ok((Ok(()), None, body)),
            Err(reason) => {
                if !retryable {
                    let reason = format!("{reason} ({})", flowproof_driver::oob::RETRY_HINT);
                    return Ok((Err(reason), None, None));
                }
                if Instant::now() >= deadline {
                    return Ok((Err(reason), None, None));
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

/// Filesystem context for `assert_screenshot`: where baselines live
/// (next to the trace) and where failure artifacts go (the run dir).
struct VisualPaths {
    baselines: std::path::PathBuf,
    run_dir: std::path::PathBuf,
}

/// Compare the live (masked) surface against the recorded baseline.
/// Outer Err = execution failure; inner Err = the assertion verdict.
fn check_visual<D: AppDriver>(
    driver: &mut D,
    baseline_file: &str,
    threshold: Option<f64>,
    masks: &[String],
    paths: &VisualPaths,
) -> Result<Result<(), String>, ReplayError> {
    use flowproof_driver::visual;
    let Some(mut frame) = driver.capture()? else {
        return Ok(Err(
            "assert_screenshot needs a driver that can capture frames".into(),
        ));
    };
    let mut rects = Vec::with_capacity(masks.len());
    for mask in masks {
        match driver.element_rect(&visual::mask_selector(mask))? {
            Some(rect) => rects.push(rect),
            None => {
                return Ok(Err(format!(
                    "assert_screenshot mask '{mask}' does not resolve to an element"
                )))
            }
        }
    }
    visual::apply_masks(&mut frame, &rects);
    let name = baseline_file
        .strip_suffix(".png")
        .unwrap_or(baseline_file)
        .to_string();
    let baseline = match visual::load_baseline(&paths.baselines, &name) {
        Ok(b) => b,
        Err(e) => return Ok(Err(e)),
    };
    let Some(result) = visual::compare(&baseline, &frame) else {
        return Ok(Err(format!(
            "screenshot is {}x{} but baseline '{name}' is {}x{} — \
             viewport changed? re-record to refresh the baseline",
            frame.width(),
            frame.height(),
            baseline.width(),
            baseline.height(),
        )));
    };
    let allowed = threshold.unwrap_or(0.0);
    if result.differing_fraction() <= allowed {
        return Ok(Ok(()));
    }
    // Reviewable failure artifacts beside the report: what we saw, and
    // where it differs.
    let dir = paths.run_dir.join("visual");
    let _ = visual::save_png(&dir.join(format!("{name}.actual.png")), &frame);
    let _ = visual::save_png(
        &dir.join(format!("{name}.diff.png")),
        &visual::diff_image(&baseline, &frame),
    );
    Ok(Err(format!(
        "visual diff {:.3}% exceeds allowed {:.3}% for baseline '{name}' \
         (see visual/{name}.diff.png)",
        result.differing_fraction() * 100.0,
        allowed * 100.0,
    )))
}

/// Build the execution-time dialog arm from a recorded trace dialog,
/// resolving any `${VAR}` in the prompt reply NOW (record and replay alike,
/// exactly like `TypeText.text`), so only the reference ever lived in the
/// trace. Mirrors the record-side `flowproof_agent::rules::dialog_arm`.
fn dialog_arm(
    dialog: &flowproof_trace::format::Dialog,
) -> Result<flowproof_driver::DialogArm, flowproof_trace::secret::MissingSecret> {
    use flowproof_trace::format::DialogDisposition as D;
    let reply = match &dialog.reply {
        Some(reply) => Some(flowproof_trace::secret::resolve_refs(reply)?),
        None => None,
    };
    Ok(flowproof_driver::DialogArm {
        disposition: match dialog.disposition {
            D::Accept => flowproof_driver::DialogDisposition::Accept,
            D::Dismiss => flowproof_driver::DialogDisposition::Dismiss,
        },
        message: dialog.message.clone(),
        reply,
    })
}

/// Decode a folded-in dialog out of a trigger action's params bag. A bag
/// with no `dialog` key yields `None` - the common case, and what keeps an
/// old trace replaying unchanged.
fn dialog_from_params(
    params: &flowproof_trace::format::Params,
) -> Option<flowproof_trace::format::Dialog> {
    params
        .get("dialog")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
}

/// Arm a folded-in dialog, dispatch the trigger, then verify the DECLARED
/// dialog fired as recorded - the replay-side mirror of the record path. An
/// unresolved `${VAR}` in the reply, or a declared dialog that did not open
/// or was handled differently, is a step FAILURE (`Ok(Err(reason))`), not a
/// hard error. With no dialog this is a plain dispatch.
fn dispatch_with_dialog<D: AppDriver>(
    driver: &mut D,
    dialog: Option<&flowproof_trace::format::Dialog>,
    dispatch: impl FnOnce(&mut D) -> Result<(), flowproof_driver::DriverError>,
) -> Result<Result<(), String>, ReplayError> {
    let arm = match dialog {
        Some(dialog) => match dialog_arm(dialog) {
            Ok(arm) => {
                driver.arm_dialog(arm.clone())?;
                Some(arm)
            }
            Err(missing) => return Ok(Err(missing.to_string())),
        },
        None => None,
    };
    dispatch(driver)?;
    if let Some(arm) = arm {
        let fired = driver.take_fired_dialog();
        if let Err(reason) = flowproof_driver::verify_dialog(&arm, fired.as_ref()) {
            return Ok(Err(reason));
        }
    }
    Ok(Ok(()))
}

fn execute_step<D: AppDriver>(
    driver: &mut D,
    step: &Step,
    base_url: &str,
    visual_paths: &VisualPaths,
    captures: &mut std::collections::HashMap<String, String>,
    api_corpus: &mut Vec<(String, String)>,
    mut recorder: Option<&mut flowproof_driver::RunRecorder>,
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
        Action::Click(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        let dialog = dialog_from_params(params);
                        // An offset click carries where inside the box to
                        // hit; without it, the midpoint, as before.
                        let at = params
                            .get("x_pct")
                            .and_then(|v| v.as_f64())
                            .zip(params.get("y_pct").and_then(|v| v.as_f64()));
                        let (x_pct, y_pct) = at.unwrap_or((50.0, 50.0));
                        pointer_checkpoint(&mut recorder, driver, &target, x_pct, y_pct);
                        let outcome =
                            dispatch_with_dialog(driver, dialog.as_ref(), |d| match at {
                                Some((x, y)) => d.click_at(&target, x, y),
                                None => d.invoke(&target),
                            })?;
                        (outcome, matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        Action::Capture(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                let Some(name) = params.get("name").and_then(|v| v.as_str()) else {
                    return Ok((Err("capture step has no name".into()), matched));
                };
                // Read at execution time, on replay exactly as on record -
                // which is why the value never needs to be in the trace.
                // `count` picks the reading; the indirection is the same.
                let value = if params.get("count").and_then(|v| v.as_bool()) == Some(true) {
                    let found = flowproof_driver::count_matching(
                        driver,
                        &target,
                        flowproof_driver::COUNT_DIAGNOSTIC_CAP,
                    )?;
                    // Unreachable while the target resolved, since a
                    // resolved element is a first match - but stated
                    // rather than assumed, because "0" is the one value
                    // this capture must never quietly hand on.
                    if found == 0 {
                        return Ok((
                            Err(
                                "nothing matched, so the count would be a guess - to assert \
                                 emptiness use 'the \"<target>\" appears 0 times'"
                                    .to_string(),
                            ),
                            matched,
                        ));
                    }
                    found.to_string()
                } else {
                    driver.read_text(&target)?
                };
                captures.insert(name.to_string(), value);
                (Ok(()), matched)
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        Action::SetChecked(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                if let Err(reason) = wait_actionable(driver, &target, actionable_timeout(step))? {
                    return Ok((Err(reason), matched));
                }
                let checked = params
                    .get("checked")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true);
                // Set-state: the driver no-ops when the control already
                // reads the wanted value, and verifies that it took.
                match driver.set_checked(&target, checked) {
                    Ok(()) => (Ok(()), matched),
                    Err(e) => (Err(e.to_string()), matched),
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
                // A multi-selection is one commit of a whole set, so it is
                // decided before the single-value path: `values` is
                // authoritative wherever it is present, and `text` carries
                // only the first option for a reader that shows text.
                if let Some(values) = params.extra.get("values").and_then(|v| v.as_array()) {
                    let wanted: Vec<String> = values
                        .iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect();
                    return Ok((
                        match driver.select_options(&target, &wanted) {
                            Ok(()) => Ok(()),
                            Err(e) => Err(e.to_string()),
                        },
                        matched,
                    ));
                }
                // The trace stores references, never values. A
                // `${captured.x}` resolves from this run's captures and a
                // `${VAR}` from the environment, both at the moment of
                // typing. Captures first: a `${VAR}` name may not contain a
                // dot, so the secret resolver passes a capture reference
                // through as literal text.
                let typed = match flowproof_trace::captures::substitute(&params.text, captures) {
                    Ok(text) => text,
                    Err(reason) => return Ok((Err(reason), matched)),
                };
                match flowproof_trace::secret::resolve_refs(&typed) {
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
        Action::RightClick(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        pointer_checkpoint(&mut recorder, driver, &target, 50.0, 50.0);
                        let dialog = dialog_from_params(params);
                        let outcome = dispatch_with_dialog(driver, dialog.as_ref(), |d| {
                            d.context_click(&target)
                        })?;
                        (outcome, matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        Action::DoubleClick(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        pointer_checkpoint(&mut recorder, driver, &target, 50.0, 50.0);
                        let dialog = dialog_from_params(params);
                        let outcome = dispatch_with_dialog(driver, dialog.as_ref(), |d| {
                            d.double_click(&target)
                        })?;
                        (outcome, matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        // Hover: the driver moves the pointer onto the element ONCE; the
        // engine synthesizes no pointer movement between steps, so the
        // hover state persists until the author's next explicit pointer
        // action.
        Action::Hover(params) => match resolve_target(driver, &step.selectors)? {
            Some((target, rung)) => {
                let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                match wait_actionable(driver, &target, actionable_timeout(step))? {
                    Ok(()) => {
                        pointer_checkpoint(&mut recorder, driver, &target, 50.0, 50.0);
                        let dialog = dialog_from_params(params);
                        let outcome =
                            dispatch_with_dialog(driver, dialog.as_ref(), |d| d.hover(&target))?;
                        (outcome, matched)
                    }
                    Err(reason) => (Err(reason), matched),
                }
            }
            None => (
                Err("no selector rung resolved to a live element".to_string()),
                StepMatch::default(),
            ),
        },
        // Drag: BOTH ends resolve through the ordinary ladder, and the drop
        // target gets the same actionability wait the source does. A drag
        // onto something not yet actionable is the flake this mechanism was
        // deferred over; waiting for it is cheaper than re-running.
        Action::Drag(params) => {
            let onto: Vec<flowproof_trace::format::Selector> = params
                .get("onto")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            match (
                resolve_target(driver, &step.selectors)?,
                resolve_target(driver, &onto)?,
            ) {
                (Some((from, rung)), Some((to, _))) => {
                    let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                    let timeout = actionable_timeout(step);
                    match (
                        wait_actionable(driver, &from, timeout)?,
                        wait_actionable(driver, &to, timeout)?,
                    ) {
                        (Ok(()), Ok(())) => {
                            pointer_checkpoint(&mut recorder, driver, &from, 50.0, 50.0);
                            let outcome = driver.drag(&from, &to);
                            if outcome.is_ok() {
                                pointer_checkpoint(&mut recorder, driver, &to, 50.0, 50.0);
                            }
                            (outcome.map_err(|e| e.to_string()), matched)
                        }
                        (Err(reason), _) | (_, Err(reason)) => (Err(reason), matched),
                    }
                }
                (None, _) => (
                    Err("no selector rung resolved the drag source".to_string()),
                    StepMatch::default(),
                ),
                (_, None) => (
                    Err("no selector rung resolved the drop target".to_string()),
                    StepMatch::default(),
                ),
            }
        }
        // Scroll a container/element/page. An empty selector list is a page
        // scroll (`Scroll to the bottom`); otherwise the target scrolls (as a
        // container for top/bottom, or into the viewport). Instant, no
        // settle-wait: the driver verifies it took, and the next assertion
        // auto-waits.
        Action::Scroll(params) => {
            let to = if params.get("into_view").and_then(|v| v.as_bool()) == Some(true) {
                Some(flowproof_driver::ScrollTo::IntoView)
            } else if let Some(px) = params.get("to_px").and_then(|v| v.as_u64()) {
                Some(flowproof_driver::ScrollTo::Offset(px as u32))
            } else {
                match params.get("to").and_then(|v| v.as_str()) {
                    Some("bottom") => Some(flowproof_driver::ScrollTo::Bottom),
                    Some("top") => Some(flowproof_driver::ScrollTo::Top),
                    _ => None,
                }
            };
            match to {
                None => (
                    Err("scroll step has neither `to` nor `into_view`".to_string()),
                    StepMatch::default(),
                ),
                Some(to) if step.selectors.is_empty() => {
                    driver.scroll(None, to)?;
                    (Ok(()), StepMatch::default())
                }
                Some(to) => match resolve_target(driver, &step.selectors)? {
                    Some((target, rung)) => {
                        let matched = StepMatch::from_rung(&step.selectors, Some(rung), 0);
                        match driver.scroll(Some(&target), to) {
                            Ok(()) => (Ok(()), matched),
                            Err(e) => (Err(e.to_string()), matched),
                        }
                    }
                    None => (
                        Err("no selector rung resolved to a live element".to_string()),
                        StepMatch::default(),
                    ),
                },
            }
        }
        // Visual assertions need filesystem context the generic checker
        // doesn't have (baselines + run artifacts) — handled here.
        Action::Assert(Assertion::VisualDiff {
            baseline,
            threshold,
            masks,
            region: _,
        }) => (
            check_visual(driver, baseline, *threshold, masks, visual_paths)?,
            StepMatch::default(),
        ),
        Action::Assert(assertion) => {
            let (outcome, rung) =
                check_assertion(driver, assertion, &step.selectors, captures, api_corpus)?;
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
    // Safety net: an UNDECLARED dialog was dismissed by the flow-wide
    // listener and fails this step deterministically, rather than hanging on
    // an unanswered dialog. A step that already failed keeps its own reason.
    if outcome.is_ok() {
        if let Some(unexpected) = driver.take_unexpected_dialog() {
            return Ok((
                Err(format!(
                    "an unexpected dialog opened: {}",
                    unexpected.message
                )),
                matched,
            ));
        }
    }
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

fn pointer_checkpoint<D: AppDriver>(
    recorder: &mut Option<&mut flowproof_driver::RunRecorder>,
    driver: &mut D,
    selector: &flowproof_driver::UiaSelector,
    x_pct: f64,
    y_pct: f64,
) {
    if let Some(recorder) = recorder.as_deref_mut() {
        recorder.pointer_event(driver, selector, x_pct, y_pct);
    }
}

/// Replay the trace at `path` against the live application. Deterministic:
/// walks recorded selectors only, stops at the first failing step. Creates
/// the run's self-contained artifact directory up front so the recording
/// bundle and the reports land together; returns it alongside the report.
pub fn run_trace<D: AppDriver>(
    path: &Path,
    driver: &mut D,
) -> Result<(RunReport, std::path::PathBuf), ReplayError> {
    run_trace_with_options(path, driver, flowproof_driver::RecordingOptions::default())
}

/// Replay with explicit visual-recording controls.
pub fn run_trace_with_options<D: AppDriver>(
    path: &Path,
    driver: &mut D,
    recording: flowproof_driver::RecordingOptions,
) -> Result<(RunReport, std::path::PathBuf), ReplayError> {
    run_trace_with_secret_scan_and_options(path, driver, &SecretScan::disabled(), recording)
}

/// Replay the trace, additionally running the flow's `assert_no_secret_leak`
/// scan against the corpus this replay re-observes (web surface text at each
/// step boundary + every `assert_api` response body). Identical to
/// [`run_trace`] when `scan` is disabled. A leak (or a corpus-less flow kind,
/// or a too-short secret) fails the run with a value-free message, exactly as
/// the record-time store-guard does.
pub fn run_trace_with_secret_scan<D: AppDriver>(
    path: &Path,
    driver: &mut D,
    scan: &SecretScan,
) -> Result<(RunReport, std::path::PathBuf), ReplayError> {
    run_trace_with_secret_scan_and_options(
        path,
        driver,
        scan,
        flowproof_driver::RecordingOptions::default(),
    )
}

/// Replay with both secret-scanning and visual-recording controls.
pub fn run_trace_with_secret_scan_and_options<D: AppDriver>(
    path: &Path,
    driver: &mut D,
    scan: &SecretScan,
    recording: flowproof_driver::RecordingOptions,
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
    let mut recorder = recording
        .enabled()
        .then_some(rules)
        .flatten()
        .and_then(|rules| {
            flowproof_driver::RunRecorder::with_options(&run_dir, rules, recording).ok()
        });
    let target =
        if header.app.name == "web" {
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
            let raw = header.app.window_title.clone().ok_or_else(|| {
                ReplayError::UnknownApp("vision trace without window title".into())
            })?;
            flowproof_driver::AppTarget {
                command: String::new(),
                window_name: flowproof_trace::secret::resolve_refs(&raw)?,
            }
        } else if header.app.name == "windows" {
            // An arbitrary Windows app: unlike `calc` or `notepad` there is no
            // registry entry to look up, because the SPEC supplied the command
            // line and window title. They travel in the header, raw, so a
            // `${VAR}` in either resolves fresh at every replay.
            let command =
                header.app.command.clone().ok_or_else(|| {
                    ReplayError::UnknownApp("windows trace without a command".into())
                })?;
            let window = header.app.window_title.clone().ok_or_else(|| {
                ReplayError::UnknownApp("windows trace without a window title".into())
            })?;
            flowproof_driver::AppTarget {
                command: flowproof_trace::secret::resolve_refs(&command)?,
                window_name: flowproof_trace::secret::resolve_refs(&window)?,
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
                browser.clock.as_ref().map(|c| flowproof_driver::WebClock {
                    at: c.at.clone(),
                    timezone: c.timezone.clone(),
                }),
                browser
                    .random
                    .as_ref()
                    .map(|r| flowproof_driver::WebRandom { seed: r.seed }),
            ))?;
        }
    }
    let visual_paths = VisualPaths {
        baselines: flowproof_driver::visual::baselines_dir(path),
        run_dir: run_dir.clone(),
    };
    let started = Instant::now();
    driver.launch(&target.command, &target.window_name, LAUNCH_TIMEOUT)?;
    // Reproduce the recording's window shape before the first step. The
    // header stores what was APPLIED then, including a position the spec
    // never asked for, so replay reproduces it exactly rather than
    // re-deriving it.
    if let Some(g) = &header.app.geometry {
        driver.set_window_geometry(g.width, g.height, Some((g.x, g.y)))?;
    }

    let name = header
        .spec
        .as_ref()
        .map(|s| s.name.clone())
        .unwrap_or_else(|| header.app.name.clone());
    let mut results = Vec::with_capacity(steps.len());
    let mut failed = false;
    // Flow-scoped captures: read at execution time, never persisted. The
    // trace holds the NAME only, which is what keeps a captured balance or
    // order number out of a reviewable artifact.
    let mut captures: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    // The secret-leak corpus, re-observed exactly as at record and held in
    // memory only: web surface text at each step boundary + every `assert_api`
    // response body. Populated only when the flow asserts the control.
    let mut secret_corpus: Vec<(String, String)> = Vec::new();
    let scan_secrets = scan.enabled();
    let scan_web = scan_secrets && header.app.name == "web";
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
        let (outcome, matched) = execute_step(
            driver,
            step,
            &target.command,
            &visual_paths,
            &mut captures,
            &mut secret_corpus,
            recorder.as_mut(),
        )?;
        let duration_ms = step_started.elapsed().as_millis() as u64;
        if let Some(rec) = recorder.as_mut() {
            rec.step_finished(driver);
        }
        // Step boundary: sample the web surface text (the same text `page
        // shows` reads, not the page source), per-step, not continuous.
        if scan_web && outcome.is_ok() {
            secret_corpus.push((
                "the surface text at a step boundary".to_string(),
                driver.surface_text()?,
            ));
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

    // The secret-leak scan, by the SAME shared mechanism as the record-time
    // store-guard, over the corpus this replay re-observed. Runs only when the
    // flow asserts the control and the earlier steps all passed (a flow that
    // already failed has its verdict; the scan does not overrule it). A
    // corpus-less flow kind is a capability error, never a vacuous pass, but
    // record's store-guard would have refused to mint such a trace, so this is
    // the honest guard, not the common path.
    if scan_secrets && !failed {
        let verdict = if flowproof_trace::secret_scan::has_readable_corpus(&header.app.name) {
            flowproof_trace::secret_scan::scan_corpus(&scan.assertions, &secret_corpus)
        } else {
            Err(flowproof_trace::secret_scan::capability_error(
                &header.app.name,
            ))
        };
        if let Err(message) = verdict {
            failed = true;
            // The leak is a whole-run verdict, not a single recorded step:
            // surface it as its own failed result so the report and every CLI
            // reader (audit included) see the value-free reason.
            results.push(StepResult {
                id: "secret-leak-scan".to_string(),
                intent: "assert_no_secret_leak".to_string(),
                status: StepStatus::Failed,
                detail: Some(message),
                started_ms: started.elapsed().as_millis() as u64,
                duration_ms: 0,
                selector_tier: None,
                degraded: false,
            });
        }
    }

    let degraded = results.iter().any(|s| s.degraded);
    let duration_ms = started.elapsed().as_millis() as u64;
    let recording = recorder.and_then(|recorder| recorder.finish_with_driver(driver));
    let report = RunReport {
        name,
        trace_id: header.trace_id.clone(),
        passed: !failed && !results.is_empty(),
        degraded,
        steps: results,
        duration_ms,
        recording,
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

    /// A `kind: "scoped"` rung decodes into a container query - never into
    /// a bare css/text selector that would resolve page-wide.
    #[test]
    fn a_scoped_rung_decodes_into_a_container_query() {
        let selector: Selector = serde_json::from_str(
            r#"{"tier":"structural","provenance":"web","confidence":0.9,
                "payload":{"kind":"scoped","container":"item",
                           "container_anchor":"Invoice 4711","inner_text":"Amount",
                           "container_id":"transaction-183"}}"#,
        )
        .expect("selector parses");
        let uia = selector_to_uia(&selector).expect("decodes");
        let scope = uia.scope.clone().expect("carries a scope query");
        assert_eq!(scope.container, "item");
        assert_eq!(scope.anchor, "Invoice 4711");
        assert_eq!(scope.inner_text.as_deref(), Some("Amount"));
        assert_eq!(scope.container_id.as_deref(), Some("transaction-183"));
        // Nothing leaked into the unscoped fields.
        assert!(uia.css.is_none() && uia.name.is_none() && uia.automation_id.is_none());

        // Existing cell payloads keep decoding, untouched.
        let cell: Selector = serde_json::from_str(
            r#"{"tier":"structural","provenance":"web",
                "payload":{"kind":"cell","column_text":"Status","row_anchor":"Grace Hopper"}}"#,
        )
        .expect("selector parses");
        let uia = selector_to_uia(&cell).expect("decodes");
        assert_eq!(uia.cell.expect("cell query").column, "Status");
    }

    /// The whole reason the inner keys are PREFIXED: an engine that
    /// predates this rung reads bare `css`/`text`/`automation_id` off any
    /// structural payload. With prefixed keys it decodes to an EMPTY
    /// selector, skips the rung, and fails loudly - instead of resolving
    /// "Amount" page-wide and passing on some other item's amount.
    #[test]
    fn an_older_engine_cannot_resolve_a_scoped_rung_unscoped() {
        let payload: serde_json::Map<String, serde_json::Value> = serde_json::from_str(
            r#"{"kind":"scoped","container":"item","container_anchor":"Invoice 4711",
                "inner_text":"Amount","inner_css":".amount","inner_id":"amount"}"#,
        )
        .expect("payload parses");
        // Verbatim the pre-scoped decode: bare keys only.
        let get = |key: &str| {
            payload
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let legacy = UiaSelector {
            automation_id: get("automation_id").or_else(|| get("id")),
            name: get("name"),
            control_type: get("control_type"),
            css: get("css"),
            ..UiaSelector::default()
        };
        assert!(
            legacy.is_empty(),
            "an older engine must skip this rung, not resolve it page-wide: {legacy:?}"
        );
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
