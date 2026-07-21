//! Rule-based recording: perform each resolved step's existence check
//! against the live application and write a v1 trace.

use std::io::Write;
use std::path::Path;
use std::time::Duration;

use flowproof_driver::{resolve_app, AppDriver, UiaSelector};
use flowproof_trace::format::{
    Action, AppInfo, Artifacts, Assertion, Condition, EnvInfo, Header, Selector, Step, Sync,
    TypeTextParams,
};
use flowproof_trace::{SelectorTier, FORMAT_NAME, FORMAT_VERSION};

use crate::author::{author_step, AuthorContext};
use crate::llm::{HttpModelClient, ModelClient};
use crate::rules::{
    resolve_step, ResolvedAction, RulesError, Target, TextMatch, NOTEPAD_EDITOR_ID,
};
use crate::spec::FlowSpec;

/// Which authoring backend records a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Author {
    /// Rules first, model fallback for steps the rules cannot resolve.
    #[default]
    Auto,
    /// Deterministic rules only (today's behavior).
    Rules,
    /// Model for every step.
    Llm,
}

const LAUNCH_TIMEOUT: Duration = Duration::from_secs(15);
const STEP_TIMEOUT_MS: u64 = 5000;
/// Poll cadence while an auto-waiting assertion is pending.
const ASSERT_POLL_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error(transparent)]
    Rules(#[from] crate::rules::RulesError),
    #[error("unknown app '{0}' (supports: calc, notepad, web, sap, vision, api)")]
    UnknownApp(String),
    #[error("app 'web' requires a `url:` field in the spec")]
    MissingUrl,
    #[error("app 'vision' requires a `window:` field in the spec (title of the window to drive)")]
    MissingWindow,
    #[error("element for step '{intent}' not found: [{selector}]")]
    ElementNotFound { intent: String, selector: String },
    #[error(
        "assertion '{intent}' does not hold while recording: expected '{expected}', element shows '{actual}'"
    )]
    AssertMismatch {
        intent: String,
        expected: String,
        actual: String,
    },
    #[error("driver error: {0}")]
    Driver(#[from] flowproof_driver::DriverError),
    #[error(transparent)]
    Secret(#[from] flowproof_trace::secret::MissingSecret),
    #[error("cannot write trace {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    Agent(#[from] crate::AgentError),
    #[error(
        "cannot resolve step '{step}' with rules and no model backend is configured \
         (set FLOWPROOF_AI_PROVIDER / FLOWPROOF_AI_API_KEY to enable LLM authoring): {rules_error}"
    )]
    NoAuthor { step: String, rules_error: String },
    #[error("driver cannot describe its scene; LLM authoring is unavailable for app '{0}'")]
    NoScene(String),
    #[error(
        "cannot author step '{}' ({}): {} — a structured clarification payload with the \
         live-screen inventory is available via `record --json` or the MCP record tool",
        .0.step, .0.stage, .0.reason
    )]
    NeedsClarification(Box<crate::clarify::Clarification>),
}

/// Outcome of a recording session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    pub trace_path: std::path::PathBuf,
    pub steps: usize,
    /// Steps reused verbatim from the previous trace (`record --reuse`);
    /// 0 on a fresh recording.
    pub reused_steps: usize,
}

fn native_selector(payload: serde_json::Map<String, serde_json::Value>) -> Selector {
    Selector {
        tier: SelectorTier::NativeId,
        provenance: flowproof_trace::format::Adapter::Uia,
        confidence: Some(1.0),
        payload,
    }
}

fn fallback_selector(
    tier: SelectorTier,
    confidence: f64,
    payload: serde_json::Map<String, serde_json::Value>,
) -> Selector {
    Selector {
        tier,
        provenance: flowproof_trace::format::Adapter::Uia,
        confidence: Some(confidence),
        payload,
    }
}

/// The recorded selector ladder for an action target. The primary rung is
/// always the native id; UIA targets with a known label also get structural
/// (control type + name) and text-anchor rungs, so replay survives an id
/// rename by degrading down the ladder (and reporting that it did).
fn selectors_for(app: &str, target: &Target, label: Option<&str>) -> Vec<Selector> {
    match target {
        // A SAP scripting id is that provenance's native rung. Payload key
        // `id` per the trace format; a text-anchor fallback (the element's
        // accessible name) survives id drift across transactions/releases.
        Target::AutomationId(automation_id) if app == "sap" => {
            let mut payload = serde_json::Map::new();
            payload.insert("id".into(), automation_id.as_str().into());
            let mut ladder = vec![Selector {
                tier: SelectorTier::NativeId,
                provenance: flowproof_trace::format::Adapter::SapCom,
                confidence: Some(1.0),
                payload,
            }];
            if let Some(label) = label {
                let mut anchor = serde_json::Map::new();
                anchor.insert("text".into(), label.into());
                ladder.push(Selector {
                    tier: SelectorTier::TextAnchor,
                    provenance: flowproof_trace::format::Adapter::SapCom,
                    confidence: Some(0.5),
                    payload: anchor,
                });
            }
            ladder
        }
        Target::AutomationId(automation_id) => {
            let mut payload = serde_json::Map::new();
            payload.insert("automation_id".into(), automation_id.as_str().into());
            if let Some(label) = label {
                payload.insert("name".into(), label.into());
            }
            let mut ladder = vec![native_selector(payload)];
            if app == "notepad" && automation_id == NOTEPAD_EDITOR_ID {
                // The Win32 control id `15` varies across Notepad
                // generations — the editor's structural identity does not.
                let mut fallback = serde_json::Map::new();
                fallback.insert("control_type".into(), "Edit".into());
                fallback.insert("name".into(), "Text Editor".into());
                ladder.push(fallback_selector(SelectorTier::Structural, 0.7, fallback));
            }
            if let Some(label) = label {
                // A labelled press target is a button; its accessible name
                // outlives automation-id refactors.
                let mut structural = serde_json::Map::new();
                structural.insert("control_type".into(), "Button".into());
                structural.insert("name".into(), label.into());
                ladder.push(fallback_selector(SelectorTier::Structural, 0.7, structural));
                let mut anchor = serde_json::Map::new();
                anchor.insert("text".into(), label.into());
                ladder.push(fallback_selector(SelectorTier::TextAnchor, 0.5, anchor));
            }
            ladder
        }
        Target::Css(css) => {
            let mut payload = serde_json::Map::new();
            payload.insert("css".into(), css.as_str().into());
            vec![Selector {
                tier: SelectorTier::NativeId,
                provenance: flowproof_trace::format::Adapter::Web,
                confidence: Some(1.0),
                payload,
            }]
        }
        // A text anchor IS the primary selector here: the element is
        // addressed the way a user sees it (visible text / placeholder).
        Target::Text(text) => {
            let mut payload = serde_json::Map::new();
            payload.insert("text".into(), text.as_str().into());
            vec![Selector {
                tier: SelectorTier::TextAnchor,
                provenance: match app {
                    "web" => flowproof_trace::format::Adapter::Web,
                    "sap" => flowproof_trace::format::Adapter::SapCom,
                    "vision" => flowproof_trace::format::Adapter::Vision,
                    _ => flowproof_trace::format::Adapter::Uia,
                },
                confidence: Some(1.0),
                payload,
            }]
        }
        // The surface has no selector — the assertion's `scope` key IS the
        // encoding (see step_for); every adapter answers `surface_text`.
        Target::Surface => Vec::new(),
        // An ordinal narrows every rung of the inner ladder to the nth match.
        Target::Nth(n, inner) => {
            let mut ladder = selectors_for(app, inner, label);
            for selector in &mut ladder {
                selector.payload.insert("nth".into(), (*n).into());
            }
            ladder
        }
    }
}

/// Pixels-only steps record WHERE the action lands relative to the matched
/// text: typing targets the input field beside its label; everything else
/// acts on the text itself. The vision driver applies the same defaults —
/// stamping the relation keeps the trace self-describing (and matches the
/// schema's spatial text_anchor form).
fn stamp_vision_relation(selectors: &mut [Selector], action: &ResolvedAction) {
    let relation = match action {
        ResolvedAction::TypeText { .. } | ResolvedAction::Clear { .. } => "right_of",
        _ => "inside",
    };
    for selector in selectors {
        if selector.tier == SelectorTier::TextAnchor {
            selector
                .payload
                .entry("relation".to_string())
                .or_insert_with(|| relation.into());
        }
    }
}

fn step_for(id: usize, intent: &str, app: &str, action: &ResolvedAction) -> Step {
    let (mut selectors, trace_action) = match action {
        ResolvedAction::Press { target, label } => (
            selectors_for(app, target, Some(label)),
            Action::Click(serde_json::Map::new()),
        ),
        ResolvedAction::TypeText { target, text } => (
            selectors_for(app, target, None),
            Action::TypeText(TypeTextParams {
                text: text.clone(),
                submit: None,
                extra: serde_json::Map::new(),
            }),
        ),
        // Focused typing has no target: an empty selector list IS the
        // "type where the focus is" encoding.
        ResolvedAction::TypeFocused { text } => (
            Vec::new(),
            Action::TypeText(TypeTextParams {
                text: text.clone(),
                submit: None,
                extra: serde_json::Map::new(),
            }),
        ),
        // Clear is a replace-with-nothing TypeText, flagged via `replace`.
        ResolvedAction::Clear { target } => {
            let mut extra = serde_json::Map::new();
            extra.insert("replace".into(), true.into());
            (
                selectors_for(app, target, None),
                Action::TypeText(TypeTextParams {
                    text: String::new(),
                    submit: None,
                    extra,
                }),
            )
        }
        ResolvedAction::PressKey { key, modifiers } => (
            Vec::new(),
            Action::PressKey(flowproof_trace::format::PressKeyParams {
                key: key.clone(),
                modifiers: modifiers.clone(),
                extra: serde_json::Map::new(),
            }),
        ),
        ResolvedAction::Upload { target, path } => (
            selectors_for(app, target, None),
            Action::Upload(flowproof_trace::format::UploadParams {
                path: path.clone(),
                extra: serde_json::Map::new(),
            }),
        ),
        ResolvedAction::ContextClick { target, label } => (
            selectors_for(app, target, Some(label)),
            Action::RightClick(serde_json::Map::new()),
        ),
        // Mid-flow navigation reuses the launch action kind: `url` (raw,
        // refs unresolved) or `reload: true`.
        ResolvedAction::Navigate { path } => {
            let mut params = serde_json::Map::new();
            params.insert("url".into(), path.as_str().into());
            (Vec::new(), Action::Launch(params))
        }
        ResolvedAction::Reload => {
            let mut params = serde_json::Map::new();
            params.insert("reload".into(), true.into());
            (Vec::new(), Action::Launch(params))
        }
        ResolvedAction::AssertText {
            target,
            expected,
            matcher,
            timeout_ms,
        } => {
            let mut expect = match matcher {
                TextMatch::Contains => serde_json::json!({ "value_contains": expected }),
                TextMatch::NotContains => serde_json::json!({ "value_not_contains": expected }),
                TextMatch::CountEquals(n) => {
                    serde_json::json!({ "value_contains": expected, "count": n })
                }
                TextMatch::Equals => serde_json::json!({ "value_equals": expected }),
                TextMatch::NumericEquals => {
                    serde_json::json!({ "value_equals": expected, "normalize": "numeric" })
                }
            };
            expect["timeout_ms"] = serde_json::json!(timeout_ms);
            let selectors = selectors_for(app, target, None);
            // Surface-scoped asserts carry no selector: the explicit
            // `scope` key is the encoding every adapter resolves its own
            // way (page text / window subtree / OCR frame).
            let selector_ref = if matches!(target, Target::Surface) {
                expect["scope"] = serde_json::json!("surface");
                None
            } else {
                Some(0)
            };
            (
                selectors,
                Action::Assert(Assertion::ElementState {
                    expect,
                    selector_ref,
                }),
            )
        }
        ResolvedAction::AssertPresence {
            target,
            present,
            timeout_ms,
        } => (
            selectors_for(app, target, None),
            Action::Assert(Assertion::ElementState {
                expect: serde_json::json!({
                    "element_present": present,
                    "timeout_ms": timeout_ms,
                }),
                selector_ref: Some(0),
            }),
        ),
        ResolvedAction::AssertEnabled {
            target,
            enabled,
            timeout_ms,
        } => (
            selectors_for(app, target, None),
            Action::Assert(Assertion::ElementState {
                expect: serde_json::json!({
                    "enabled": enabled,
                    "timeout_ms": timeout_ms,
                }),
                selector_ref: Some(0),
            }),
        ),
        // Out-of-band assertions: the connection NAME and the raw (ref-
        // bearing) query/url travel in the trace; credentials never do.
        ResolvedAction::AssertSql {
            connection,
            query,
            equals,
            timeout_ms,
        } => {
            let mut expect = serde_json::Map::new();
            if let Some(equals) = equals {
                expect.insert("equals".into(), equals.as_str().into());
            }
            expect.insert("timeout_ms".into(), (*timeout_ms).into());
            (
                Vec::new(),
                Action::Assert(Assertion::Sql {
                    connection: connection.clone(),
                    query: query.clone(),
                    expect: Some(serde_json::Value::Object(expect)),
                }),
            )
        }
        ResolvedAction::AssertApi {
            method,
            url,
            headers,
            body,
            status,
            body_contains,
            timeout_ms,
        } => {
            let mut expect = serde_json::Map::new();
            if let Some(needle) = body_contains {
                expect.insert("body_contains".into(), needle.as_str().into());
            }
            expect.insert("timeout_ms".into(), (*timeout_ms).into());
            (
                Vec::new(),
                Action::Assert(Assertion::Api {
                    // Raw clones: body string leaves and header values keep
                    // their ${VAR} refs — tokens never enter the trace.
                    request: flowproof_trace::format::ApiRequest {
                        method: method.clone(),
                        url: url.clone(),
                        body: body.clone(),
                        headers: headers.clone(),
                    },
                    status: *status,
                    expect: Some(serde_json::Value::Object(expect)),
                }),
            )
        }
    };
    if app == "vision" {
        stamp_vision_relation(&mut selectors, action);
    }
    let is_assert = matches!(trace_action, Action::Assert(_));
    // Targetless actions (key press, focused typing) have nothing to wait
    // for, and assertions do their OWN waiting — a presence-absence assert
    // must not be gated on the element existing first. Targeted actions
    // wait for any rung of the ladder.
    let pre = if selectors.is_empty() || is_assert {
        vec![]
    } else {
        vec![Condition::ElementExists {
            timeout_ms: STEP_TIMEOUT_MS,
            selector_ref: None,
        }]
    };
    Step {
        id: format!("s{id:04}"),
        intent: intent.to_string(),
        action: trace_action,
        selectors,
        sync: Sync { pre, post: vec![] },
        artifacts: Artifacts::default(),
    }
}

fn target_selector(target: &Target) -> Option<UiaSelector> {
    match target {
        Target::AutomationId(id) => Some(UiaSelector::automation_id(id.clone())),
        Target::Css(css) => Some(UiaSelector::css(css.clone())),
        Target::Text(text) => Some(UiaSelector {
            name: Some(text.clone()),
            ..UiaSelector::default()
        }),
        // The surface is not an element — it resolves via `surface_text`.
        Target::Surface => None,
        Target::Nth(n, inner) => target_selector(inner).map(|s| s.with_nth(Some(*n))),
    }
}

/// The live-driver selector for an action's target; None for targetless
/// actions (key press, focused typing) and surface-scoped assertions.
fn action_selector(action: &ResolvedAction) -> Option<UiaSelector> {
    let target = match action {
        ResolvedAction::Press { target, .. }
        | ResolvedAction::TypeText { target, .. }
        | ResolvedAction::Upload { target, .. }
        | ResolvedAction::ContextClick { target, .. }
        | ResolvedAction::Clear { target }
        | ResolvedAction::AssertText { target, .. }
        | ResolvedAction::AssertPresence { target, .. }
        | ResolvedAction::AssertEnabled { target, .. } => target,
        ResolvedAction::TypeFocused { .. }
        | ResolvedAction::PressKey { .. }
        | ResolvedAction::Navigate { .. }
        | ResolvedAction::Reload
        | ResolvedAction::AssertSql { .. }
        | ResolvedAction::AssertApi { .. } => return None,
    };
    target_selector(target)
}

/// Resolve where to launch: registry apps by id, `web` from the spec URL
/// (`${VAR}` references resolve from the environment; relative paths become
/// absolute `file://` URLs).
fn launch_target(spec: &FlowSpec) -> Result<flowproof_driver::AppTarget, RecordError> {
    if spec.app == "web" {
        let url = spec.url.as_deref().ok_or(RecordError::MissingUrl)?;
        let url = flowproof_trace::secret::resolve_refs(url)?;
        let url = if url.contains("://") {
            url.to_string()
        } else {
            let absolute = std::fs::canonicalize(&url).map_err(|source| RecordError::Io {
                path: url.to_string(),
                source,
            })?;
            format!("file://{}", absolute.display())
        };
        return Ok(flowproof_driver::AppTarget {
            command: url,
            window_name: String::new(),
        });
    }
    if spec.app == "sap" {
        // `command` carries the SAP Logon connection description (empty =
        // attach to whatever logged-in session exists). Like the web URL it
        // may hold `${VAR}` references, resolved here and at every launch.
        let connection = spec.connection.as_deref().unwrap_or_default();
        let connection = flowproof_trace::secret::resolve_refs(connection)?;
        return Ok(flowproof_driver::AppTarget {
            command: connection,
            window_name: "SAP".into(),
        });
    }
    if spec.app == "vision" {
        // Pixels mode attaches to a window by title — nothing is spawned.
        let window = spec.window.as_deref().ok_or(RecordError::MissingWindow)?;
        let window = flowproof_trace::secret::resolve_refs(window)?;
        return Ok(flowproof_driver::AppTarget {
            command: String::new(),
            window_name: window,
        });
    }
    if spec.app == "api" {
        // Out-of-band only: nothing to launch. NoOpDriver::launch ignores
        // this empty target.
        return Ok(flowproof_driver::AppTarget {
            command: String::new(),
            window_name: String::new(),
        });
    }
    resolve_app(&spec.app).ok_or_else(|| RecordError::UnknownApp(spec.app.clone()))
}

fn driver_key_mod(m: &flowproof_trace::format::KeyModifier) -> flowproof_driver::KeyMod {
    match m {
        flowproof_trace::format::KeyModifier::Ctrl => flowproof_driver::KeyMod::Ctrl,
        flowproof_trace::format::KeyModifier::Alt => flowproof_driver::KeyMod::Alt,
        flowproof_trace::format::KeyModifier::Shift => flowproof_driver::KeyMod::Shift,
        flowproof_trace::format::KeyModifier::Win => flowproof_driver::KeyMod::Meta,
        // Portable primary modifier, resolved by the OS running the flow.
        flowproof_trace::format::KeyModifier::Mod => {
            if cfg!(target_os = "macos") {
                flowproof_driver::KeyMod::Meta
            } else {
                flowproof_driver::KeyMod::Ctrl
            }
        }
    }
}

/// Reconstruct the recorder's `Target` from a step's PRIMARY selector —
/// the inverse of `selectors_for` for the shapes the recorder itself
/// emits. `None` = not reconstructable (reuse falls back to fresh
/// authoring, never to a guess).
fn target_from_selector(selectors: &[Selector]) -> Option<Target> {
    let primary = selectors.first()?;
    let get = |key: &str| primary.payload.get(key).and_then(|v| v.as_str());
    let base = if let Some(css) = get("css") {
        Target::css(css)
    } else if let Some(id) = get("automation_id").or_else(|| get("id")) {
        Target::id(id)
    } else if primary.tier == SelectorTier::TextAnchor {
        Target::text(get("text")?)
    } else {
        return None;
    };
    match primary.payload.get("nth").and_then(|v| v.as_u64()) {
        Some(n) => Some(Target::Nth(n as u32, Box::new(base))),
        None => Some(base),
    }
}

/// Decode one recorded step back into the `ResolvedAction` that produced
/// it — the inverse of `step_for`, for incremental re-record: a decoded
/// action re-executes and re-encodes IDENTICALLY (same target → same
/// ladder), with zero rules or model involvement. `None` = this step kind
/// is not safely reconstructable; the caller authors fresh instead.
fn decode_step(step: &Step) -> Option<ResolvedAction> {
    use flowproof_trace::format::Assertion;
    match &step.action {
        Action::Click(_) => Some(ResolvedAction::Press {
            target: target_from_selector(&step.selectors)?,
            label: step.intent.clone(),
        }),
        Action::TypeText(params) => {
            let replace = params.extra.get("replace").and_then(|v| v.as_bool()) == Some(true);
            if step.selectors.is_empty() {
                (!replace).then(|| ResolvedAction::TypeFocused {
                    text: params.text.clone(),
                })
            } else if replace && params.text.is_empty() {
                Some(ResolvedAction::Clear {
                    target: target_from_selector(&step.selectors)?,
                })
            } else if replace {
                // Replace-with-text is a compound the rules encode as two
                // steps; a single step with both flags isn't ours to guess.
                None
            } else {
                Some(ResolvedAction::TypeText {
                    target: target_from_selector(&step.selectors)?,
                    text: params.text.clone(),
                })
            }
        }
        Action::PressKey(params) => Some(ResolvedAction::PressKey {
            key: params.key.clone(),
            modifiers: params.modifiers.clone(),
        }),
        Action::Upload(params) => Some(ResolvedAction::Upload {
            target: target_from_selector(&step.selectors)?,
            path: params.path.clone(),
        }),
        Action::RightClick(_) => Some(ResolvedAction::ContextClick {
            target: target_from_selector(&step.selectors)?,
            label: step.intent.clone(),
        }),
        Action::Launch(params) => {
            if params.get("reload").and_then(|v| v.as_bool()) == Some(true) {
                Some(ResolvedAction::Reload)
            } else {
                Some(ResolvedAction::Navigate {
                    path: params.get("url")?.as_str()?.to_string(),
                })
            }
        }
        Action::Assert(Assertion::ElementState {
            expect,
            selector_ref: _,
        }) => {
            let timeout_ms = expect
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(10_000);
            let target = if expect.get("scope").and_then(|v| v.as_str()) == Some("surface") {
                Target::Surface
            } else {
                target_from_selector(&step.selectors)?
            };
            if let Some(present) = expect.get("element_present").and_then(|v| v.as_bool()) {
                return Some(ResolvedAction::AssertPresence {
                    target,
                    present,
                    timeout_ms,
                });
            }
            if let Some(enabled) = expect.get("enabled").and_then(|v| v.as_bool()) {
                return Some(ResolvedAction::AssertEnabled {
                    target,
                    enabled,
                    timeout_ms,
                });
            }
            let (expected, matcher) =
                if let Some(e) = expect.get("value_not_contains").and_then(|v| v.as_str()) {
                    (e, TextMatch::NotContains)
                } else if let Some(e) = expect.get("value_contains").and_then(|v| v.as_str()) {
                    match expect.get("count").and_then(|v| v.as_u64()) {
                        Some(n) => (e, TextMatch::CountEquals(n)),
                        None => (e, TextMatch::Contains),
                    }
                } else {
                    let e = expect.get("value_equals").and_then(|v| v.as_str())?;
                    if expect.get("normalize").and_then(|v| v.as_str()) == Some("numeric") {
                        (e, TextMatch::NumericEquals)
                    } else {
                        (e, TextMatch::Equals)
                    }
                };
            Some(ResolvedAction::AssertText {
                target,
                expected: expected.to_string(),
                matcher,
                timeout_ms,
            })
        }
        Action::Assert(Assertion::Sql {
            connection,
            query,
            expect,
        }) => Some(ResolvedAction::AssertSql {
            connection: connection.clone(),
            query: query.clone(),
            equals: expect
                .as_ref()
                .and_then(|e| e.get("equals"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            timeout_ms: oob_timeout_from(expect.as_ref()),
        }),
        Action::Assert(Assertion::Api {
            request,
            status,
            expect,
        }) => Some(ResolvedAction::AssertApi {
            method: request.method.clone(),
            url: request.url.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            status: *status,
            body_contains: expect
                .as_ref()
                .and_then(|e| e.get("body_contains"))
                .and_then(|v| v.as_str())
                .map(str::to_string),
            timeout_ms: oob_timeout_from(expect.as_ref()),
        }),
        _ => None,
    }
}

fn oob_timeout_from(expect: Option<&serde_json::Value>) -> u64 {
    expect
        .and_then(|e| e.get("timeout_ms"))
        .and_then(|v| v.as_u64())
        .unwrap_or(10_000)
}

/// Old-trace reuse state for incremental re-record (`record --reuse`):
/// consecutive old steps grouped by intent (one group per original spec
/// step), consumed in order as spec steps match. Skipped-over groups are
/// deleted spec steps; a spec step with no matching group is new.
pub struct ReuseCursor {
    groups: Vec<(String, Vec<Step>)>,
    next: usize,
    /// Trace steps reused verbatim (for the summary).
    pub reused_steps: usize,
}

impl ReuseCursor {
    pub fn new(old_steps: &[Step]) -> Self {
        let mut groups: Vec<(String, Vec<Step>)> = Vec::new();
        for step in old_steps {
            match groups.last_mut() {
                Some((intent, group)) if *intent == step.intent => group.push(step.clone()),
                _ => groups.push((step.intent.clone(), vec![step.clone()])),
            }
        }
        Self {
            groups,
            next: 0,
            reused_steps: 0,
        }
    }

    /// The old actions for `intent`, iff every step of the matching group
    /// decodes AND its target still resolves on the live app. Anything
    /// less → `None`, and the caller authors fresh (the incremental heal).
    fn take_matching<D: AppDriver>(
        &mut self,
        driver: &mut D,
        intent: &str,
    ) -> Result<Option<Vec<ResolvedAction>>, RecordError> {
        let Some(pos) = (self.next..self.groups.len()).find(|&i| self.groups[i].0 == intent) else {
            return Ok(None);
        };
        let mut actions = Vec::new();
        for step in &self.groups[pos].1 {
            let Some(action) = decode_step(step) else {
                return Ok(None);
            };
            let is_assert = matches!(
                &action,
                ResolvedAction::AssertText { .. }
                    | ResolvedAction::AssertPresence { .. }
                    | ResolvedAction::AssertEnabled { .. }
                    | ResolvedAction::AssertSql { .. }
                    | ResolvedAction::AssertApi { .. }
            );
            if !is_assert {
                if let Some(selector) = action_selector(&action) {
                    if !driver.element_exists(&selector)? {
                        return Ok(None); // drifted — re-author this step
                    }
                }
            }
            actions.push(action);
        }
        self.reused_steps += self.groups[pos].1.len();
        self.next = pos + 1;
        Ok(Some(actions))
    }
}

/// Trace mock rule → the driver's fully-resolved form (one conversion,
/// shared shape with replay via `WebMock::from_rule_parts`).
fn web_mock_from_rule(rule: &flowproof_trace::format::MockRule) -> flowproof_driver::WebMock {
    flowproof_driver::WebMock::from_rule_parts(
        &rule.url_contains,
        rule.method.as_deref(),
        rule.status,
        rule.content_type.as_deref(),
        rule.body.as_ref(),
    )
}

/// Trace browser setup → the driver's fully-resolved form (defaults live
/// in `WebBrowserConfig::from_setup_parts`, shared with replay).
fn web_browser_from_setup(
    setup: &flowproof_trace::format::BrowserSetup,
) -> flowproof_driver::WebBrowserConfig {
    flowproof_driver::WebBrowserConfig::from_setup_parts(
        setup
            .viewport
            .as_ref()
            .map(|v| (v.width, v.height, v.device_scale_factor, v.mobile, v.touch)),
        setup.user_agent.as_deref(),
        &setup.args,
    )
}

/// Poll an out-of-band probe until it holds or the bound elapses — the row
/// may still be committing, the API still converging. Configuration errors
/// (missing connection env) fail immediately.
fn poll_oob(
    probe: &flowproof_driver::oob::OobProbe,
    timeout_ms: u64,
    intent: &str,
) -> Result<(), RecordError> {
    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match flowproof_driver::oob::check(probe)? {
            Ok(()) => return Ok(()),
            Err(reason) => {
                if std::time::Instant::now() >= deadline {
                    return Err(RecordError::AssertMismatch {
                        intent: intent.to_string(),
                        expected: intent.to_string(),
                        actual: reason,
                    });
                }
                std::thread::sleep(ASSERT_POLL_INTERVAL);
            }
        }
    }
}

fn assert_holds(actual: &str, expected: &str, matcher: TextMatch) -> bool {
    match matcher {
        TextMatch::Contains => actual.contains(expected),
        TextMatch::NotContains => !actual.contains(expected),
        TextMatch::CountEquals(n) => actual.matches(expected).count() as u64 == n,
        TextMatch::Equals => actual == expected,
        TextMatch::NumericEquals => matches!(
            (flowproof_driver::numeric_value(actual), expected.parse::<f64>()),
            (Some(a), Ok(e)) if a == e
        ),
    }
}

/// Record `spec` against the live app via `driver`, writing the trace to
/// `out`. Every planned action's target element must exist before it is
/// written — recording is a verification pass, not a transcription.
/// Uses [`Author::Auto`]: rules first, model fallback when configured.
pub fn record<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
) -> Result<RecordSummary, RecordError> {
    let mut client = HttpModelClient::from_env();
    record_with_client(spec, driver, out, Author::Auto, client.as_mut())
}

/// Record with an explicit authoring mode (the CLI's `--author`).
pub fn record_with_author<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
    author: Author,
) -> Result<RecordSummary, RecordError> {
    let mut client = HttpModelClient::from_env();
    record_with_client(spec, driver, out, author, client.as_mut())
}

/// Incremental re-record (the CLI's `record --reuse`): env-configured
/// model backend, old steps consulted per spec step.
pub fn record_incremental<D: AppDriver>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
    author: Author,
    old_steps: &[Step],
) -> Result<RecordSummary, RecordError> {
    let mut client = HttpModelClient::from_env();
    record_with_reuse(spec, driver, out, author, client.as_mut(), Some(old_steps))
}

/// Resolve one spec step into actions per the authoring mode.
#[allow(clippy::too_many_arguments)] // internal plumbing fn; grouping would obscure it
fn author_actions<D: AppDriver, C: ModelClient>(
    spec: &FlowSpec,
    driver: &mut D,
    author: Author,
    client: &mut Option<&mut C>,
    prior: &[String],
    spec_step: &crate::spec::SpecStep,
    llm_used: &mut bool,
    reuse: &mut Option<ReuseCursor>,
) -> Result<Vec<ResolvedAction>, RecordError> {
    let intent = spec_step.intent();
    let intent = intent.as_str();
    // Incremental re-record: an old step group whose intent matches and
    // whose target still resolves is reused VERBATIM — no rules, no model.
    if let Some(cursor) = reuse {
        if let Some(actions) = cursor.take_matching(driver, intent)? {
            return Ok(actions);
        }
    }
    let rules_result = match author {
        Author::Llm => Err(RulesError::UnsupportedApp("llm forced".into())),
        _ => resolve_step(&spec.app, spec_step),
    };
    match rules_result {
        Ok(actions) => Ok(actions),
        Err(rules_error) => {
            if author == Author::Rules {
                return Err(RecordError::Rules(rules_error));
            }
            // Ambiguity from here on ends in a structured clarification:
            // the driving agent — not flowproof — resolves it and re-records.
            // `prior` holds the intents already performed, so its length is
            // this step's index and the live scene reflects their effects.
            let clarify = |stage, reason: String, rules_err: Option<String>, scene: Vec<_>| {
                RecordError::NeedsClarification(Box::new(crate::clarify::Clarification {
                    step: intent.to_string(),
                    step_index: prior.len(),
                    stage,
                    reason,
                    rules_error: rules_err,
                    completed_steps: prior.to_vec(),
                    scene,
                    hint: crate::clarify::Clarification::HINT.into(),
                }))
            };
            let Some(client) = client.as_mut() else {
                let inventory = driver
                    .scene()
                    .ok()
                    .flatten()
                    .map(|s| crate::clarify::scene_inventory(&s))
                    .unwrap_or_default();
                return Err(clarify(
                    crate::clarify::ClarifyStage::NoModel,
                    format!(
                        "no model backend is configured (set FLOWPROOF_AI_PROVIDER / \
                         FLOWPROOF_AI_API_KEY to enable LLM authoring): {rules_error}"
                    ),
                    Some(rules_error.to_string()),
                    inventory,
                ));
            };
            let scene = driver
                .scene()?
                .ok_or_else(|| RecordError::NoScene(spec.app.clone()))?;
            let ctx = AuthorContext {
                flow_name: &spec.name,
                app: &spec.app,
                url: spec.url.as_deref(),
                prior_steps: prior,
                intent,
                scene: &scene,
            };
            match author_step(*client, &ctx) {
                Ok(action) => {
                    *llm_used = true;
                    Ok(vec![action])
                }
                // Grounding failure after the retry = genuine ambiguity.
                // Config errors (bad key, network) stay plain errors.
                Err(crate::AgentError::Authoring { reason, .. }) => Err(clarify(
                    crate::clarify::ClarifyStage::Model,
                    reason,
                    Some(rules_error.to_string()),
                    crate::clarify::scene_inventory(&scene),
                )),
                Err(other) => Err(other.into()),
            }
        }
    }
}

/// Record with an injected model client (used by tests; `record` and
/// `record_with_author` build one from the environment).
pub fn record_with_client<D: AppDriver, C: ModelClient>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
    author: Author,
    client: Option<&mut C>,
) -> Result<RecordSummary, RecordError> {
    record_with_reuse(spec, driver, out, author, client, None)
}

/// Incremental re-record: reuse every old step whose intent still matches
/// and whose target still resolves; author fresh only what drifted. Turns
/// "the app changed, re-record the flow" into "re-record the step" — with
/// zero rules/model work for the stable majority.
pub fn record_with_reuse<D: AppDriver, C: ModelClient>(
    spec: &FlowSpec,
    driver: &mut D,
    out: &Path,
    author: Author,
    mut client: Option<&mut C>,
    old_steps: Option<&[Step]>,
) -> Result<RecordSummary, RecordError> {
    let mut reuse = old_steps.map(ReuseCursor::new);
    let target = launch_target(spec)?;
    if let Some(setup) = &spec.session {
        let (cookies, local_storage) = setup.resolved()?;
        driver.stage_session(flowproof_driver::WebSession {
            cookies,
            local_storage,
        })?;
    }
    if !spec.mock.is_empty() {
        driver.stage_mocks(spec.mock.iter().map(web_mock_from_rule).collect())?;
    }
    if let Some(browser) = &spec.browser {
        if !browser.is_empty() {
            driver.stage_browser(web_browser_from_setup(browser))?;
        }
    }
    driver.launch(&target.command, &target.window_name, LAUNCH_TIMEOUT)?;
    let (width, height) = driver.screen_size()?;

    // The authoring execution is itself recorded (review surface): frames
    // land in a self-contained bundle keyed by trace_id, referenced from
    // the header. Recording being unavailable never blocks authoring.
    let trace_id = uuid::Uuid::new_v4().to_string();
    let bundle_rel = format!(".flowproof/recordings/{trace_id}");
    let bundle_base = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(&bundle_rel);
    let mut recorder = flowproof_driver::RunRecorder::new(&bundle_base, spec.redact.clone()).ok();

    // Recording PERFORMS the flow once: buttons are really pressed and the
    // assert is checked against the live display, so a trace is only ever
    // written for a flow that actually worked.
    let mut steps = Vec::new();
    let mut prior_intents: Vec<String> = Vec::new();
    let mut llm_used = false;
    for spec_step in &spec.steps {
        let intent = spec_step.intent().to_string();
        let actions = author_actions(
            spec,
            driver,
            author,
            &mut client,
            &prior_intents,
            spec_step,
            &mut llm_used,
            &mut reuse,
        )?;
        prior_intents.push(intent);
        for action in actions {
            let step_id = format!("s{:04}", steps.len() + 1);
            if let Some(rec) = recorder.as_mut() {
                rec.step_started(driver, &step_id);
            }
            let selector = action_selector(&action);
            // Assertions do their own waiting (an element may legitimately
            // not exist yet — a toast — or be expected to be gone); every
            // other targeted action requires its element up front.
            let is_assert = matches!(
                &action,
                ResolvedAction::AssertText { .. }
                    | ResolvedAction::AssertPresence { .. }
                    | ResolvedAction::AssertEnabled { .. }
            );
            if !is_assert {
                if let Some(selector) = &selector {
                    if !driver.element_exists(selector)? {
                        return Err(RecordError::ElementNotFound {
                            intent: spec_step.intent().to_string(),
                            selector: selector.to_string(),
                        });
                    }
                }
            }
            let targeted = || selector.as_ref().expect("targeted action has a selector");
            match &action {
                ResolvedAction::Press { .. } => driver.invoke(targeted())?,
                ResolvedAction::TypeText { text, .. } => {
                    // `${VAR}` secrets resolve at the moment of typing; the
                    // trace only ever stores the reference (see step_for).
                    let value = flowproof_trace::secret::resolve_refs(text)?;
                    driver.type_text(targeted(), &value)?
                }
                ResolvedAction::TypeFocused { text } => {
                    let value = flowproof_trace::secret::resolve_refs(text)?;
                    driver.type_focused(&value)?
                }
                ResolvedAction::Clear { .. } => driver.clear_text(targeted())?,
                ResolvedAction::Upload { path, .. } => {
                    driver.set_files(targeted(), std::slice::from_ref(path))?
                }
                ResolvedAction::ContextClick { .. } => driver.context_click(targeted())?,
                ResolvedAction::AssertSql {
                    connection,
                    query,
                    equals,
                    timeout_ms,
                } => {
                    let probe = flowproof_driver::oob::OobProbe::Sql {
                        connection: connection.clone(),
                        query: flowproof_trace::secret::resolve_refs(query)?,
                        equals: match equals {
                            Some(e) => Some(flowproof_trace::secret::resolve_refs(e)?),
                            None => None,
                        },
                    };
                    poll_oob(&probe, *timeout_ms, &spec_step.intent())?
                }
                ResolvedAction::AssertApi {
                    method,
                    url,
                    headers,
                    body,
                    status,
                    body_contains,
                    timeout_ms,
                } => {
                    // Resolved like `equals` above: the trace keeps the raw
                    // ${VAR}; only the live probe sees values — including
                    // header tokens and body string leaves.
                    let probe = flowproof_driver::oob::OobProbe::Api {
                        method: method.clone(),
                        url: flowproof_trace::secret::resolve_refs(url)?,
                        body: match body {
                            Some(b) => Some(flowproof_trace::secret::resolve_refs_in_json(b)?),
                            None => None,
                        },
                        headers: headers
                            .iter()
                            .map(|(k, v)| {
                                Ok((k.clone(), flowproof_trace::secret::resolve_refs(v)?))
                            })
                            .collect::<Result<_, flowproof_trace::secret::MissingSecret>>()?,
                        status: *status,
                        body_contains: match body_contains {
                            Some(needle) => Some(flowproof_trace::secret::resolve_refs(needle)?),
                            None => None,
                        },
                    };
                    poll_oob(&probe, *timeout_ms, &spec_step.intent())?
                }
                ResolvedAction::PressKey { key, modifiers } => {
                    let mods: Vec<flowproof_driver::KeyMod> =
                        modifiers.iter().map(driver_key_mod).collect();
                    driver.press_key(key, &mods)?
                }
                ResolvedAction::Navigate { path } => {
                    let path = flowproof_trace::secret::resolve_refs(path)?;
                    driver.navigate(&flowproof_driver::absolute_url(&path, &target.command))?
                }
                ResolvedAction::Reload => driver.reload()?,
                ResolvedAction::AssertText {
                    expected,
                    matcher,
                    timeout_ms,
                    ..
                } => {
                    // Assertions auto-wait while recording too: the flow is
                    // being performed for real, so a slow backend operation
                    // takes just as long here as it will at replay. The
                    // element itself may also still be appearing (a toast),
                    // so existence is part of the same poll. A surface-
                    // scoped assert (no selector) reads the whole surface.
                    let wanted = flowproof_trace::secret::resolve_refs(expected)?;
                    let deadline = std::time::Instant::now() + Duration::from_millis(*timeout_ms);
                    loop {
                        let actual = if selector.is_none() {
                            Some(driver.surface_text()?)
                        } else if driver.element_exists(targeted())? {
                            Some(driver.read_text(targeted())?)
                        } else {
                            None
                        };
                        if let Some(actual) = &actual {
                            if assert_holds(actual, &wanted, *matcher) {
                                break;
                            }
                        }
                        if std::time::Instant::now() >= deadline {
                            // Error messages carry the RAW expectation — a
                            // resolved secret must not leak through a failure.
                            return Err(RecordError::AssertMismatch {
                                intent: spec_step.intent().to_string(),
                                expected: expected.clone(),
                                actual: actual.unwrap_or_else(|| "<element not found>".to_string()),
                            });
                        }
                        std::thread::sleep(ASSERT_POLL_INTERVAL);
                    }
                }
                ResolvedAction::AssertPresence {
                    present,
                    timeout_ms,
                    ..
                } => {
                    let deadline = std::time::Instant::now() + Duration::from_millis(*timeout_ms);
                    while driver.element_exists(targeted())? != *present {
                        if std::time::Instant::now() >= deadline {
                            return Err(RecordError::AssertMismatch {
                                intent: spec_step.intent().to_string(),
                                expected: if *present {
                                    "element visible".to_string()
                                } else {
                                    "element not visible".to_string()
                                },
                                actual: if *present {
                                    "element never appeared".to_string()
                                } else {
                                    "element still on screen".to_string()
                                },
                            });
                        }
                        std::thread::sleep(ASSERT_POLL_INTERVAL);
                    }
                }
                ResolvedAction::AssertEnabled {
                    enabled,
                    timeout_ms,
                    ..
                } => {
                    let state = |e: bool| if e { "enabled" } else { "disabled" };
                    let deadline = std::time::Instant::now() + Duration::from_millis(*timeout_ms);
                    loop {
                        if driver.element_exists(targeted())?
                            && driver.element_enabled(targeted())? == *enabled
                        {
                            break;
                        }
                        if std::time::Instant::now() >= deadline {
                            return Err(RecordError::AssertMismatch {
                                intent: spec_step.intent().to_string(),
                                expected: format!("element {}", state(*enabled)),
                                actual: format!("element {}", state(!*enabled)),
                            });
                        }
                        std::thread::sleep(ASSERT_POLL_INTERVAL);
                    }
                }
            }
            if let Some(rec) = recorder.as_mut() {
                rec.step_finished(driver);
            }
            steps.push(step_for(
                steps.len() + 1,
                &spec_step.intent(),
                &spec.app,
                &action,
            ));
        }
    }

    let recording = recorder.and_then(flowproof_driver::RunRecorder::finish);
    if let Some(recording) = &recording {
        for step in &mut steps {
            if let Some(timing) = recording.steps.iter().find(|t| t.id == step.id) {
                step.artifacts.recording = Some(flowproof_trace::format::StepRecording {
                    start_ms: timing.start_ms,
                    end_ms: timing.end_ms,
                });
            }
        }
    }

    let header = Header {
        format: FORMAT_NAME.to_string(),
        version: FORMAT_VERSION,
        trace_id: trace_id.clone(),
        recorded_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        recording: recording
            .as_ref()
            .map(|r| flowproof_trace::format::RecordingRef {
                format: r.format.clone(),
                dir: format!("{bundle_rel}/{}", r.dir),
                started_at: None,
            }),
        redaction: spec
            .redact
            .iter()
            .filter_map(|rule| serde_json::to_value(rule).ok())
            .collect(),
        // The RAW session setup — cookie values keep their `${VAR}` refs.
        session: spec.session.clone(),
        // Mock rules travel with the trace: what was mocked at record MUST
        // be mocked at replay, or the two executions test different things.
        mock: spec.mock.clone(),
        // Browser shape travels too: a flow recorded mobile replays mobile.
        browser: spec.browser.clone(),
        spec: Some(flowproof_trace::format::SpecRef {
            name: spec.name.clone(),
            path: None,
            hash: None,
        }),
        app: AppInfo {
            name: spec.app.clone(),
            adapter: match spec.app.as_str() {
                "web" => flowproof_trace::format::Adapter::Web,
                "sap" => flowproof_trace::format::Adapter::SapCom,
                "vision" => flowproof_trace::format::Adapter::Vision,
                "api" => flowproof_trace::format::Adapter::Api,
                _ => flowproof_trace::format::Adapter::Uia,
            },
            window_title: (!target.window_name.is_empty()).then(|| target.window_name.to_string()),
            // If the spec URL (or SAP connection) carries `${VAR}` refs, the
            // header stores them RAW (resolved again at each replay);
            // otherwise the resolved launch value (absolute file:// paths
            // included). For `app: sap` this field carries the connection
            // description — how replay reaches the same system.
            url: match spec.app.as_str() {
                "web" => Some(
                    spec.url
                        .as_ref()
                        .filter(|u| flowproof_trace::secret::has_refs(u))
                        .cloned()
                        .unwrap_or_else(|| target.command.clone()),
                ),
                "sap" => spec
                    .connection
                    .as_ref()
                    .filter(|c| flowproof_trace::secret::has_refs(c))
                    .cloned()
                    .or_else(|| (!target.command.is_empty()).then(|| target.command.clone())),
                _ => None,
            },
            version: None,
        },
        agent: (llm_used && client.is_some()).then(|| {
            let (backend, model) = client.as_ref().map(|c| c.identity()).unwrap_or_default();
            flowproof_trace::format::AgentInfo {
                backend: if backend == "anthropic" {
                    flowproof_trace::format::AgentBackend::Anthropic
                } else {
                    flowproof_trace::format::AgentBackend::OpenaiCompatible
                },
                model: Some(model),
            }
        }),
        env: EnvInfo {
            os: std::env::consts::OS.to_string(),
            resolution: (width.max(1), height.max(1)),
            dpi_scale: None,
            locale: None,
        },
    };

    let io_err = |source: std::io::Error| RecordError::Io {
        path: out.display().to_string(),
        source,
    };
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(io_err)?;
    }
    let mut file = std::fs::File::create(out).map_err(io_err)?;
    writeln!(file, "{}", serde_json::to_string(&header)?).map_err(io_err)?;
    for step in &steps {
        writeln!(file, "{}", serde_json::to_string(step)?).map_err(io_err)?;
    }

    Ok(RecordSummary {
        trace_path: out.to_path_buf(),
        steps: steps.len(),
        reused_steps: reuse.map(|c| c.reused_steps).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use flowproof_driver::mock::MockAppDriver;
    use flowproof_trace::TraceLine;

    use super::*;
    use crate::spec::FlowSpec;

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

    #[test]
    fn records_the_calc_flow_against_a_mock() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let mut driver =
            MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
        let dir = std::env::temp_dir().join("flowproof-recorder-test");
        let out = dir.join("calc.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");

        assert_eq!(summary.steps, 5); // 4 presses + 1 assert
        assert_eq!(
            driver.launched,
            Some(("calc.exe".to_string(), "Calculator".to_string()))
        );
        // Recording really performed the flow.
        assert_eq!(
            driver.invoked,
            vec!["num5Button", "plusButton", "num3Button", "equalButton"]
        );

        let contents = std::fs::read_to_string(&out).expect("trace written");
        let lines: Vec<_> = contents.lines().collect();
        assert_eq!(lines.len(), 6);
        assert!(matches!(
            TraceLine::parse(lines[0]).expect("header parses"),
            TraceLine::Header(_)
        ));
        for line in &lines[1..] {
            assert!(matches!(
                TraceLine::parse(line).expect("step parses"),
                TraceLine::Step(_)
            ));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_element_fails_recording() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        // No plusButton in the fake UI.
        let mut driver = MockAppDriver::new(&[
            "num5Button",
            "num3Button",
            "equalButton",
            "CalculatorResults",
        ]);
        let out = std::env::temp_dir().join("flowproof-recorder-missing.trace.jsonl");
        let err = record(&spec, &mut driver, &out).expect_err("must fail");
        assert!(matches!(err, RecordError::ElementNotFound { .. }));
    }

    #[test]
    fn failing_assert_aborts_recording() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let mut driver =
            MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 9");
        let out = std::env::temp_dir().join("flowproof-recorder-mismatch.trace.jsonl");
        let err = record(&spec, &mut driver, &out).expect_err("must fail");
        assert!(matches!(err, RecordError::AssertMismatch { .. }));
    }

    const NOTEPAD_SPEC: &str = "\
name: Write a note
app: notepad
steps:
  - Type hello from flowproof
  - assert: document contains hello
";

    #[test]
    fn records_the_notepad_flow_against_a_mock() {
        let spec = FlowSpec::parse(NOTEPAD_SPEC).expect("spec parses");
        let mut driver = MockAppDriver::new(&["15"]);
        let dir = std::env::temp_dir().join("flowproof-recorder-notepad");
        let out = dir.join("notepad.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");

        assert_eq!(summary.steps, 2); // one type + one assert
        assert_eq!(
            driver.typed,
            vec![("15".to_string(), "hello from flowproof".to_string())]
        );

        // The editor step carries the two-rung selector ladder.
        let contents = std::fs::read_to_string(&out).expect("trace written");
        let step_line = contents.lines().nth(1).expect("first step");
        let step: serde_json::Value = serde_json::from_str(step_line).expect("step is JSON");
        let selectors = step["selectors"].as_array().expect("selectors array");
        assert_eq!(selectors.len(), 2);
        assert_eq!(selectors[0]["payload"]["automation_id"], "15");
        assert_eq!(selectors[1]["payload"]["control_type"], "Edit");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_keyboard_clear_and_focused_typing() {
        let spec = FlowSpec::parse(
            "name: Keyboard flow
app: web
url: https://e.test/x
steps:
  - Type old into the \"Template name\" field
  - Clear the \"Template name\" field
  - Type new
  - Press Enter
  - Press Control+V
",
        )
        .expect("spec parses");
        let mut driver = MockAppDriver::new(&["Template name"]);
        let dir = std::env::temp_dir().join("flowproof-recorder-keyboard");
        let out = dir.join("keyboard.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");
        assert_eq!(summary.steps, 5);
        assert_eq!(driver.cleared, vec!["Template name"]);
        assert_eq!(driver.typed_focused, vec!["new"]);
        assert_eq!(driver.keys_pressed, vec!["Enter", "Ctrl+v"]);

        let contents = std::fs::read_to_string(&out).expect("trace written");
        // Clear is a replace-with-nothing TypeText.
        assert!(contents.contains("\"replace\":true"), "clear encoded");
        // The key press travels as a first-class press_key action.
        assert!(contents.contains("\"press_key\""), "press_key encoded");
        assert!(
            contents.contains("\"modifiers\":[\"ctrl\"]"),
            "modifiers encoded"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upload_right_click_and_portable_modifier_record_and_encode() {
        let spec = FlowSpec::parse(
            "name: Import
app: web
url: https://e.test/x
steps:
  - Upload fixtures/data.qif into the \"Import file\" field
  - Right-click \"Accounts\"
  - Press Mod+K
",
        )
        .expect("spec parses");
        let mut driver = MockAppDriver::new(&["Import file", "Accounts"]);
        let dir = std::env::temp_dir().join("flowproof-recorder-upload");
        let out = dir.join("upload.trace.jsonl");
        let summary = record(&spec, &mut driver, &out).expect("recording succeeds");
        assert_eq!(summary.steps, 3);
        assert_eq!(
            driver.uploads,
            vec![("Import file".to_string(), "fixtures/data.qif".to_string())]
        );
        assert_eq!(driver.context_clicked, vec!["Accounts"]);
        // Mod resolves per-OS at execution (Ctrl here on CI), but the
        // TRACE stays neutral: the same file replays on any OS.
        let expected_chord = if cfg!(target_os = "macos") {
            "Meta+k"
        } else {
            "Ctrl+k"
        };
        assert_eq!(driver.keys_pressed, vec![expected_chord]);

        let contents = std::fs::read_to_string(&out).expect("trace written");
        assert!(contents.contains("\"upload\""), "upload action encoded");
        assert!(
            contents.contains("\"path\":\"fixtures/data.qif\""),
            "upload path encoded"
        );
        assert!(
            contents.contains("\"right_click\""),
            "right_click action encoded"
        );
        assert!(
            contents.contains("\"modifiers\":[\"mod\"]"),
            "portable modifier stored neutrally, not resolved into the trace"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn upload_and_right_click_decode_back_for_reuse() {
        // decode_step must be a strict inverse of step_for so `record
        // --reuse` re-executes these without rules or model involvement.
        let upload = ResolvedAction::Upload {
            target: Target::text("Import file"),
            path: "fixtures/data.qif".into(),
        };
        let step = step_for(1, "Upload data", "web", &upload);
        assert_eq!(decode_step(&step), Some(upload));

        let context_click = ResolvedAction::ContextClick {
            target: Target::text("Accounts"),
            label: "Right-click Accounts".into(),
        };
        let step = step_for(2, "Right-click Accounts", "web", &context_click);
        match decode_step(&step) {
            Some(ResolvedAction::ContextClick { target, .. }) => {
                assert_eq!(target, Target::text("Accounts"));
            }
            other => panic!("right_click must decode to ContextClick, got {other:?}"),
        }
    }

    struct CountingClient {
        reply: String,
        calls: usize,
    }

    impl crate::ModelClient for CountingClient {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, crate::AgentError> {
            self.calls += 1;
            Ok(self.reply.clone())
        }

        fn identity(&self) -> (String, String) {
            ("openai-compatible".into(), "test-model".into())
        }
    }

    #[test]
    fn rules_resolvable_steps_never_call_the_model() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        let mut driver =
            MockAppDriver::new(&CALC_ELEMENTS).with_text("CalculatorResults", "Display is 8");
        let mut client = CountingClient {
            reply: String::new(),
            calls: 0,
        };
        let out = std::env::temp_dir().join("flowproof-rules-first.trace.jsonl");
        record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
            .expect("rules author the whole flow");
        assert_eq!(client.calls, 0, "rules-first: model must not be consulted");
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn unresolvable_step_falls_back_to_the_model_and_stamps_agent() {
        let spec = FlowSpec::parse(
            "name: Freeform
app: web
url: https://example.test/x
steps:
  - Smash that shiny button
",
        )
        .expect("spec parses");
        let mut driver = MockAppDriver::new(&["#shiny", "body"]);
        driver.scene = Some(r##"[{"target":"css:#shiny","tag":"button","text":"Shiny"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#shiny"}"##.into(),
            calls: 0,
        };
        let dir = std::env::temp_dir().join("flowproof-llm-fallback");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("freeform.trace.jsonl");
        record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
            .expect("model authors the step");
        assert_eq!(client.calls, 1);
        assert_eq!(driver.invoked, vec!["#shiny"]);
        let header = std::fs::read_to_string(&out)
            .expect("trace written")
            .lines()
            .next()
            .map(str::to_string)
            .expect("header line");
        assert!(header.contains("\"agent\""), "agent stamped: {header}");
        assert!(header.contains("openai-compatible"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rules_only_mode_refuses_unresolvable_steps() {
        let spec = FlowSpec::parse(
            "name: x
app: web
url: https://e.test/x
steps:
  - Smash that shiny button
",
        )
        .expect("parses");
        let mut driver = MockAppDriver::new(&["#shiny"]);
        driver.scene = Some(r##"[{"target":"css:#shiny"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#shiny"}"##.into(),
            calls: 0,
        };
        let out = std::env::temp_dir().join("flowproof-rules-only.trace.jsonl");
        let err = record_with_client(&spec, &mut driver, &out, Author::Rules, Some(&mut client))
            .expect_err("rules-only must fail");
        assert!(matches!(err, RecordError::Rules(_)));
        assert_eq!(client.calls, 0);
    }

    #[test]
    fn incremental_reuses_stable_steps_with_zero_model_calls() {
        let spec = FlowSpec::parse(
            "name: Mixed\napp: web\nurl: https://e.test/x\nsteps:\n  - Type hello into the \"Name\" field\n  - Smash that shiny button\n",
        )
        .expect("parses");
        let dir = std::env::temp_dir().join("flowproof-incremental-stable");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("mixed.trace.jsonl");

        // Original recording: the freeform step needs the model.
        let mut driver = MockAppDriver::new(&["Name", "#shiny"]);
        driver.scene = Some(r##"[{"target":"css:#shiny","tag":"button"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#shiny"}"##.into(),
            calls: 0,
        };
        record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
            .expect("original records");
        assert_eq!(client.calls, 1);
        let (_, old_steps) = load_steps(&out);
        let old_selectors: Vec<_> = old_steps.iter().map(|s| s.selectors.clone()).collect();

        // Unchanged app: everything reuses, the model is NEVER consulted.
        let mut driver = MockAppDriver::new(&["Name", "#shiny"]);
        driver.scene = Some(r##"[{"target":"css:#shiny","tag":"button"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#WRONG-IF-CALLED"}"##.into(),
            calls: 0,
        };
        let summary = record_with_reuse(
            &spec,
            &mut driver,
            &out,
            Author::Auto,
            Some(&mut client),
            Some(&old_steps),
        )
        .expect("incremental records");
        assert_eq!(client.calls, 0, "stable steps must not consult the model");
        assert_eq!(summary.reused_steps, old_steps.len());
        let (_, new_steps) = load_steps(&out);
        let new_selectors: Vec<_> = new_steps.iter().map(|s| s.selectors.clone()).collect();
        assert_eq!(new_selectors, old_selectors, "selectors identical");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn incremental_reauthors_only_the_drifted_step() {
        let spec = FlowSpec::parse(
            "name: Mixed\napp: web\nurl: https://e.test/x\nsteps:\n  - Type hello into the \"Name\" field\n  - Smash that shiny button\n",
        )
        .expect("parses");
        let dir = std::env::temp_dir().join("flowproof-incremental-drift");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("mixed.trace.jsonl");

        let mut driver = MockAppDriver::new(&["Name", "#shiny"]);
        driver.scene = Some(r##"[{"target":"css:#shiny","tag":"button"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#shiny"}"##.into(),
            calls: 0,
        };
        record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
            .expect("original records");
        let (_, old_steps) = load_steps(&out);

        // The app drifted: #shiny became #polished. The Type step reuses;
        // the drifted step re-grounds via the model against the new scene.
        let mut driver = MockAppDriver::new(&["Name", "#polished"]);
        driver.scene = Some(r##"[{"target":"css:#polished","tag":"button"}]"##.into());
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#polished"}"##.into(),
            calls: 0,
        };
        let summary = record_with_reuse(
            &spec,
            &mut driver,
            &out,
            Author::Auto,
            Some(&mut client),
            Some(&old_steps),
        )
        .expect("incremental records");
        assert_eq!(client.calls, 1, "only the drifted step consults the model");
        assert_eq!(summary.reused_steps, 1, "the stable Type step reused");
        let trace = std::fs::read_to_string(&out).expect("trace readable");
        assert!(trace.contains("#polished"), "drifted step re-grounded");
        assert!(!trace.contains("#shiny"), "stale selector gone");
        std::fs::remove_dir_all(&dir).ok();
    }

    fn load_steps(path: &Path) -> ((), Vec<Step>) {
        let contents = std::fs::read_to_string(path).expect("trace readable");
        let steps = contents
            .lines()
            .skip(1)
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str::<Step>(l).expect("step parses"))
            .collect();
        ((), steps)
    }

    #[test]
    fn no_model_backend_yields_clarification_with_scene() {
        let spec = FlowSpec::parse(
            "name: Vague
app: web
url: https://e.test/x
steps:
  - Type acme into the \"Supplier\" field
  - make required field changes
",
        )
        .expect("parses");
        let mut driver = MockAppDriver::new(&["Supplier"]);
        driver.scene = Some(
            r##"[{"target":"css:#price","tag":"input","type":"text","label":"Net Price"}]"##.into(),
        );
        let out = std::env::temp_dir().join("flowproof-clarify-nomodel.trace.jsonl");
        let err = record_with_client(
            &spec,
            &mut driver,
            &out,
            Author::Auto,
            Option::<&mut CountingClient>::None,
        )
        .expect_err("vague step with no model must need clarification");
        let RecordError::NeedsClarification(c) = err else {
            panic!("expected NeedsClarification, got: {err}");
        };
        assert_eq!(c.step, "make required field changes");
        assert_eq!(c.step_index, 1);
        assert_eq!(c.stage, crate::ClarifyStage::NoModel);
        assert_eq!(
            c.completed_steps,
            vec!["Type acme into the \"Supplier\" field"]
        );
        // The live inventory reached the payload — the driving agent can
        // see the "Net Price" field exists and rewrite the step.
        assert_eq!(c.scene.len(), 1);
        assert_eq!(c.scene[0].label.as_deref(), Some("Net Price"));
        assert!(c.rules_error.is_some());
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn ungrounded_model_yields_clarification() {
        let spec = FlowSpec::parse(
            "name: Vague
app: web
url: https://e.test/x
steps:
  - make required field changes
",
        )
        .expect("parses");
        let mut driver = MockAppDriver::new(&["#price"]);
        driver.scene = Some(r##"[{"target":"css:#price","tag":"input"}]"##.into());
        // The model invents a selector on both attempts — grounding fails.
        let mut client = CountingClient {
            reply: r##"{"action":"click","target":"css:#invented"}"##.into(),
            calls: 0,
        };
        let out = std::env::temp_dir().join("flowproof-clarify-model.trace.jsonl");
        let err = record_with_client(&spec, &mut driver, &out, Author::Auto, Some(&mut client))
            .expect_err("ungroundable step must need clarification");
        let RecordError::NeedsClarification(c) = err else {
            panic!("expected NeedsClarification, got: {err}");
        };
        assert_eq!(c.stage, crate::ClarifyStage::Model);
        assert_eq!(client.calls, 2, "one attempt + one self-correcting retry");
        assert!(c.rules_error.is_some(), "rules diagnostic travels along");
        assert_eq!(c.scene[0].target, "css:#price");
        std::fs::remove_file(&out).ok();
    }

    #[test]
    fn unknown_app_is_rejected() {
        let spec =
            FlowSpec::parse("name: x\napp: oracle-forms\nsteps:\n  - Type 1\n").expect("parses");
        let mut driver = MockAppDriver::new(&[]);
        let out = std::env::temp_dir().join("unused.trace.jsonl");
        assert!(matches!(
            record(&spec, &mut driver, &out).expect_err("must fail"),
            RecordError::UnknownApp(_)
        ));
    }
}
