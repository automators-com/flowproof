//! The LLM authoring loop: given a natural-language step and the live app's
//! scene graph, ask a model for the actions that carry that step out against
//! elements it can see. A step is a unit of INTENT, not a unit of work — "fill
//! in the vehicle data and continue" is one step and a dozen actions — so the
//! reply may be a sequence. The model must pick every target from the offered
//! scene — it cannot invent selectors — and each chosen action is then
//! performed and verified by the recorder exactly like a rules-authored one.

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
- A step is a unit of INTENT, not a single click. One step may need many actions - \
\"fill in the vehicle data and continue\" is one step covering every field on that \
form plus the button. CARRY OUT THE WHOLE STEP: reply with a JSON ARRAY of action \
objects, in the order they must happen. Reply with a bare object only when the step \
really is one action. Author the whole step in one reply: plan it as a whole, in \
order, rather than leaving the rest for later. If the step plainly needs elements \
that are NOT on this screen yet - a later wizard page, a section one of your own \
actions will reveal - author every action you can ground here, then end the array \
with {\"action\":\"step_continues\"}. flowproof will perform them, read the screen \
again and ask you for the rest. Never guess a target for something you cannot see.
- Before authoring, read what the screen says. A wizard will happily move on \
from a section it is still flagging - a heading carrying a count of outstanding \
problems, a message naming a field it would not accept - and that is the PREVIOUS \
step failing, not this one. When you see it, author nothing: reply with exactly \
{\"action\":\"previous_step_incomplete\",\"evidence\":\"<the page's own words>\"}. \
A page that moved on is not proof that it accepted anything.
- You may be asked to CONTINUE a step that is partway done, because the page \
changed under the plan - a control vanished, or something covered it. That message \
lists the actions already performed, which you must NEVER repeat, and says why the \
next one was refused. The elements it lists are the screen as it is NOW, read again \
after those actions ran. Reply with only the actions still needed. If the step's \
intent is already satisfied, reply with exactly [{\"action\":\"step_complete\"}] - a \
reply valid ONLY when you were asked to continue, never in a first reply.
- When an action was refused because another element would receive the click, do \
what a person would: if something is covering the control, dismiss it (its close \
control, or press_key Escape) and continue. If the control simply cannot be reached, \
choose a DIFFERENT listed target that serves the same intent - a navigation link \
rather than a banner button, say - rather than retrying the one that was refused.
- When a step asks for a whole form, group or screen, cover EVERY field it names, \
including ones the user did not enumerate. Each scene entry tells you what a field \
holds now (`value`, `checked`), whether the page demands it (`required`), the \
constraint the page states in its own words (`rule` - obey it, it is what the \
application will enforce), whether the page has already REJECTED what it holds \
(`invalid` - correct every one of those before moving on), and, for a dropdown, its \
exact `options` - choose an option verbatim from that list. Invent \
plausible, valid data for fields the step leaves unspecified, and leave a field alone \
when it already holds a value the step does not contradict.
- If the step's goal cannot be reached without data the screen requires first - a \
submit button behind mandatory fields - include the actions that supply it, then the \
action that reaches the goal.
- Respond with ONLY JSON, no prose, no code fences.
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
    /// Only for `previous_step_incomplete`: the page's own words, quoted
    /// back as the evidence a person needs to judge the report.
    #[serde(default)]
    evidence: Option<String>,
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
    /// The date the application believes it is, `YYYY-MM-DD`, when the
    /// driver can tell. A flow may pin the browser clock, so a date the
    /// model invents from its own sense of now can be rejected as past.
    pub today: Option<&'a str>,
    /// What the screen currently reads, whitespace-collapsed and bounded.
    /// The step before this one may have left a problem behind that no
    /// element carries a mark for; the page usually says so in words.
    pub page_text: Option<&'a str>,
}

fn user_prompt(ctx: &AuthorContext<'_>) -> String {
    let prior = if ctx.prior_steps.is_empty() {
        "(none)".to_string()
    } else {
        ctx.prior_steps.join("; ")
    };
    let captures = serde_json::to_string(ctx.captures).expect("capture names serialize");
    format!(
        "Flow: {name}\nApp: {app}{url}{today}\nSteps already performed: {prior}\n\
         Remembered captures in scope: {captures}\n\
         Current step to perform: {intent}{page}\n\nInteractable elements:\n{scene}",
        name = ctx.flow_name,
        app = ctx.app,
        url = ctx.url.map(|u| format!(" ({u})")).unwrap_or_default(),
        today = ctx
            .today
            .map(|d| format!(
                "\nToday, as the application sees it, is {d}. Any date you invent must make \
                 sense relative to THAT date, not to your own sense of now - a policy start \
                 date in its past is rejected and the page will refuse to move on."
            ))
            .unwrap_or_default(),
        prior = prior,
        captures = captures,
        intent = ctx.intent,
        page = ctx
            .page_text
            .map(|t| format!("\n\nWhat the screen reads right now:\n\"{t}\""))
            .unwrap_or_default(),
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
    /// A sequence rejected at one action, carrying how far it got. A step
    /// meaning a whole form authors a dozen actions, and re-authoring all of
    /// them from scratch is a coin flip the model has to win twice; knowing
    /// the prefix was accepted turns the retry into a correction.
    Sequence {
        reason: String,
        grounded: usize,
        total: usize,
    },
    CaptureAmbiguity(CaptureAmbiguity),
    /// Not a grounding failure at all: the model read the screen and found
    /// the PREVIOUS step's work disputed by the page. Carries the page's own
    /// words, which are the evidence a person needs to judge it.
    PreviousStepIncomplete(String),
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
            Self::Sequence { reason, .. } => f.write_str(reason),
            Self::CaptureAmbiguity(ambiguity) => f.write_str(&ambiguity.reason()),
            Self::PreviousStepIncomplete(evidence) => write!(
                f,
                "the previous step left a problem behind; the page reports: {evidence}"
            ),
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

/// The most actions one natural step may author. A step that means a whole
/// form legitimately runs to a couple of dozen; a reply an order of magnitude
/// past that is a model that has started looping, and a bound says so at the
/// point of authoring rather than after the run has clicked sixty times.
pub(crate) const MAX_STEP_ACTIONS: usize = 60;

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
    let reply: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| GroundingError::Rejected(format!("reply is not valid JSON: {e}")))?;
    // One step may be one action or a sequence. `{"actions": [...]}` is the
    // shape models reach for unprompted often enough to be worth accepting.
    let authored = match reply {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut fields) => match fields.remove("actions") {
            Some(serde_json::Value::Array(items)) => items,
            Some(_) => {
                return Err(GroundingError::Rejected(
                    "'actions' must be an array of action objects".into(),
                ))
            }
            None => vec![serde_json::Value::Object(fields)],
        },
        other => {
            return Err(GroundingError::Rejected(format!(
                "reply must be an action object or an array of them, not {other}"
            )))
        }
    };
    if authored.is_empty() {
        return Err(GroundingError::Rejected(
            "reply authored no action; a step must do something".into(),
        ));
    }
    if authored.len() > MAX_STEP_ACTIONS {
        return Err(GroundingError::Rejected(format!(
            "reply authored {} actions for one step; the bound is {MAX_STEP_ACTIONS}",
            authored.len()
        )));
    }

    let total = authored.len();
    // A capture made partway through the sequence is in scope for the rest of
    // it, exactly as it would be for a later step.
    let mut scope = captures.to_vec();
    let mut resolved = Vec::new();
    for (index, item) in authored.into_iter().enumerate() {
        let mut actions =
            ground_one(item, targets, scene, app, intent, &scope).map_err(|error| match error {
                GroundingError::Rejected(reason) if total > 1 => GroundingError::Sequence {
                    reason: format!("action {} of {total} was rejected: {reason}", index + 1),
                    grounded: index,
                    total,
                },
                other => other,
            })?;
        for action in &actions {
            if let ResolvedAction::Capture { name, .. } = action {
                scope.push(name.clone());
            }
        }
        resolved.append(&mut actions);
    }
    Ok(resolved)
}

/// Ground ONE model-authored action object. A deterministic `rule_step` may
/// still expand to more than one action, which is why this returns a vector.
fn ground_one(
    item: serde_json::Value,
    targets: &[String],
    scene: &str,
    app: &str,
    intent: &str,
    captures: &[String],
) -> Result<Vec<ResolvedAction>, GroundingError> {
    let authored: AuthoredAction = serde_json::from_value(item)
        .map_err(|e| GroundingError::Rejected(format!("reply is not a valid action: {e}")))?;

    // Answered BEFORE any target is resolved: this is a report about the
    // previous step, not an action, and it names no element. Resolving a
    // target first would reject it as an empty selector and lose the very
    // evidence it was sent to deliver.
    if authored.action == "previous_step_incomplete" {
        return Err(GroundingError::PreviousStepIncomplete(
            authored
                .evidence
                .clone()
                .unwrap_or_else(|| "the page reports an outstanding problem".to_string()),
        ));
    }

    // Answered here for the same reason, and it has to be: `step_complete`
    // names no element either, so the target resolution below reached it
    // first and refused it as "target '' is not one of the listed elements".
    // That tells a model which has just used the vocabulary correctly that
    // the word does not exist, so the retry reaches for something else
    // instead of authoring the step. Named rather than lumped in with typos:
    // reaching for the sentinel in a first reply is having understood the
    // vocabulary and mistaken the moment, and the retry must say which.
    if authored.action == "step_complete" {
        return Err(GroundingError::Rejected(
            "step_complete is valid only when you are asked to CONTINUE a step that is \
             partway done; a first reply must author the actions the step needs"
                .into(),
        ));
    }

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
        // `step_complete` never arrives here: it names no target, so it is
        // answered above, before target resolution could refuse it as an
        // empty selector.
        other => Err(GroundingError::Rejected(format!(
            "unknown action '{other}'"
        ))),
    }
}

/// What the recorder has already done for a step, and why it stopped.
pub struct StepProgress<'a> {
    /// One line per action already performed, in order.
    pub executed: &'a [String],
    /// Why the next planned action was refused.
    pub refusal: &'a str,
    /// What the page currently reads, whitespace-collapsed. A form that
    /// refuses to move on says why, and a verifier that cannot read that
    /// has nothing to weigh.
    pub page_text: &'a str,
}

/// The answer to "what is left of this step?".
pub enum Remainder {
    /// The step's intent is already satisfied; author nothing more.
    Complete,
    /// The actions still needed, grounded against the fresh scene.
    Actions(Vec<ResolvedAction>),
}

/// Tolerate models that wrap JSON in a code fence despite instructions.
fn strip_code_fence(reply: &str) -> &str {
    reply
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim()
}

/// `[{"action":"step_complete"}]` — the one reply that means "nothing left".
/// Recognised before grounding, because `step_complete` names no target and
/// would otherwise be refused as an unknown action.
fn is_step_complete(reply: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(strip_code_fence(reply)) else {
        return false;
    };
    let items = match value {
        serde_json::Value::Array(items) => items,
        object @ serde_json::Value::Object(_) => vec![object],
        _ => return false,
    };
    items.len() == 1 && items[0]["action"].as_str() == Some("step_complete")
}

fn remainder_prompt(ctx: &AuthorContext<'_>, progress: &StepProgress<'_>) -> String {
    let done = if progress.executed.is_empty() {
        "  (nothing yet)".to_string()
    } else {
        progress
            .executed
            .iter()
            .enumerate()
            .map(|(i, line)| format!("  {}. {line}", i + 1))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "{}\n\nThis step is PARTWAY DONE. Already performed - never repeat these:\n{done}\n\
         The next planned action was refused: {}\n\
         The elements listed above are the screen as it is NOW, read again after those \
         actions ran.\nWhat the page currently reads:\n\"{}\"\n\
         Weigh that text before answering. The step is NOT satisfied - and you must \
         author the actions that fix it - if the text names a missing choice, an \
         invalid value or a field still wanted, or if a section heading carries a \
         count of outstanding problems. A page that \
         moved on is not proof: a wizard can advance and still mark the section it \
         left behind. Otherwise author only the actions still needed, or \
         reply with exactly [{{\"action\":\"step_complete\"}}].",
        user_prompt(ctx),
        progress.refusal,
        progress.page_text
    )
}

/// Author what is left of a step after the page changed under its plan.
///
/// The recorder calls this with a FRESH scene, so the model chooses from the
/// screen as it is now rather than the one its plan was built against. This
/// is the whole difference between a planner that must be right first time
/// and one that can look again — the trace it produces is identical either
/// way, because the trace records executed actions, not the deliberation.
pub fn author_remainder<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
    progress: &StepProgress<'_>,
) -> Result<Remainder, AgentError> {
    let targets = scene_targets(ctx.scene);
    let prompt = remainder_prompt(ctx, progress);
    let mut last_error = String::new();
    for attempt in 0..2 {
        let user = if attempt == 0 {
            prompt.clone()
        } else {
            format!(
                "{prompt}\n\nYour previous reply was rejected: {last_error}. \
                 Reply again with ONLY the corrected JSON array."
            )
        };
        let reply = client.complete(SYSTEM_PROMPT, &user)?;
        if is_step_complete(&reply) {
            return Ok(Remainder::Complete);
        }
        match parse_and_ground(
            &reply,
            &targets,
            ctx.scene,
            ctx.app,
            ctx.intent,
            ctx.captures,
        ) {
            Ok(actions) => return Ok(Remainder::Actions(actions)),
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

/// Split a trailing `step_continues` off a reply, if present.
///
/// The sentinel is not an action and never grounds; it is the model saying
/// "this screen had no more of the step on it". Legal only as the LAST
/// element, so it cannot be used to skip work in the middle of a sequence.
fn split_continuation(reply: &str) -> (String, bool) {
    let trimmed = strip_code_fence(reply);
    let Ok(serde_json::Value::Array(mut items)) = serde_json::from_str(trimmed) else {
        return (reply.to_string(), false);
    };
    let continues = items
        .last()
        .and_then(|item| item["action"].as_str())
        .is_some_and(|action| action == "step_continues");
    if !continues {
        return (reply.to_string(), false);
    }
    items.pop();
    (
        serde_json::to_string(&items).unwrap_or_else(|_| reply.to_string()),
        true,
    )
}

/// Author one step, reporting whether the model says it has more to do once
/// the screen changes. [`author_steps`] is the same thing without the flag.
pub fn author_steps_continuable<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<(Vec<ResolvedAction>, bool), AgentError> {
    author_steps_inner(client, ctx)
}

/// Author one step. One retry with the failure appended, then a clear error.
pub fn author_steps<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<Vec<ResolvedAction>, AgentError> {
    author_steps_inner(client, ctx).map(|(actions, _)| actions)
}

fn author_steps_inner<C: ModelClient>(
    client: &mut C,
    ctx: &AuthorContext<'_>,
) -> Result<(Vec<ResolvedAction>, bool), AgentError> {
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
    // How much of the last rejected sequence had already grounded. `None` for
    // a single action, or for a sequence whose very first action was wrong:
    // in neither case is there a correct prefix to stand on.
    let mut kept: Option<(usize, usize)> = None;
    for attempt in 0..2 {
        let user = if attempt == 0 {
            prompt.clone()
        } else if let Some((grounded, total)) = kept {
            format!(
                "{prompt}\n\nYour previous reply was rejected: {last_error}. \
                 The first {grounded} of those {total} actions were correct — repeat them \
                 unchanged, then correct the rest. Every action in the sequence has to ground \
                 or the whole step is refused, so do not drop the remaining ones. \
                 Reply again with ONLY the corrected JSON array."
            )
        } else {
            format!("{prompt}\n\nYour previous reply was rejected: {last_error}. Reply again with ONLY the corrected JSON object.")
        };
        let reply = client.complete(SYSTEM_PROMPT, &user)?;
        let (reply, continues) = split_continuation(&reply);
        match parse_and_ground(
            &reply,
            &targets,
            ctx.scene,
            ctx.app,
            ctx.intent,
            ctx.captures,
        ) {
            Ok(actions) => return Ok((actions, continues)),
            Err(GroundingError::CaptureAmbiguity(ambiguity)) => {
                return Err(AgentError::Authoring {
                    step: ctx.intent.to_string(),
                    reason: ambiguity.reason(),
                })
            }
            // Retrying would only ask the same question of the same screen.
            // This is a report about the step BEFORE this one, and the
            // recorder has to hear it intact.
            Err(GroundingError::PreviousStepIncomplete(evidence)) => {
                return Err(AgentError::PreviousStepIncomplete {
                    step: ctx.intent.to_string(),
                    evidence,
                })
            }
            Err(GroundingError::Sequence {
                reason,
                grounded,
                total,
            }) => {
                last_error = reason;
                kept = (grounded > 0).then_some((grounded, total));
            }
            Err(reason) => {
                last_error = reason.to_string();
                kept = None;
            }
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
            page_text: None,
            today: None,
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
                page_text: None,
                today: None,
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

    /// The scene of a small form, shaped like the live web scene: what each
    /// field holds, what the page demands, and a dropdown's exact options.
    const FORM_SCENE: &str = r##"[
        {"target":"css:#make","tag":"select","actionable":true,"label":"Make","required":true,
         "options":["Audi","BMW","Tesla"]},
        {"target":"css:#seats","tag":"select","actionable":true,"label":"Seats","required":true,
         "options":["1","2","3"]},
        {"target":"css:#plate","tag":"input","actionable":true,"label":"Licence plate","value":""},
        {"target":"css:#next","tag":"button","actionable":true,"text":"Next »"}
    ]"##;

    #[test]
    fn one_step_may_author_the_whole_form_it_names() {
        // The step is one INTENT over several fields. Before this, a reply
        // could only be one action, so nine of ten fields stayed empty and
        // the form's own validation failed the run.
        let mut client = Scripted {
            replies: vec![r##"[
                {"action":"select_option","target":"css:#make","text":"BMW"},
                {"action":"select_option","target":"css:#seats","text":"2"},
                {"action":"type_text","target":"css:#plate","text":"W-12345"},
                {"action":"click","target":"css:#next"}
            ]"##
            .into()],
            calls: 0,
        };
        let actions = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Fill out all the vehicle data and click next",
                scene: FORM_SCENE,
                ..ctx()
            },
        )
        .expect("a whole-form step authors a whole form");
        assert_eq!(
            actions,
            vec![
                ResolvedAction::TypeText {
                    target: Target::css("#make"),
                    text: "BMW".into()
                },
                ResolvedAction::TypeText {
                    target: Target::css("#seats"),
                    text: "2".into()
                },
                ResolvedAction::TypeText {
                    target: Target::css("#plate"),
                    text: "W-12345".into()
                },
                ResolvedAction::Press {
                    target: Target::css("#next"),
                    label: "Next »".into(),
                    dialog: None
                },
            ]
        );
        assert_eq!(client.calls, 1, "one step is still one model call");
    }

    #[test]
    fn a_sequence_grounds_every_action_and_names_the_one_it_rejects() {
        // Partial grounding would be the worst outcome: half a form filled
        // and a trace that looks authored. The step fails, saying which
        // action failed and where in the sequence it was.
        let reply = r##"[
            {"action":"select_option","target":"css:#make","text":"BMW"},
            {"action":"type_text","target":"css:#invented","text":"W-12345"}
        ]"##;
        let mut client = Scripted {
            replies: vec![reply.into(), reply.into()],
            calls: 0,
        };
        let error = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Fill out all the vehicle data",
                scene: FORM_SCENE,
                ..ctx()
            },
        )
        .expect_err("one ungrounded action rejects the sequence");
        let reason = error.to_string();
        assert!(reason.contains("action 2 of 2"), "{reason}");
        assert!(
            reason.contains("not one of the listed elements"),
            "{reason}"
        );
    }

    /// A client that keeps what it was asked, so a test can assert on the
    /// correction rather than only on the outcome.
    struct Recording {
        replies: Vec<String>,
        prompts: Vec<String>,
    }

    impl ModelClient for Recording {
        fn complete(&mut self, _system: &str, user: &str) -> Result<String, AgentError> {
            let reply = self
                .replies
                .get(self.prompts.len())
                .cloned()
                .unwrap_or_default();
            self.prompts.push(user.to_string());
            Ok(reply)
        }

        fn identity(&self) -> (String, String) {
            ("recording".into(), "test".into())
        }
    }

    #[test]
    fn a_rejected_sequence_is_corrected_rather_than_re_authored_from_scratch() {
        // The regression this guards. A step naming a whole form authors a
        // long sequence, and one ungrounded action refuses all of it — so the
        // retry has to re-author every action correctly to land anything at
        // all. On a real form that is a coin flip the model must win twice,
        // and losing it fills FEWER fields than the one-action-per-step
        // behaviour this replaced: not some of the form, none of it.
        //
        // The all-or-nothing accept stands — half a form and a trace that
        // looks authored is still the worst outcome. What changes is that the
        // correction stands on the prefix that already grounded.
        let bad = r##"[
            {"action":"select_option","target":"css:#make","text":"BMW"},
            {"action":"select_option","target":"css:#seats","text":"2"},
            {"action":"type_text","target":"css:#invented","text":"W-12345"}
        ]"##;
        let good = r##"[
            {"action":"select_option","target":"css:#make","text":"BMW"},
            {"action":"select_option","target":"css:#seats","text":"2"},
            {"action":"type_text","target":"css:#plate","text":"W-12345"},
            {"action":"click","target":"css:#next"}
        ]"##;
        let mut client = Recording {
            replies: vec![bad.into(), good.into()],
            prompts: Vec::new(),
        };
        let actions = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Fill out all the vehicle data and click next",
                scene: FORM_SCENE,
                ..ctx()
            },
        )
        .expect("the corrected sequence grounds");
        assert_eq!(actions.len(), 4, "the whole form lands, not a prefix of it");

        let retry = &client.prompts[1];
        assert!(retry.contains("action 3 of 3"), "{retry}");
        assert!(
            retry.contains("The first 2 of those 3 actions were correct"),
            "the correction has to name the prefix it can stand on: {retry}"
        );
        assert!(
            retry.contains("do not drop the remaining ones"),
            "a shorter reply is the failure mode being guarded: {retry}"
        );
        // The sharpest edge of the regression: a sequence was refused and the
        // correction then asked for ONE JSON *object*. A model that complies
        // authors a single action, so a step meaning a whole form came back
        // filling one field — or none.
        assert!(
            !retry.contains("JSON object"),
            "a rejected sequence must not be asked to come back as one action: {retry}"
        );
    }

    #[test]
    fn a_sequence_wrong_from_its_first_action_claims_no_correct_prefix() {
        // Nothing grounded, so there is no prefix to repeat. Telling the model
        // "the first 0 actions were correct" would be noise at best.
        let bad = r##"[
            {"action":"type_text","target":"css:#invented","text":"W-12345"},
            {"action":"click","target":"css:#next"}
        ]"##;
        let mut client = Recording {
            replies: vec![bad.into(), bad.into()],
            prompts: Vec::new(),
        };
        author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Fill out all the vehicle data",
                scene: FORM_SCENE,
                ..ctx()
            },
        )
        .expect_err("nothing grounds");
        let retry = &client.prompts[1];
        assert!(!retry.contains("were correct"), "{retry}");
    }

    #[test]
    fn the_actions_wrapper_shape_is_accepted() {
        let mut client = Scripted {
            replies: vec![r##"{"actions":[{"action":"click","target":"css:#next"}]}"##.into()],
            calls: 0,
        };
        let actions = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Continue",
                scene: FORM_SCENE,
                ..ctx()
            },
        )
        .expect("the wrapper object models reach for is accepted");
        assert!(matches!(actions[..], [ResolvedAction::Press { .. }]));
    }

    #[test]
    fn a_sequence_that_authors_nothing_or_runs_away_is_refused() {
        for (reply, expected) in [
            ("[]", "a step must do something"),
            (
                &format!(
                    "[{}]",
                    vec![r##"{"action":"click","target":"css:#next"}"##; MAX_STEP_ACTIONS + 1]
                        .join(",")
                ),
                "the bound is 60",
            ),
        ] {
            let mut client = Scripted {
                replies: vec![reply.to_string(), reply.to_string()],
                calls: 0,
            };
            let error = author_steps(
                &mut client,
                &AuthorContext {
                    page_text: None,
                    today: None,
                    intent: "Fill out all the vehicle data",
                    scene: FORM_SCENE,
                    ..ctx()
                },
            )
            .expect_err("a degenerate sequence is not a step");
            assert!(error.to_string().contains(expected), "{error}");
        }
    }

    #[test]
    fn a_capture_made_mid_sequence_is_in_scope_for_the_rest_of_it() {
        let mut client = Scripted {
            replies: vec![r##"[
                {"action":"capture_text","target":"css:#greet","name":"greeting"},
                {"action":"type_captured","target":"css:#name","capture":"greeting"}
            ]"##
            .into()],
            calls: 0,
        };
        let actions = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Remember the greeting as the greeting, then put it in the name box",
                ..ctx()
            },
        )
        .expect("the capture is in scope for the action after it");
        assert!(matches!(
            actions[1],
            ResolvedAction::TypeText { ref text, .. } if text == "${captured.greeting}"
        ));
    }

    #[test]
    fn prompt_contract_asks_for_the_whole_step() {
        assert!(SYSTEM_PROMPT.contains("a unit of INTENT, not a single click"));
        assert!(SYSTEM_PROMPT.contains("CARRY OUT THE WHOLE STEP"));
        assert!(SYSTEM_PROMPT.contains("cover EVERY field it names"));
        assert!(
            SYSTEM_PROMPT.contains("`options`"),
            "the model is told a dropdown's options are authoritative"
        );
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
            page_text: None,
            today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                    page_text: None,
                    today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
    fn sap_transaction_intent_can_ground_to_deterministic_navigation() {
        let mut client = Scripted {
            replies: vec![r##"{"action":"rule_step","step":"Go to /nVA01"}"##.into()],
            calls: 0,
        };
        let action = author_step(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                flow_name: "Create an order",
                app: "sap",
                url: None,
                prior_steps: &[],
                intent: "Open transaction VA01 to create a sales order",
                scene: "[]",
                captures: &[],
            },
        )
        .expect("SAP transaction intent needs no element selector");
        assert_eq!(
            action,
            ResolvedAction::Navigate {
                path: "/nVA01".into()
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                    page_text: None,
                    today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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
                page_text: None,
                today: None,
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

    /// A wizard that advanced while still flagging the section behind it is
    /// the PREVIOUS step failing. That report is not a grounding failure and
    /// must not be laundered into one: the recorder has to hear the page's
    /// own words, because they are the only evidence of which step actually
    /// went wrong. Reported as "the model authored something ungroundable"
    /// it names this step, and the real defect is a screen back.
    #[test]
    fn a_previous_step_report_surfaces_with_the_pages_own_words() {
        let mut client = Recording {
            replies: vec![r##"{"action":"previous_step_incomplete",
                     "evidence":"Vehicle data: 2 outstanding problems"}"##
                .into()],
            prompts: Vec::new(),
        };
        let err = author_steps(
            &mut client,
            &AuthorContext {
                page_text: Some("Vehicle data: 2 outstanding problems"),
                today: None,
                intent: "Choose the payment method",
                ..ctx()
            },
        )
        .expect_err("a page still flagging the step before is not authorable");

        let AgentError::PreviousStepIncomplete { step, evidence } = err else {
            panic!("the report must arrive as itself, not as an authoring failure: {err:?}");
        };
        assert_eq!(
            evidence, "Vehicle data: 2 outstanding problems",
            "the page's own words are the evidence, carried verbatim"
        );
        assert_eq!(
            step, "Choose the payment method",
            "and the step named is the one that could not be authored"
        );
    }

    /// The retry budget exists to correct a model that got the screen wrong.
    /// This model got the screen RIGHT, so spending the retry asks the same
    /// question of the same page - one wasted call per stuck step, and a
    /// second chance to invent an action on a screen that must not be
    /// touched.
    #[test]
    fn a_previous_step_report_does_not_burn_the_retry() {
        let report = r##"[{"action":"previous_step_incomplete","evidence":"Enter a start date"}]"##;
        let mut client = Recording {
            replies: vec![report.into(), report.into()],
            prompts: Vec::new(),
        };
        author_steps(
            &mut client,
            &AuthorContext {
                page_text: Some("Enter a start date"),
                today: None,
                intent: "Continue to the summary",
                ..ctx()
            },
        )
        .expect_err("the report stops authoring");
        assert_eq!(
            client.prompts.len(),
            1,
            "asked once and believed; a retry would only ask the same screen again"
        );
    }

    /// The evidence field is what makes the report actionable, but a model
    /// that omits it must still stop the step rather than have its reply
    /// rejected as an action with no target.
    #[test]
    fn a_previous_step_report_without_evidence_still_stops_the_step() {
        let mut client = Recording {
            replies: vec![r##"{"action":"previous_step_incomplete"}"##.into()],
            prompts: Vec::new(),
        };
        let err = author_steps(
            &mut client,
            &AuthorContext {
                page_text: None,
                today: None,
                intent: "Continue to the summary",
                ..ctx()
            },
        )
        .expect_err("the report stops authoring even unevidenced");
        let AgentError::PreviousStepIncomplete { evidence, .. } = err else {
            panic!("an empty-handed report is still a report: {err:?}");
        };
        assert!(
            !evidence.is_empty(),
            "and it still says something rather than nothing"
        );
    }

    #[test]
    fn a_trailing_continuation_marker_is_split_off_the_actions() {
        let (reply, continues) = split_continuation(
            r##"[{"action":"type_text","target":"css:#name","text":"Ada"},
                 {"action":"step_continues"}]"##,
        );
        assert!(continues, "the model said the rest is on a later screen");
        let items: Vec<serde_json::Value> =
            serde_json::from_str(&reply).expect("what is left is still an array");
        assert_eq!(items.len(), 1, "only the marker was removed: {reply}");
        assert_eq!(items[0]["action"], "type_text");
    }

    /// The marker means "this screen had no more of the step on it", so it
    /// is only ever the last element. Accepted mid-array it would become a
    /// way to skip the actions after it - a step that silently does less
    /// than it says, which is the false green this tool exists to catch.
    #[test]
    fn a_continuation_marker_that_is_not_last_is_left_for_grounding_to_refuse() {
        let mid = r##"[{"action":"type_text","target":"css:#name","text":"Ada"},
             {"action":"step_continues"},
             {"action":"click","target":"css:#greet"}]"##;
        let (reply, continues) = split_continuation(mid);
        assert!(
            !continues,
            "a marker in the middle does not continue a step"
        );
        assert_eq!(reply, mid, "and nothing is quietly dropped from the reply");
        let err = parse_and_ground(
            &reply,
            &scene_targets(SCENE),
            SCENE,
            "web",
            "Greet Ada",
            &[],
        )
        .expect_err("a marker that is not last is refused rather than obeyed");
        // Refused at position 2, which is where the marker sat: the whole
        // step goes, so the click behind it cannot be skipped by putting a
        // marker in front of it.
        assert!(
            err.to_string().contains("action 2 of 3 was rejected"),
            "and the refusal names where it went wrong: {err}"
        );
    }

    #[test]
    fn a_fenced_reply_can_still_continue() {
        let (reply, continues) = split_continuation(
            "```json\n[{\"action\":\"click\",\"target\":\"css:#greet\"},\
             {\"action\":\"step_continues\"}]\n```",
        );
        assert!(continues, "a code fence does not hide the marker");
        let actions = parse_and_ground(
            &reply,
            &scene_targets(SCENE),
            SCENE,
            "web",
            "Greet Ada",
            &[],
        )
        .expect("and what is left grounds");
        assert_eq!(actions.len(), 1);
    }

    /// "Nothing here yet, ask me after the page changes" is not sayable: a
    /// step must do something on the screen it was given, or the recorder
    /// would replan against a page nothing has touched.
    #[test]
    fn a_reply_that_is_only_a_continuation_marker_authors_nothing() {
        let (reply, continues) = split_continuation(r##"[{"action":"step_continues"}]"##);
        assert!(continues);
        let err = parse_and_ground(
            &reply,
            &scene_targets(SCENE),
            SCENE,
            "web",
            "Greet Ada",
            &[],
        )
        .expect_err("an empty plan is refused however it was reached");
        assert!(
            err.to_string().contains("authored no action"),
            "and says so plainly: {err}"
        );
    }

    #[test]
    fn step_complete_is_recognised_in_both_shapes_and_nothing_else_is() {
        assert!(
            is_step_complete(r##"[{"action":"step_complete"}]"##),
            "the documented array form"
        );
        assert!(
            is_step_complete(r##"{"action":"step_complete"}"##),
            "and the bare object a model reaches for unprompted"
        );
        assert!(
            is_step_complete("```json\n[{\"action\":\"step_complete\"}]\n```"),
            "a code fence does not hide it either"
        );

        for not_complete in [
            r##"[{"action":"click","target":"css:#greet"}]"##,
            // Buried among real actions it is not a verdict about the step.
            r##"[{"action":"click","target":"css:#greet"},{"action":"step_complete"}]"##,
            r##"[{"action":"step_continues"}]"##,
            "step_complete",
            "",
        ] {
            assert!(
                !is_step_complete(not_complete),
                "must not read as a finished step: {not_complete}"
            );
        }
    }

    /// A flow may pin the application's clock. A date the model invents from
    /// its own sense of now is then in the page's past and gets refused, so
    /// the date the driver reports has to reach the prompt - and has to say
    /// which clock it is, or the model has no reason to prefer it.
    #[test]
    fn the_prompt_carries_the_date_the_application_believes_it_is() {
        let dated = user_prompt(&AuthorContext {
            today: Some("2019-03-04"),
            ..ctx()
        });
        assert!(
            dated.contains("2019-03-04"),
            "the application's date reaches the model: {dated}"
        );
        assert!(
            dated.contains("as the application sees it"),
            "named as the app's clock, not as today: {dated}"
        );

        let undated = user_prompt(&ctx());
        assert!(
            !undated.contains("as the application sees it"),
            "a driver that cannot tell invents no date: {undated}"
        );
    }
}
