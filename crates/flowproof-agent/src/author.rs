//! The LLM authoring loop: given a natural-language step and the live app's
//! scene graph, ask a model to choose ONE action against an element it can
//! see. The model must pick its target from the offered scene — it cannot
//! invent selectors — and the chosen action is then performed and verified
//! by the recorder exactly like a rules-authored one.

use serde::{Deserialize, Serialize};

use crate::rules::{resolve_step, ResolvedAction, Target};
use crate::spec::SpecStep;
use crate::{AgentError, ModelClient};

const SYSTEM_PROMPT: &str = "\
You are the authoring agent of flowproof, an end-to-end UI testing tool. \
You translate ONE natural-language test step into concrete UI actions \
against the app under test (a web page, a desktop window, ...). You are \
given the actionable or readable elements of the current screen as JSON; each \
carries a `target` token. Rules:
- Interpret the user's meaning, not a fixed phrase, keyword list, or deterministic \
grammar. Instructions may be terse, conversational, indirect, or use synonyms. \
Use the current screen, prior steps, and remembered captures to infer the intent.
- The user never needs to provide selectors, target tokens, or rule syntax. Choose \
the matching target from the live-screen inventory and let flowproof validate and \
persist its deterministic target. Never invent a selector from the user's wording.
- Respond with ONLY a JSON object, no prose, no code fences.
- The JSON action is one of: \"click\", \"click_at\", \"drag\", \"type_text\", \
\"assert_text\", \"capture_text\", \"capture_count\", \"type_captured\", \
\"select_option\", \"select_options\", \"scroll\", \"press_key\", \"rule_step\", or \
\"capture_ambiguity\".
- UI actions MUST include \"target\": \"<target token of a listed element>\". \
Clicking and typing require an entry whose \"actionable\" field is true. \
Readable-only entries may be captured or asserted, never acted on. \
type_text also needs \"text\"; assert_text needs \"expected\" and optional \
\"contains\"; capture_text needs a safe \"name\"; type_captured needs \"capture\". \
drag needs \"onto\" with a second listed actionable target. click_at needs \
\"x_pct\" and \"y_pct\" from 0 through 100. capture_count needs a safe \"name\". \
select_option needs one \"text\" value; select_options is only for a multi-select \
and needs a non-empty \"values\" array. scroll needs \"to_px\". \
press_key needs \"key\" and has no target. \
For another action, use rule_step with \"step\" containing exact \
deterministic grammar and copy listed target tokens into quoted targets, for \
example `Clear the \"css:#name\" field`, `Check the \"css:#terms\" checkbox`, \
`Select Canada from the \"css:#country\" field`, `Press Enter`, \
`Hover over \"css:#menu\"`, or `Go to /settings`.
- capture_text reads the listed target's visible text into its name. A safe \
name starts with a lowercase letter and then contains only lowercase letters, \
digits, or underscores.
- type_captured may name ONLY one of the remembered captures listed in the \
user message. Do not put a remembered value or a capture reference in \"text\"; \
use type_captured so flowproof can preserve the reference.
- A pronoun such as \"it\" or \"that value\" may select a capture only when exactly \
one remembered capture can fit. If more than one could fit, respond with \
{\"action\":\"capture_ambiguity\"}. Never guess; flowproof supplies the safe \
intent-derived reference and candidates.
- `target` MUST be copied verbatim from one of the listed elements. \
Scoped targets beginning with `scoped:` already encode a stable container, \
anchor and inner element; copy the whole token exactly like any other target. \
For assert_text you may also use \"surface\" to check everything readable \
on the current screen.
- Type exactly the text the step asks for; do not add anything.";

/// Machine-readable reason emitted when a remembered-value reference is
/// ambiguous. `author_step` serializes this as the `AgentError::Authoring`
/// reason so the recorder can lift it into its structured clarification.
pub const CAPTURE_AMBIGUITY_KIND: &str = "capture_reference_ambiguity";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureAmbiguity {
    pub kind: String,
    pub reference: String,
    pub candidates: Vec<String>,
}

impl CaptureAmbiguity {
    fn new(reference: impl Into<String>, mut candidates: Vec<String>) -> Self {
        candidates.sort();
        candidates.dedup();
        Self {
            kind: CAPTURE_AMBIGUITY_KIND.into(),
            reference: reference.into(),
            candidates,
        }
    }

    fn reason(&self) -> String {
        serde_json::to_string(self).expect("capture ambiguity always serializes")
    }
}

/// What the model must return. `target_css` is accepted as a legacy alias
/// for `target` so replies shaped for the old web-only contract still parse.
#[derive(Debug, Deserialize)]
struct AuthoredAction {
    action: String,
    #[serde(default, alias = "target_css")]
    target: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    expected: Option<String>,
    #[serde(default)]
    contains: Option<bool>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    capture: Option<String>,
    #[serde(default)]
    step: Option<String>,
    #[serde(default)]
    onto: Option<String>,
    #[serde(default)]
    values: Option<Vec<String>>,
    #[serde(default)]
    x_pct: Option<f64>,
    #[serde(default)]
    y_pct: Option<f64>,
    #[serde(default)]
    to_px: Option<u32>,
    #[serde(default)]
    key: Option<String>,
}

/// Context for authoring one step.
pub struct AuthorContext<'a> {
    pub flow_name: &'a str,
    pub app: &'a str,
    pub url: Option<&'a str>,
    /// Intents of the steps already authored, in order.
    pub prior_steps: &'a [String],
    pub intent: &'a str,
    /// Scene JSON from the driver.
    pub scene: &'a str,
    /// Safe names of flow-scoped captures already available to this step.
    pub captures: &'a [String],
}

fn user_prompt(ctx: &AuthorContext<'_>) -> String {
    let prior = if ctx.prior_steps.is_empty() {
        "(none)".to_string()
    } else {
        ctx.prior_steps.join("; ")
    };
    let captures = serde_json::to_string(ctx.captures).expect("capture names serialize");
    format!(
        "Flow: {name}\nApp: {app}{url}\nSteps already performed: {prior}\n\
         Remembered captures in scope: {captures}\n\
         Current step to perform: {intent}\n\nInteractable elements:\n{scene}",
        name = ctx.flow_name,
        app = ctx.app,
        url = ctx.url.map(|u| format!(" ({u})")).unwrap_or_default(),
        prior = prior,
        captures = captures,
        intent = ctx.intent,
        scene = ctx.scene,
    )
}

/// Capture names use the same deliberately narrow grammar as rules-authored
/// captures: `[a-z][a-z0-9_]*`.
fn valid_capture_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn normalized_words(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn mentioned_captures(intent: &str, captures: &[String]) -> Vec<String> {
    let intent = format!(" {} ", normalized_words(intent));
    captures
        .iter()
        .filter(|name| {
            let name = normalized_words(name);
            intent.contains(&format!(" {name} "))
        })
        .cloned()
        .collect()
}

fn has_capture_pronoun(intent: &str) -> bool {
    let words = format!(" {} ", normalized_words(intent));
    [
        " it ",
        " that ",
        " this ",
        " them ",
        " that value ",
        " this value ",
        " the value ",
        " remembered value ",
        " same value ",
    ]
    .iter()
    .any(|phrase| words.contains(phrase))
}

#[derive(Debug)]
enum GroundingError {
    Rejected(String),
    CaptureAmbiguity(CaptureAmbiguity),
}

impl From<&str> for GroundingError {
    fn from(value: &str) -> Self {
        Self::Rejected(value.into())
    }
}

impl From<String> for GroundingError {
    fn from(value: String) -> Self {
        Self::Rejected(value)
    }
}

impl std::fmt::Display for GroundingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(reason) => f.write_str(reason),
            Self::CaptureAmbiguity(ambiguity) => f.write_str(&ambiguity.reason()),
        }
    }
}

/// The grounding set: one TARGET TOKEN per scene element. Modern drivers
/// emit a `target` token directly (`css:…`, `id:…`, `text:…`); a legacy
/// web scene that only carries a `css` key is lifted into `css:<sel>`.
fn scene_targets(scene: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| {
            e["target"]
                .as_str()
                .map(str::to_string)
                .or_else(|| e["css"].as_str().map(|css| format!("css:{css}")))
        })
        .collect()
}

fn scene_label(scene: &str, token: &str) -> Option<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .ok()?
        .into_iter()
        .find(|element| {
            element["target"].as_str() == Some(token)
                || element["css"]
                    .as_str()
                    .is_some_and(|css| format!("css:{css}") == token)
        })
        .and_then(|element| {
            element["label"]
                .as_str()
                .or_else(|| element["text"].as_str())
                .filter(|label| !label.is_empty())
                .map(str::to_string)
        })
}

/// Resolve one model-visible scene token into the deterministic target that
/// will be persisted in the trace. Most tokens are ordinary `css:`, `id:` or
/// `text:` values. A web scene may additionally expose a synthetic `scoped:`
/// token for a readable value that is only stable relative to a labelled
/// row/card; its structured metadata becomes the existing `Target::Scoped`
/// representation and the synthetic token itself never enters the trace.
fn target_from_scene(scene: &str, token: &str) -> Option<Target> {
    let elements = serde_json::from_str::<Vec<serde_json::Value>>(scene).ok()?;
    let entry = elements.iter().find(|element| {
        element["target"].as_str() == Some(token)
            || element["css"]
                .as_str()
                .is_some_and(|css| format!("css:{css}") == token)
    })?;
    if let Some(scope) = entry.get("scope") {
        if let Some(frame) = scope.get("frame").and_then(|value| value.as_str()) {
            let inner = crate::rules::target_from_token(scope["inner"].as_str()?)?;
            return Some(Target::Framed {
                frame: frame.to_string(),
                inner: Box::new(inner),
            });
        }
        let container = scope["container"].as_str()?.to_string();
        let anchor = scope["anchor"].as_str()?.to_string();
        let inner = crate::rules::target_from_token(scope["inner"].as_str()?)?;
        return Some(Target::Scoped {
            container,
            anchor,
            also: Vec::new(),
            inner: Box::new(inner),
        });
    }
    crate::rules::target_from_token(token).or_else(|| {
        entry["css"]
            .as_str()
            .and_then(|css| crate::rules::target_from_token(&format!("css:{css}")))
    })
}

fn scene_actionable_targets(scene: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(scene)
        .unwrap_or_default()
        .iter()
        .filter(|element| element["actionable"].as_bool().unwrap_or(true))
        .filter_map(|element| {
            element["target"]
                .as_str()
                .map(str::to_string)
                .or_else(|| element["css"].as_str().map(|css| format!("css:{css}")))
        })
        .collect()
}

fn scene_token_is_actionable(scene: &str, token: &str) -> bool {
    scene_actionable_targets(scene)
        .iter()
        .any(|item| item == token || item == &format!("css:{token}"))
}

fn action_targets(action: &ResolvedAction) -> Vec<&Target> {
    match action {
        ResolvedAction::Press { target, .. }
        | ResolvedAction::TypeText { target, .. }
        | ResolvedAction::Upload { target, .. }
        | ResolvedAction::ContextClick { target, .. }
        | ResolvedAction::DoubleClick { target, .. }
        | ResolvedAction::Hover { target, .. }
        | ResolvedAction::Clear { target }
        | ResolvedAction::ClickAt { target, .. }
        | ResolvedAction::SelectOptions { target, .. }
        | ResolvedAction::Capture { target, .. }
        | ResolvedAction::SetChecked { target, .. }
        | ResolvedAction::AssertText { target, .. }
        | ResolvedAction::AssertCaptured { target, .. }
        | ResolvedAction::AssertCount { target, .. }
        | ResolvedAction::AssertChecked { target, .. }
        | ResolvedAction::AssertEnabled { target, .. }
        | ResolvedAction::AssertAttribute { target, .. }
        | ResolvedAction::AssertStyle { target, .. }
        | ResolvedAction::AssertPresence { target, .. } => vec![target],
        ResolvedAction::Drag { target, onto, .. } => vec![target, onto],
        ResolvedAction::Scroll {
            target: Some(target),
            ..
        } => vec![target],
        ResolvedAction::AssertScreenshot { .. }
        | ResolvedAction::TypeFocused { .. }
        | ResolvedAction::PressKey { .. }
        | ResolvedAction::Navigate { .. }
        | ResolvedAction::Reload
        | ResolvedAction::Scroll { target: None, .. }
        | ResolvedAction::AssertSql { .. }
        | ResolvedAction::AssertApi { .. } => vec![],
    }
}

fn validate_rule_action(
    action: &ResolvedAction,
    scene: &str,
    scene_tokens: &[String],
    actionable_tokens: &[String],
    captures: &[String],
) -> Result<(), GroundingError> {
    let grounded: Vec<Target> = scene_tokens
        .iter()
        .filter_map(|token| target_from_scene(scene, token))
        .collect();
    for target in action_targets(action) {
        if *target != Target::Surface && !grounded.contains(target) {
            return Err(GroundingError::Rejected(format!(
                "rule_step target '{target:?}' is not one of the listed scene targets"
            )));
        }
        if !matches!(
            action,
            ResolvedAction::Capture { .. }
                | ResolvedAction::AssertText { .. }
                | ResolvedAction::AssertCaptured { .. }
                | ResolvedAction::AssertCount { .. }
                | ResolvedAction::AssertChecked { .. }
                | ResolvedAction::AssertEnabled { .. }
                | ResolvedAction::AssertAttribute { .. }
                | ResolvedAction::AssertStyle { .. }
                | ResolvedAction::AssertPresence { .. }
        ) && !actionable_tokens
            .iter()
            .any(|token| target_from_scene(scene, token).as_ref() == Some(target))
        {
            return Err(GroundingError::Rejected(format!(
                "rule_step target '{target:?}' is readable but not actionable"
            )));
        }
    }
    match action {
        ResolvedAction::TypeFocused { .. } => Err(GroundingError::Rejected(
            "rule_step cannot use targetless typing; choose a listed field target".into(),
        )),
        ResolvedAction::Drag { .. } => Err(GroundingError::Rejected(
            "rule_step cannot smuggle a drag past the spec's adjacent-assertion check; use the grounded drag action for a human step beginning with 'Drag'".into(),
        )),
        ResolvedAction::Capture { name, .. } if captures.contains(name) => {
            Err(GroundingError::Rejected(format!(
                "capture '{name}' is already in scope; choose a distinct name"
            )))
        }
        ResolvedAction::TypeText { text, .. } if text.contains("${captured.") => {
            Err(GroundingError::Rejected(
                "rule_step cannot type a capture reference; use type_captured".into(),
            ))
        }
        _ => Ok(()),
    }
}

fn parse_and_ground(
    reply: &str,
    targets: &[String],
    scene: &str,
    app: &str,
    intent: &str,
    captures: &[String],
) -> Result<Vec<ResolvedAction>, GroundingError> {
    // Tolerate models that wrap JSON in a code fence despite instructions.
    let trimmed = reply
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    let authored: AuthoredAction = serde_json::from_str(trimmed)
        .map_err(|e| GroundingError::Rejected(format!("reply is not valid JSON: {e}")))?;

    if authored.action == "capture_ambiguity" {
        if captures.len() < 2 {
            return Err(GroundingError::Rejected(
                "capture_ambiguity needs at least two remembered captures in scope".into(),
            ));
        }
        return Err(GroundingError::CaptureAmbiguity(CaptureAmbiguity::new(
            intent,
            captures.to_vec(),
        )));
    }

    if authored.action == "rule_step" {
        let step = authored
            .step
            .filter(|step| !step.trim().is_empty())
            .ok_or("rule_step needs a non-empty 'step'")?;
        let actions = resolve_step(app, &SpecStep::Plain(step)).map_err(|error| {
            GroundingError::Rejected(format!("rule_step was rejected: {error}"))
        })?;
        if actions.is_empty() {
            return Err(GroundingError::Rejected(
                "rule_step resolved to no actions".into(),
            ));
        }
        let actionable = scene_actionable_targets(scene);
        for action in &actions {
            validate_rule_action(action, scene, targets, &actionable, captures)?;
        }
        return Ok(actions);
    }

    if authored.action == "press_key" {
        let key = authored
            .key
            .filter(|key| !key.trim().is_empty())
            .ok_or("press_key needs a non-empty 'key'")?;
        return Ok(vec![ResolvedAction::PressKey {
            key,
            modifiers: Vec::new(),
        }]);
    }

    let token = authored.target.trim();
    let asserting = authored.action == "assert_text";
    // "body" is the legacy web spelling of the whole readable surface.
    let target = if token == "surface" || (asserting && token == "body") {
        if !asserting {
            return Err("'surface' is only a valid target for assert_text".into());
        }
        Target::Surface
    } else if targets.iter().any(|t| t == token) {
        target_from_scene(scene, token).ok_or_else(|| {
            GroundingError::Rejected(format!(
                "listed target '{token}' is not a well-formed token"
            ))
        })?
    } else if targets.iter().any(|t| t == &format!("css:{token}")) {
        // Old-style reply echoing a bare css selector from a legacy scene.
        Target::css(token)
    } else {
        return Err(GroundingError::Rejected(format!(
            "target '{token}' is not one of the listed elements"
        )));
    };
    match authored.action.as_str() {
        "click" => {
            if !scene_token_is_actionable(scene, token) {
                return Err("click target is readable but not actionable".into());
            }
            Ok(vec![ResolvedAction::Press {
                target,
                label: scene_label(scene, token).unwrap_or_default(),
                dialog: None,
            }])
        }
        "click_at" => {
            if !scene_token_is_actionable(scene, token) {
                return Err("click_at target is readable but not actionable".into());
            }
            let x_pct = authored
                .x_pct
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                .ok_or("click_at needs 'x_pct' from 0 through 100")?;
            let y_pct = authored
                .y_pct
                .filter(|value| value.is_finite() && (0.0..=100.0).contains(value))
                .ok_or("click_at needs 'y_pct' from 0 through 100")?;
            Ok(vec![ResolvedAction::ClickAt {
                target,
                x_pct,
                y_pct,
            }])
        }
        "drag" => {
            if !scene_token_is_actionable(scene, token) {
                return Err("drag source is readable but not actionable".into());
            }
            let onto_token = authored
                .onto
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or("drag needs a non-empty 'onto' target")?;
            if !targets.iter().any(|candidate| candidate == onto_token) {
                return Err(GroundingError::Rejected(format!(
                    "drag target '{onto_token}' is not one of the listed elements"
                )));
            }
            if !scene_token_is_actionable(scene, onto_token) {
                return Err("drag destination is readable but not actionable".into());
            }
            let onto = target_from_scene(scene, onto_token).ok_or_else(|| {
                GroundingError::Rejected(format!(
                    "listed drag target '{onto_token}' is not a well-formed token"
                ))
            })?;
            Ok(vec![ResolvedAction::Drag {
                label: scene_label(scene, token).unwrap_or_default(),
                target,
                onto_label: scene_label(scene, onto_token).unwrap_or_default(),
                onto,
            }])
        }
        "type_text" => {
            let text = authored
                .text
                .filter(|t| !t.is_empty())
                .ok_or("type_text needs a non-empty 'text'")?;
            if text.contains("${captured.") {
                return Err(GroundingError::Rejected(
                    "type_text cannot contain a capture reference; use type_captured".into(),
                ));
            }
            if !scene_token_is_actionable(scene, token) {
                return Err("type_text target is readable but not actionable".into());
            }
            Ok(vec![ResolvedAction::TypeText { target, text }])
        }
        "capture_text" | "capture_count" => {
            let name = authored
                .name
                .filter(|name| valid_capture_name(name))
                .ok_or("capture_text needs a safe 'name' matching [a-z][a-z0-9_]*")?;
            if captures.contains(&name) {
                return Err(GroundingError::Rejected(format!(
                    "capture '{name}' is already in scope; choose a distinct name"
                )));
            }
            Ok(vec![ResolvedAction::Capture {
                target,
                name,
                count: authored.action == "capture_count",
            }])
        }
        "select_options" => {
            if !scene_token_is_actionable(scene, token) {
                return Err("select_options target is readable but not actionable".into());
            }
            let values = authored.values.filter(|values| {
                !values.is_empty() && values.iter().all(|value| !value.is_empty())
            });
            Ok(vec![ResolvedAction::SelectOptions {
                target,
                values: values.ok_or("select_options needs a non-empty 'values' array")?,
            }])
        }
        "select_option" => {
            if !scene_token_is_actionable(scene, token) {
                return Err("select_option target is readable but not actionable".into());
            }
            let text = authored
                .text
                .filter(|text| !text.is_empty())
                .ok_or("select_option needs a non-empty 'text'")?;
            Ok(vec![ResolvedAction::TypeText { target, text }])
        }
        "scroll" => Ok(vec![ResolvedAction::Scroll {
            target: Some(target),
            to: crate::rules::ScrollTo::Offset(
                authored.to_px.ok_or("scroll needs a 'to_px' offset")?,
            ),
        }]),
        "type_captured" => {
            let name = authored
                .capture
                .filter(|name| valid_capture_name(name))
                .ok_or("type_captured needs a safe 'capture' matching [a-z][a-z0-9_]*")?;
            if !captures.contains(&name) {
                return Err(GroundingError::Rejected(format!(
                    "capture '{name}' is not one of the remembered captures in scope"
                )));
            }
            if !scene_token_is_actionable(scene, token) {
                return Err("type_captured target is readable but not actionable".into());
            }
            if captures.len() > 1 {
                if has_capture_pronoun(intent) {
                    return Err(GroundingError::CaptureAmbiguity(CaptureAmbiguity::new(
                        intent,
                        captures.to_vec(),
                    )));
                }
                let mentioned = mentioned_captures(intent, captures);
                match mentioned.as_slice() {
                    [only] if only == &name => {}
                    [only] => {
                        return Err(GroundingError::Rejected(format!(
                            "the step names capture '{only}', not '{name}'"
                        )))
                    }
                    [] => {
                        return Err(GroundingError::CaptureAmbiguity(CaptureAmbiguity::new(
                            intent,
                            captures.to_vec(),
                        )))
                    }
                    many => {
                        return Err(GroundingError::CaptureAmbiguity(CaptureAmbiguity::new(
                            intent,
                            many.to_vec(),
                        )))
                    }
                }
            }
            Ok(vec![ResolvedAction::TypeText {
                target,
                text: format!("${{captured.{name}}}"),
            }])
        }
        "assert_text" => {
            let expected = authored
                .expected
                .filter(|t| !t.is_empty())
                .ok_or("assert_text needs a non-empty 'expected'")?;
            let matcher = if authored.contains.unwrap_or(true) {
                crate::rules::TextMatch::Contains
            } else {
                crate::rules::TextMatch::Equals
            };
            Ok(vec![ResolvedAction::AssertText {
                target,
                expected,
                matcher,
                timeout_ms: crate::rules::ASSERT_TIMEOUT_MS,
            }])
        }
        other => Err(GroundingError::Rejected(format!(
            "unknown action '{other}'"
        ))),
    }
}

/// Author one step. One retry with the failure appended, then a clear error.
pub fn author_steps<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<Vec<ResolvedAction>, AgentError> {
    let targets = scene_targets(ctx.scene);
    if let Some(name) = ctx.captures.iter().find(|name| !valid_capture_name(name)) {
        return Err(AgentError::Authoring {
            step: ctx.intent.to_string(),
            reason: format!(
                "capture '{name}' in the authoring context is not a safe name matching [a-z][a-z0-9_]*"
            ),
        });
    }
    let prompt = user_prompt(ctx);
    let mut last_error = String::new();
    for attempt in 0..2 {
        let user = if attempt == 0 {
            prompt.clone()
        } else {
            format!("{prompt}\n\nYour previous reply was rejected: {last_error}. Reply again with ONLY the corrected JSON object.")
        };
        let reply = client.complete(SYSTEM_PROMPT, &user)?;
        match parse_and_ground(
            &reply,
            &targets,
            ctx.scene,
            ctx.app,
            ctx.intent,
            ctx.captures,
        ) {
            Ok(actions) => return Ok(actions),
            Err(GroundingError::CaptureAmbiguity(ambiguity)) => {
                return Err(AgentError::Authoring {
                    step: ctx.intent.to_string(),
                    reason: ambiguity.reason(),
                })
            }
            Err(reason) => last_error = reason.to_string(),
        }
    }
    Err(AgentError::Authoring {
        step: ctx.intent.to_string(),
        reason: last_error,
    })
}

/// Backward-compatible single-action authoring API. Recording uses
/// [`author_steps`] so deterministic grammar expansions can persist as a
/// sequence, while callers of this older surface still get one action.
pub fn author_step<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<ResolvedAction, AgentError> {
    let mut actions = author_steps(client, ctx)?;
    if actions.len() != 1 {
        return Err(AgentError::Authoring {
            step: ctx.intent.to_string(),
            reason: format!(
                "authored step expanded to {} actions; use author_steps",
                actions.len()
            ),
        });
    }
    Ok(actions.remove(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted {
        replies: Vec<String>,
        calls: usize,
    }

    impl ModelClient for Scripted {
        fn complete(&mut self, _system: &str, _user: &str) -> Result<String, AgentError> {
            let reply = self.replies.get(self.calls).cloned().unwrap_or_default();
            self.calls += 1;
            Ok(reply)
        }

        fn identity(&self) -> (String, String) {
            ("scripted".into(), "test".into())
        }
    }

    const SCENE: &str = r##"[
        {"target":"css:#name","css":"#name","tag":"input","label":"Your name"},
        {"target":"css:#greet","css":"#greet","tag":"button","text":"Greet"}
    ]"##;

    /// A desktop (UIA) scene: native ids and text anchors, no css anywhere.
    const UIA_SCENE: &str = r##"[
        {"target":"id:15","control_type":"Edit","text":"Text editor"},
        {"target":"text:Close","control_type":"Button","text":"Close"}
    ]"##;

    const SCOPED_SCENE: &str = r##"[
        {
          "target":"scoped:css:div.row.propertyGrid containing \"order id\" > css:div.col-md-4.border:not(.bg-info)",
          "css":"body > div:nth-of-type(3) > div:nth-of-type(2)",
          "tag":"div",
          "actionable":false,
          "text":"1092875",
          "scope":{
            "container":"css:div.row.propertyGrid",
            "anchor":"order id",
            "inner":"css:div.col-md-4.border:not(.bg-info)"
          }
        }
    ]"##;

    const HUMAN_PRIMITIVE_SCENE: &str = r##"[
        {"target":"css:#task-1","tag":"tr","actionable":true,"text":"task 1"},
        {"target":"css:#todo","tag":"tbody","actionable":true,"label":"todo drop area"},
        {"target":"css:#half","tag":"button","actionable":true,"text":"Click into my right half"},
        {"target":"css:#rows tr","tag":"collection","actionable":false,"count":7,"label":"displayed table rows"},
        {"target":"css:#methods","tag":"select","actionable":true,"label":"testing methods"},
        {
          "target":"framed:\"container\" > css:body",
          "tag":"body",
          "actionable":false,
          "label":"scroll surface",
          "scope":{"frame":"container","inner":"css:body"}
        },
        {
          "target":"framed:\"container\" > css:#textfield",
          "tag":"input",
          "actionable":true,
          "label":"text field",
          "scope":{"frame":"container","inner":"css:#textfield"}
        }
    ]"##;

    fn ctx<'a>() -> AuthorContext<'a> {
        AuthorContext {
            flow_name: "Greet",
            app: "web",
            url: Some("file:///greeter.html"),
            prior_steps: &[],
            intent: "Put Ada into the box labelled name",
            scene: SCENE,
            captures: &[],
        }
    }

    #[test]
    fn prompt_contract_is_semantic_and_framework_grounded() {
        assert!(SYSTEM_PROMPT.contains("Interpret the user's meaning"));
        assert!(SYSTEM_PROMPT.contains("terse, conversational, indirect, or use synonyms"));
        assert!(SYSTEM_PROMPT.contains("user never needs to provide selectors"));
        assert!(SYSTEM_PROMPT.contains("Never invent a selector"));

        for instruction in [
            "Put Ada in the name field",
            "The person's name should be Ada",
            "Could you fill in Ada where this page asks who I am?",
            "name = Ada please",
        ] {
            let prompt = user_prompt(&AuthorContext {
                intent: instruction,
                ..ctx()
            });
            assert!(
                prompt.contains(&format!("Current step to perform: {instruction}")),
                "the raw wording must reach the semantic model unchanged"
            );
            assert!(
                prompt.contains("css:#name"),
                "the live deterministic target inventory must accompany the intent"
            );
        }
    }

    #[test]
    fn happy_path_grounds_to_listed_element() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##.into()],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "Ada".into()
            }
        );
        assert_eq!(client.calls, 1);
    }

    #[test]
    fn uia_scene_grounds_native_id_and_text_tokens() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target":"id:15","text":"hello"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                app: "notepad",
                url: None,
                scene: UIA_SCENE,
                ..ctx()
            },
        )
        .expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::id("15"),
                text: "hello".into()
            }
        );

        let mut client = Scripted {
            replies: vec![r##"{"action":"click","target":"text:Close"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                app: "notepad",
                url: None,
                scene: UIA_SCENE,
                ..ctx()
            },
        )
        .expect("authored");
        assert!(matches!(
            action,
            ResolvedAction::Press {
                ref target,
                ref label,
                ..
            } if *target == Target::text("Close") && label == "Close"
        ));
    }

    #[test]
    fn legacy_reply_and_scene_shapes_still_ground() {
        // Old-style scene (css key only) + old-style reply (target_css field,
        // bare selector): both sides of the legacy contract keep working.
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target_css":"#name","text":"Ada"}"##.into()],
            calls: 0,
        };
        let legacy_scene = r##"[{"css":"#name","tag":"input"}]"##;
        let action = author_step(
            &mut client,
            &AuthorContext {
                scene: legacy_scene,
                ..ctx()
            },
        )
        .expect("authored");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "Ada".into()
            }
        );
    }

    #[test]
    fn invalid_json_gets_one_retry() {
        let mut client = Scripted {
            replies: vec![
                "sure! here's the JSON you asked for".into(),
                r##"```json
{"action":"click","target":"css:#greet"}
```"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored on retry");
        assert!(matches!(action, ResolvedAction::Press { .. }));
        assert_eq!(client.calls, 2);
    }

    #[test]
    fn invented_selectors_are_rejected() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"click","target":"css:#made-up"}"##.into(),
                r##"{"action":"click","target":"id:not-listed"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("ungrounded must fail");
        assert!(err.to_string().contains("not one of the listed elements"));
        assert_eq!(client.calls, 2, "exactly one retry");
    }

    #[test]
    fn assert_on_surface_is_allowed() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"assert_text","target":"surface","expected":"Hello, Ada","contains":true}"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert_eq!(
            action,
            ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "Hello, Ada".into(),
                matcher: crate::rules::TextMatch::Contains,
                timeout_ms: crate::rules::ASSERT_TIMEOUT_MS,
            }
        );
    }

    #[test]
    fn legacy_body_alias_maps_to_surface() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"assert_text","target_css":"body","expected":"Hello, Ada"}"##.into(),
            ],
            calls: 0,
        };
        let action = author_step(&mut client, &ctx()).expect("authored");
        assert!(
            matches!(action, ResolvedAction::AssertText { ref target, .. } if *target == Target::Surface)
        );
    }

    #[test]
    fn surface_is_assert_only() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"click","target":"surface"}"##.into(),
                r##"{"action":"click","target":"surface"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("surface click must fail");
        assert!(err
            .to_string()
            .contains("only a valid target for assert_text"));
    }

    #[test]
    fn prompt_lists_capture_names_without_values() {
        let captures = vec!["order_number".into(), "customer_id".into()];
        let prompt = user_prompt(&AuthorContext {
            captures: &captures,
            ..ctx()
        });
        assert!(prompt.contains(r#"Remembered captures in scope: ["order_number","customer_id"]"#));
    }

    #[test]
    fn capture_text_grounds_to_a_listed_target() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"capture_text","target":"css:#greet","name":"greeting"}"##.into(),
            ],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Remember the greeting as greeting",
                ..ctx()
            },
        )
        .expect("capture authored");
        assert_eq!(
            action,
            ResolvedAction::Capture {
                target: Target::css("#greet"),
                name: "greeting".into(),
                count: false,
            }
        );
    }

    #[test]
    fn capture_text_grounds_a_scoped_scene_token_without_persisting_it() {
        let token = r#"scoped:css:div.row.propertyGrid containing "order id" > css:div.col-md-4.border:not(.bg-info)"#;
        let mut client = Scripted {
            replies: vec![format!(
                r##"{{"action":"capture_text","target":{},"name":"order_id"}}"##,
                serde_json::to_string(token).expect("token serializes")
            )],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Remember the value beside \"order id\" as the order ID",
                scene: SCOPED_SCENE,
                ..ctx()
            },
        )
        .expect("scoped capture authored");
        assert_eq!(
            action,
            ResolvedAction::Capture {
                target: Target::Scoped {
                    container: "css:div.row.propertyGrid".into(),
                    anchor: "order id".into(),
                    also: Vec::new(),
                    inner: Box::new(Target::css("div.col-md-4.border:not(.bg-info)")),
                },
                name: "order_id".into(),
                count: false,
            }
        );
    }

    #[test]
    fn human_language_primitives_ground_without_rules_in_the_input() {
        let cases = [
            (
                "Drag task 1 into the todo drop area",
                r#"{"action":"drag","target":"css:#task-1","onto":"css:#todo"}"#,
                ResolvedAction::Drag {
                    target: Target::css("#task-1"),
                    label: "task 1".into(),
                    onto: Target::css("#todo"),
                    onto_label: "todo drop area".into(),
                },
            ),
            (
                "Click the right half of \"Click into my right half\"",
                r#"{"action":"click_at","target":"css:#half","x_pct":75,"y_pct":50}"#,
                ResolvedAction::ClickAt {
                    target: Target::css("#half"),
                    x_pct: 75.0,
                    y_pct: 50.0,
                },
            ),
            (
                "Remember the number of displayed table rows as the row count",
                r#"{"action":"capture_count","target":"css:#rows tr","name":"row_count"}"#,
                ResolvedAction::Capture {
                    target: Target::css("#rows tr"),
                    name: "row_count".into(),
                    count: true,
                },
            ),
            (
                "Choose WebDriver in the second dropdown",
                r#"{"action":"select_option","target":"css:#methods","text":"WebDriver"}"#,
                ResolvedAction::TypeText {
                    target: Target::css("#methods"),
                    text: "WebDriver".into(),
                },
            ),
            (
                "Select Functional, End2End, GUI, and Exploratory testing together",
                r#"{"action":"select_options","target":"css:#methods","values":["Functional testing","End2End testing","GUI testing","Exploratory testing"]}"#,
                ResolvedAction::SelectOptions {
                    target: Target::css("#methods"),
                    values: vec![
                        "Functional testing".into(),
                        "End2End testing".into(),
                        "GUI testing".into(),
                        "Exploratory testing".into(),
                    ],
                },
            ),
            (
                "Scroll the embedded challenge to 147 pixels",
                r#"{"action":"scroll","target":"framed:\"container\" > css:body","to_px":147}"#,
                ResolvedAction::Scroll {
                    target: Some(Target::Framed {
                        frame: "container".into(),
                        inner: Box::new(Target::css("body")),
                    }),
                    to: crate::rules::ScrollTo::Offset(147),
                },
            ),
            (
                "Enter Tosca in the text field inside the embedded challenge",
                r#"{"action":"type_text","target":"framed:\"container\" > css:#textfield","text":"Tosca"}"#,
                ResolvedAction::TypeText {
                    target: Target::Framed {
                        frame: "container".into(),
                        inner: Box::new(Target::css("#textfield")),
                    },
                    text: "Tosca".into(),
                },
            ),
            (
                "Move focus to the next field",
                r#"{"action":"press_key","key":"Tab"}"#,
                ResolvedAction::PressKey {
                    key: "Tab".into(),
                    modifiers: Vec::new(),
                },
            ),
        ];

        for (intent, reply, expected) in cases {
            assert!(
                !intent.contains("rules:") && !intent.contains("css:") && !intent.contains("id:"),
                "the human input must stay free of authoring syntax"
            );
            let mut client = Scripted {
                replies: vec![reply.into()],
                calls: 0,
            };
            let action = author_step(
                &mut client,
                &AuthorContext {
                    intent,
                    scene: HUMAN_PRIMITIVE_SCENE,
                    ..ctx()
                },
            )
            .unwrap_or_else(|error| panic!("{intent}: {error}"));
            assert_eq!(action, expected, "{intent}");
        }
    }

    #[test]
    fn rule_step_covers_grounded_actions_outside_the_small_json_vocabulary() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"rule_step","step":"Clear the \"css:#name\" field"}"##.into(),
            ],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Empty the name field",
                ..ctx()
            },
        )
        .expect("grounded deterministic action authored");
        assert_eq!(
            action,
            ResolvedAction::Clear {
                target: Target::css("#name")
            }
        );
    }

    #[test]
    fn rule_step_rejects_an_invented_selector() {
        let reply = r##"{"action":"rule_step","step":"Clear the \"css:#invented\" field"}"##;
        let mut client = Scripted {
            replies: vec![reply.into(), reply.into()],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("invented target must fail");
        assert!(err
            .to_string()
            .contains("not one of the listed scene targets"));
    }

    #[test]
    fn rule_step_allows_a_valid_targetless_action_on_an_empty_scene() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"rule_step","step":"Go to /settings"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Open settings",
                scene: "[]",
                ..ctx()
            },
        )
        .expect("navigation needs no element target");
        assert_eq!(
            action,
            ResolvedAction::Navigate {
                path: "/settings".into()
            }
        );
    }

    #[test]
    fn capture_text_rejects_unsafe_names() {
        let reply = r##"{"action":"capture_text","target":"css:#greet","name":"Order Number"}"##;
        let mut client = Scripted {
            replies: vec![reply.into(), reply.into()],
            calls: 0,
        };
        let err = author_step(&mut client, &ctx()).expect_err("unsafe name must fail");
        assert!(err.to_string().contains("[a-z][a-z0-9_]*"));
        assert_eq!(client.calls, 2, "ordinary schema rejection gets one retry");
    }

    #[test]
    fn type_captured_emits_the_existing_reference() {
        let captures = vec!["order_number".into()];
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"type_captured","target":"css:#name","capture":"order_number"}"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter it in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect("single capture makes the pronoun unambiguous");
        assert_eq!(
            action,
            ResolvedAction::TypeText {
                target: Target::css("#name"),
                text: "${captured.order_number}".into(),
            }
        );
    }

    #[test]
    fn ordinary_type_text_remains_literal_when_captures_exist() {
        let captures = vec!["order_number".into()];
        let mut client = Scripted {
            replies: vec![r##"{"action":"type_text","target":"css:#name","text":"it"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Type the literal word it in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect("literal authored");
        assert!(matches!(
            action,
            ResolvedAction::TypeText { ref text, .. } if text == "it"
        ));
    }

    #[test]
    fn type_text_cannot_smuggle_a_capture_reference_past_ambiguity_checks() {
        let captures = vec!["customer_id".into(), "order_number".into()];
        let reply =
            r##"{"action":"type_text","target":"css:#name","text":"${captured.order_number}"}"##;
        let mut client = Scripted {
            replies: vec![reply.into(), reply.into()],
            calls: 0,
        };
        let err = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter it in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect_err("capture references must use the guarded action");
        assert!(err.to_string().contains("use type_captured"));
    }

    #[test]
    fn an_existing_capture_name_cannot_be_silently_overwritten() {
        let captures = vec!["order_number".into()];
        let reply = r##"{"action":"capture_text","target":"css:#greet","name":"order_number"}"##;
        let mut client = Scripted {
            replies: vec![reply.into(), reply.into()],
            calls: 0,
        };
        let err = author_step(
            &mut client,
            &AuthorContext {
                intent: "Remember the customer number",
                captures: &captures,
                ..ctx()
            },
        )
        .expect_err("model-authored captures need distinct names");
        assert!(err.to_string().contains("already in scope"));
    }

    #[test]
    fn an_unnamed_reference_with_multiple_captures_is_structured_ambiguity() {
        let captures = vec!["customer_id".into(), "order_number".into()];
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"type_captured","target":"css:#name","capture":"order_number"}"##
                    .into(),
            ],
            calls: 0,
        };
        let err = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter it in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect_err("the model may not guess which capture 'it' means");
        let reason = match err {
            AgentError::Authoring { reason, .. } => reason,
            other => panic!("unexpected error: {other}"),
        };
        let ambiguity: CaptureAmbiguity =
            serde_json::from_str(&reason).expect("machine-readable ambiguity");
        assert_eq!(ambiguity.kind, CAPTURE_AMBIGUITY_KIND);
        assert_eq!(ambiguity.reference, "Enter it in the name field");
        assert_eq!(ambiguity.candidates, vec!["customer_id", "order_number"]);
        assert_eq!(
            client.calls, 1,
            "ambiguity stops instead of retrying into a guess"
        );
    }

    #[test]
    fn destination_words_do_not_disambiguate_a_pronoun() {
        let captures = vec!["name".into(), "order_number".into()];
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"type_captured","target":"css:#name","capture":"name"}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter it in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect_err("the destination name must not choose the capture");
        let AgentError::Authoring { reason, .. } = err else {
            panic!("expected authoring ambiguity")
        };
        let ambiguity: CaptureAmbiguity = serde_json::from_str(&reason).expect("structured");
        assert_eq!(ambiguity.candidates, vec!["name", "order_number"]);
    }

    #[test]
    fn bare_pronouns_do_not_disambiguate_from_destination_words() {
        for pronoun in ["that", "this", "them"] {
            let mut client = Scripted {
                replies: vec![format!(
                    r##"{{"action":"type_captured","target":"css:#name","capture":"name"}}"##
                )],
                calls: 0,
            };
            let captures = vec!["name".into(), "order_number".into()];
            let error = author_step(
                &mut client,
                &AuthorContext {
                    flow_name: "f",
                    app: "web",
                    url: None,
                    prior_steps: &[],
                    intent: &format!("Enter {pronoun} in the name field"),
                    scene: r##"[{"target":"css:#name"}]"##,
                    captures: &captures,
                },
            )
            .expect_err("a bare pronoun with multiple captures must be ambiguous");
            assert!(error.to_string().contains(CAPTURE_AMBIGUITY_KIND));
        }
    }

    #[test]
    fn rule_step_rejects_targetless_typing() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"rule_step","step":"Type Alice"}"##.into(),
                r##"{"action":"rule_step","step":"Type Alice"}"##.into(),
            ],
            calls: 0,
        };
        let error = author_step(
            &mut client,
            &AuthorContext {
                flow_name: "f",
                app: "web",
                url: None,
                prior_steps: &[],
                intent: "Enter Alice in the email field",
                scene: r##"[{"target":"css:#email"}]"##,
                captures: &[],
            },
        )
        .expect_err("model-authored typing must name a grounded target");
        assert!(error.to_string().contains("targetless typing"));
    }

    #[test]
    fn rule_step_can_expand_one_intent_into_multiple_grounded_actions() {
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"rule_step","step":"Replace the \"css:#name\" field with Ada"}"##
                    .into(),
            ],
            calls: 0,
        };
        let actions = author_steps(
            &mut client,
            &AuthorContext {
                flow_name: "f",
                app: "web",
                url: None,
                prior_steps: &[],
                intent: "Put Ada in the name field, replacing what is there",
                scene: r##"[{"target":"css:#name","actionable":true}]"##,
                captures: &[],
            },
        )
        .expect("the deterministic expansion remains grounded");
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[0], ResolvedAction::Clear { .. }));
        assert!(matches!(actions[1], ResolvedAction::TypeText { .. }));
    }

    #[test]
    fn model_controlled_ambiguity_text_is_ignored() {
        let captures = vec!["customer_id".into(), "order_number".into()];
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"capture_ambiguity","reference":"SECRET-ON-SCREEN","candidates":["customer_id","order_number"]}"##.into(),
            ],
            calls: 0,
        };
        let err = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter it in the confirmation field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect_err("model-declared ambiguity stops");
        let AgentError::Authoring { reason, .. } = err else {
            panic!("expected authoring ambiguity")
        };
        let ambiguity: CaptureAmbiguity = serde_json::from_str(&reason).expect("structured");
        assert_eq!(ambiguity.reference, "Enter it in the confirmation field");
        assert!(!reason.contains("SECRET-ON-SCREEN"));
    }

    #[test]
    fn an_explicit_capture_name_disambiguates_multiple_captures() {
        let captures = vec!["customer_id".into(), "order_number".into()];
        let mut client = Scripted {
            replies: vec![
                r##"{"action":"type_captured","target":"css:#name","capture":"order_number"}"##
                    .into(),
            ],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                intent: "Enter the order number in the name field",
                captures: &captures,
                ..ctx()
            },
        )
        .expect("the named capture is unambiguous");
        assert!(matches!(
            action,
            ResolvedAction::TypeText { ref text, .. } if text == "${captured.order_number}"
        ));
    }
}
