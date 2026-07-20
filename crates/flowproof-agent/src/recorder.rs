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
    #[error("unknown app '{0}' (this slice supports: calc, notepad, web, sap)")]
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
}

/// Outcome of a recording session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSummary {
    pub trace_path: std::path::PathBuf,
    pub steps: usize,
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
                    request: flowproof_trace::format::ApiRequest {
                        method: method.clone(),
                        url: url.clone(),
                        body: None,
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
        | ResolvedAction::Clear { target }
        | ResolvedAction::AssertText { target, .. }
        | ResolvedAction::AssertPresence { target, .. } => target,
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
    resolve_app(&spec.app).ok_or_else(|| RecordError::UnknownApp(spec.app.clone()))
}

fn driver_key_mod(m: &flowproof_trace::format::KeyModifier) -> flowproof_driver::KeyMod {
    match m {
        flowproof_trace::format::KeyModifier::Ctrl => flowproof_driver::KeyMod::Ctrl,
        flowproof_trace::format::KeyModifier::Alt => flowproof_driver::KeyMod::Alt,
        flowproof_trace::format::KeyModifier::Shift => flowproof_driver::KeyMod::Shift,
        flowproof_trace::format::KeyModifier::Win => flowproof_driver::KeyMod::Meta,
    }
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

/// Resolve one spec step into actions per the authoring mode.
fn author_actions<D: AppDriver, C: ModelClient>(
    spec: &FlowSpec,
    driver: &mut D,
    author: Author,
    client: &mut Option<&mut C>,
    prior: &[String],
    spec_step: &crate::spec::SpecStep,
    llm_used: &mut bool,
) -> Result<Vec<ResolvedAction>, RecordError> {
    let intent = spec_step.intent();
    let intent = intent.as_str();
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
            let Some(client) = client.as_mut() else {
                return Err(RecordError::NoAuthor {
                    step: intent.to_string(),
                    rules_error: rules_error.to_string(),
                });
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
            let action = author_step(*client, &ctx)?;
            *llm_used = true;
            Ok(vec![action])
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
    mut client: Option<&mut C>,
) -> Result<RecordSummary, RecordError> {
    let target = launch_target(spec)?;
    if let Some(setup) = &spec.session {
        let (cookies, local_storage) = setup.resolved()?;
        driver.stage_session(flowproof_driver::WebSession {
            cookies,
            local_storage,
        })?;
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
                ResolvedAction::AssertText { .. } | ResolvedAction::AssertPresence { .. }
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
                    status,
                    body_contains,
                    timeout_ms,
                } => {
                    let probe = flowproof_driver::oob::OobProbe::Api {
                        method: method.clone(),
                        url: flowproof_trace::secret::resolve_refs(url)?,
                        body: None,
                        status: *status,
                        body_contains: body_contains.clone(),
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
