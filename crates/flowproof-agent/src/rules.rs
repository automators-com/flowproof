//! Deterministic step resolution, per app.
//!
//! This is the first, rule-based "authoring backend": each supported app has
//! a small vocabulary of natural-language steps mapped to concrete UIA
//! targets. LLM-backed authoring for arbitrary apps slots in beside it later
//! — the recorder only consumes the resolved actions, not the rules.

use flowproof_trace::format::KeyModifier;

use crate::spec::SpecStep;

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("cannot resolve step '{step}': {reason}")]
    Unresolvable { step: String, reason: String },
    #[error("no rules for app '{0}' (supported: calc, notepad, web)")]
    UnsupportedApp(String),
}

/// What an action targets: a UIA automation id, a CSS selector, or a text
/// anchor (visible text / accessible label / placeholder — how elements
/// without ids are addressed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    AutomationId(String),
    Css(String),
    Text(String),
    /// The whole readable surface — the page for a browser, the foreground
    /// window's subtree for a desktop app, the OCR'd frame for a vision
    /// adapter. `page shows X` asserts against this, not against any
    /// provenance-specific selector.
    Surface,
    /// The nth (1-based) element matching the inner css/text target —
    /// `Type email into the 2nd "Field Name" field`.
    Nth(u32, Box<Target>),
}

impl Target {
    pub fn id(id: impl Into<String>) -> Self {
        Target::AutomationId(id.into())
    }

    pub fn css(css: impl Into<String>) -> Self {
        Target::Css(css.into())
    }

    pub fn text(text: impl Into<String>) -> Self {
        Target::Text(text.into())
    }

    pub fn nth(n: u32, inner: Target) -> Self {
        Target::Nth(n, Box::new(inner))
    }
}

/// Parse a leading `"quoted label"` off `rest`, returning (label, tail).
fn quoted_label(rest: &str) -> Option<(&str, &str)> {
    let end = rest.find('"')?;
    let label = &rest[..end];
    (!label.is_empty()).then_some((label, rest[end + 1..].trim()))
}

/// A concrete action planned from one natural-language step. One step may
/// expand to several actions (e.g. `Type 53` in calc → two button presses).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedAction {
    /// Press (invoke/click) an element.
    Press {
        target: Target,
        /// Human-readable label (recorded as the selector name hint).
        label: String,
    },
    /// Type literal text into an element.
    TypeText { target: Target, text: String },
    /// Type into whatever currently has keyboard focus (dropdown search
    /// boxes, pre-focused rename inputs).
    TypeFocused { text: String },
    /// Clear an input's current value (Playwright's `clear()`).
    Clear { target: Target },
    /// Press a named key, optionally with modifiers (`Enter`, `Ctrl+V`).
    PressKey {
        key: String,
        modifiers: Vec<KeyModifier>,
    },
    /// Navigate to a path or URL mid-flow (`Go to /settings`). Relative
    /// paths resolve against the flow URL's origin at execution time.
    Navigate { path: String },
    /// Reload the current page.
    Reload,
    /// Assert on an element's visible text. Assertions AUTO-WAIT: the
    /// engine polls until the expectation holds or `timeout_ms` elapses —
    /// deterministic (bounded, recorded in the trace), and what makes slow
    /// async UIs testable without sleeps.
    AssertText {
        target: Target,
        expected: String,
        matcher: TextMatch,
        /// How long the expectation may take to become true.
        timeout_ms: u64,
    },
    /// Assert an element's enabled/disabled state ("the \"Save\" is
    /// disabled") — a first-class form, not a css attribute-selector trick.
    AssertEnabled {
        target: Target,
        enabled: bool,
        timeout_ms: u64,
    },
    /// Assert that an element is (or is not) present on screen — the
    /// deterministic reading of "is visible" / "is not visible".
    AssertPresence {
        target: Target,
        present: bool,
        timeout_ms: u64,
    },
    /// Out-of-band SQL assertion: the posted record, not the pixel.
    AssertSql {
        connection: String,
        query: String,
        equals: Option<String>,
        timeout_ms: u64,
    },
    /// Out-of-band HTTP assertion.
    AssertApi {
        method: String,
        url: String,
        status: Option<u16>,
        body_contains: Option<String>,
        timeout_ms: u64,
    },
}

/// How an [`ResolvedAction::AssertText`] expectation compares against the
/// element's live text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextMatch {
    /// Substring present.
    Contains,
    /// Substring absent — `page does not show X`.
    NotContains,
    /// Substring occurs exactly N times — `page shows X 2 times`.
    CountEquals(u64),
    /// Exact equality.
    Equals,
    /// Compare the trailing numeric value instead of raw text.
    NumericEquals,
}

/// Default auto-wait for assertions (Playwright's expect default).
pub const ASSERT_TIMEOUT_MS: u64 = 10_000;
/// Default for explicit `Wait until …` steps — sized for slow backend
/// operations (a data-generation job, a report build).
pub const WAIT_STEP_TIMEOUT_MS: u64 = 60_000;

/// Parse a trailing `within <N>s` / `within <N> seconds` qualifier off a
/// step, returning (rest, timeout override).
fn split_within(text: &str) -> (&str, Option<u64>) {
    let lower = text.to_lowercase();
    let Some(pos) = lower.rfind(" within ") else {
        return (text, None);
    };
    let qualifier = lower[pos + " within ".len()..].trim();
    let digits = qualifier
        .strip_suffix(" seconds")
        .or_else(|| qualifier.strip_suffix(" second"))
        .or_else(|| qualifier.strip_suffix("sec"))
        .or_else(|| qualifier.strip_suffix('s'))
        .map(str::trim)
        .unwrap_or(qualifier);
    match digits.parse::<u64>() {
        Ok(seconds) if seconds > 0 => (text[..pos].trim_end(), Some(seconds * 1000)),
        _ => (text, None),
    }
}

/// AutomationId of the Windows Calculator result display.
pub const CALC_DISPLAY_ID: &str = "CalculatorResults";

/// AutomationId of classic Notepad's edit control (Win32 control id 15).
pub const NOTEPAD_EDITOR_ID: &str = "15";

fn unresolvable(step: &str, reason: impl Into<String>) -> RulesError {
    RulesError::Unresolvable {
        step: step.to_string(),
        reason: reason.into(),
    }
}

/// Case-insensitively strip an ASCII `prefix`, returning the rest of the
/// ORIGINAL string (case preserved).
fn strip_prefix_ci<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    if text.len() >= prefix.len() && text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        Some(&text[prefix.len()..])
    } else {
        None
    }
}

/// Case-insensitively strip an ASCII `suffix`, returning the front of the
/// ORIGINAL string (case preserved).
fn strip_suffix_ci<'a>(text: &'a str, suffix: &str) -> Option<&'a str> {
    if text.len() >= suffix.len() && text[text.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
    {
        Some(&text[..text.len() - suffix.len()])
    } else {
        None
    }
}

/// Resolve one spec step into concrete actions for `app`. Out-of-band
/// assertions are app-independent — they never touch the UI at all.
pub fn resolve_step(app: &str, step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
    match step {
        SpecStep::AssertSql { assert_sql } => {
            return Ok(vec![ResolvedAction::AssertSql {
                connection: assert_sql.connection.clone(),
                query: assert_sql.query.clone(),
                equals: assert_sql.equals.clone(),
                timeout_ms: assert_sql
                    .timeout_seconds
                    .map_or(ASSERT_TIMEOUT_MS, |s| s * 1000),
            }]);
        }
        SpecStep::AssertApi { assert_api } => {
            let (method, url) = assert_api
                .request
                .split_once(' ')
                .map(|(m, u)| (m.trim(), u.trim()))
                .filter(|(m, u)| !m.is_empty() && !u.is_empty())
                .ok_or_else(|| {
                    unresolvable(
                        &assert_api.request,
                        "assert_api request must be '<METHOD> <url>' (e.g. 'GET ${API}/x')",
                    )
                })?;
            return Ok(vec![ResolvedAction::AssertApi {
                method: method.to_ascii_uppercase(),
                url: url.to_string(),
                status: assert_api.status,
                body_contains: assert_api.body_contains.clone(),
                timeout_ms: assert_api
                    .timeout_seconds
                    .map_or(ASSERT_TIMEOUT_MS, |s| s * 1000),
            }]);
        }
        _ => {}
    }
    match app {
        "calc" => calc::resolve(step),
        "notepad" => notepad::resolve(step),
        "web" => web::resolve(step),
        "sap" => sap::resolve(step),
        // Pixels-only mode shares the generic grammar too: quoted labels
        // are OCR text anchors, asserts read the OCR'd surface.
        "vision" => sap::resolve(step),
        other => Err(RulesError::UnsupportedApp(other.to_string())),
    }
}

/// `css:` targets by CSS selector, `id:` by native id (UIA automation id,
/// SAP scripting id) — adapter-specific escape hatches inside a quoted
/// label. Everything else is a text anchor, meaningful on every provenance.
fn target_from_label(label: &str) -> Target {
    if let Some(css) = label.strip_prefix("css:") {
        if !css.trim().is_empty() {
            return Target::css(css.trim());
        }
    }
    if let Some(id) = label.strip_prefix("id:") {
        if !id.trim().is_empty() {
            return Target::id(id.trim());
        }
    }
    Target::text(label)
}

/// Scene TARGET TOKENS, as emitted by each driver's `scene()` and echoed
/// verbatim by the authoring model: `css:<sel>` (web), `id:<automation
/// id>` (UIA / SAP), `text:<name>` (text anchor, any provenance), and the
/// literal `surface` (the whole readable surface, assert-only). Returns
/// None for anything that isn't a well-formed token.
pub(crate) fn target_from_token(token: &str) -> Option<Target> {
    if token == "surface" {
        return Some(Target::Surface);
    }
    if let Some(css) = token.strip_prefix("css:") {
        return (!css.trim().is_empty()).then(|| Target::css(css.trim()));
    }
    if let Some(id) = token.strip_prefix("id:") {
        return (!id.trim().is_empty()).then(|| Target::id(id.trim()));
    }
    if let Some(text) = token.strip_prefix("text:") {
        return (!text.trim().is_empty()).then(|| Target::text(text.trim()));
    }
    None
}

/// Parse an optional leading 1-based ordinal (`2nd `, `3rd `, `10th `)
/// off `rest`, returning it with the remainder.
fn split_ordinal(rest: &str) -> (Option<u32>, &str) {
    let Some((word, tail)) = rest.split_once(' ') else {
        return (None, rest);
    };
    let digits = word
        .strip_suffix("st")
        .or_else(|| word.strip_suffix("nd"))
        .or_else(|| word.strip_suffix("rd"))
        .or_else(|| word.strip_suffix("th"));
    match digits.map(str::parse::<u32>) {
        Some(Ok(n)) if n >= 1 => (Some(n), tail.trim_start()),
        _ => (None, rest),
    }
}

fn with_nth(nth: Option<u32>, target: Target) -> Target {
    match nth {
        Some(n) => Target::nth(n, target),
        None => target,
    }
}

/// The PROVENANCE-AGNOSTIC assertion grammar, shared by every app profile.
/// Forms describe WHAT to check; how each target resolves is the adapter's
/// business (surface = page / window subtree / OCR frame; `<id>` = DOM id /
/// UIA AutomationId; quoted labels = text anchors). Apps layer their own
/// sugar on top (calc's `display shows`, notepad's `document contains`).
mod assertions {
    use super::*;

    /// Parse a trailing occurrence count (`… 2 times`) off an expectation.
    fn split_count(text: &str) -> (&str, Option<u64>) {
        let stripped = strip_suffix_ci(text, " times").or_else(|| strip_suffix_ci(text, " time"));
        let Some(stripped) = stripped else {
            return (text, None);
        };
        match stripped.rsplit_once(' ') {
            Some((rest, digits)) => match digits.parse::<u64>() {
                Ok(n) => (rest.trim_end(), Some(n)),
                Err(_) => (text, None),
            },
            None => (text, None),
        }
    }

    /// All forms auto-wait and accept a trailing `within <N>s`. Every form
    /// starts with a plain word — a YAML scalar cannot START with a double
    /// quote, so quoted targets always follow `the `:
    ///   page shows <text>                       (the whole surface)
    ///   page shows <text> <N> times             (occurrences of the TEXT)
    ///   page does not show <text>
    ///   the "<label>"|<id> field contains <text>
    ///   the "<text-or-css:sel>" shows <text>
    ///   the "<text-or-css:sel>" is visible | is not visible
    pub(super) fn resolve(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
        let trimmed = text.trim();
        let (trimmed, timeout) = split_within(trimmed);
        let timeout_ms = timeout.unwrap_or(ASSERT_TIMEOUT_MS);

        // `the page shows X` reads as naturally as `page shows X` — accept
        // the article rather than teaching people the one true spelling.
        let trimmed = strip_prefix_ci(trimmed, "the page ")
            .map(|rest| format!("page {rest}"))
            .map(std::borrow::Cow::Owned)
            .unwrap_or(std::borrow::Cow::Borrowed(trimmed));
        let trimmed = trimmed.as_ref();

        if let Some(rest) = strip_prefix_ci(trimmed, "page shows ") {
            let (expected, count) = split_count(rest.trim());
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected text"));
            }
            let matcher = match count {
                Some(n) => TextMatch::CountEquals(n),
                None => TextMatch::Contains,
            };
            return Ok(vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: expected.to_string(),
                matcher,
                timeout_ms,
            }]);
        }

        if let Some(rest) = strip_prefix_ci(trimmed, "page does not show ") {
            let expected = rest.trim();
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected text"));
            }
            return Ok(vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: expected.to_string(),
                matcher: TextMatch::NotContains,
                timeout_ms,
            }]);
        }

        // `the …` forms. After the optional ordinal and quoted target the
        // tail dispatches:
        //   the "<label>" field contains <text>    (input VALUE)
        //   the <id> field contains <text>         (native id: DOM id / UIA)
        //   the "<target>" shows <text>            (element-scoped contains)
        //   the "<target>" is visible | is not visible
        if let Some(rest) = strip_prefix_ci(trimmed, "the ") {
            let (nth, rest) = split_ordinal(rest.trim());
            if let Some(quoted) = rest.strip_prefix('"') {
                if let Some((label, tail)) = quoted_label(quoted) {
                    let target = with_nth(nth, target_from_label(label));
                    if let Some(expected) = strip_prefix_ci(tail, "field contains ")
                        .or_else(|| strip_prefix_ci(tail, "shows "))
                    {
                        let expected = expected.trim();
                        if expected.is_empty() {
                            return Err(unresolvable(trimmed, "no expected text"));
                        }
                        return Ok(vec![ResolvedAction::AssertText {
                            target,
                            expected: expected.to_string(),
                            matcher: TextMatch::Contains,
                            timeout_ms,
                        }]);
                    }
                    if tail.eq_ignore_ascii_case("is visible") {
                        return Ok(vec![ResolvedAction::AssertPresence {
                            target,
                            present: true,
                            timeout_ms,
                        }]);
                    }
                    if tail.eq_ignore_ascii_case("is not visible") {
                        return Ok(vec![ResolvedAction::AssertPresence {
                            target,
                            present: false,
                            timeout_ms,
                        }]);
                    }
                    if tail.eq_ignore_ascii_case("is disabled") {
                        return Ok(vec![ResolvedAction::AssertEnabled {
                            target,
                            enabled: false,
                            timeout_ms,
                        }]);
                    }
                    if tail.eq_ignore_ascii_case("is enabled") {
                        return Ok(vec![ResolvedAction::AssertEnabled {
                            target,
                            enabled: true,
                            timeout_ms,
                        }]);
                    }
                }
            } else if nth.is_none() {
                if let Some(pos) = rest.find(" field contains ") {
                    let id = rest[..pos].trim();
                    let expected = rest[pos + " field contains ".len()..].trim();
                    if !id.is_empty() && !expected.is_empty() {
                        // A NATIVE id, not a web-ism: DOM id on web (the
                        // adapter derives `#id`), AutomationId on UIA, a
                        // scripting id on SAP.
                        return Ok(vec![ResolvedAction::AssertText {
                            target: Target::id(id),
                            expected: expected.to_string(),
                            matcher: TextMatch::Contains,
                            timeout_ms,
                        }]);
                    }
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'the \"<label>\" field contains <text>', 'the <id> field \
                 contains <text>', 'the \"<target>\" shows <text>', \
                 'the \"<target>\" is [not] visible', or 'the \"<target>\" is \
                 enabled|disabled'",
            ));
        }

        Err(unresolvable(
            trimmed,
            "expected '[the ]page shows <text>[ N times]', '[the ]page does not show \
             <text>', 'the \"<label>\" field contains <text>', 'the \"<target>\" shows \
             <text>', 'the \"<target>\" is [not] visible', or 'the \"<target>\" is \
             enabled|disabled' (see docs/authoring.md for the full grammar)",
        ))
    }
}

mod calc {
    use super::*;

    fn digit_button(c: char) -> Option<(String, String)> {
        if let Some(d) = c.to_digit(10) {
            const NAMES: [&str; 10] = [
                "Zero", "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine",
            ];
            Some((format!("num{d}Button"), NAMES[d as usize].to_string()))
        } else if c == '.' {
            Some(("decimalSeparatorButton".into(), "Decimal separator".into()))
        } else {
            None
        }
    }

    fn operator_button(word: &str) -> Option<(&'static str, &'static str)> {
        match word {
            "plus" | "add" => Some(("plusButton", "Plus")),
            "minus" | "subtract" => Some(("minusButton", "Minus")),
            "times" | "multiply" => Some(("multiplyButton", "Multiply by")),
            "divide" => Some(("divideButton", "Divide by")),
            "equals" | "equal" => Some(("equalButton", "Equals")),
            "clear" => Some(("clearButton", "Clear")),
            _ => None,
        }
    }

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => resolve_plain(text),
            // App sugar first, then the shared provenance-agnostic grammar
            // (surface / field-value / element-scoped / visibility forms).
            SpecStep::Assert { assert } => resolve_assert(assert)
                .map(|a| vec![a])
                .or_else(|sugar_err| assertions::resolve(assert).map_err(|_| sugar_err)),
            // Out-of-band steps are dispatched before app resolution.
            other => Err(unresolvable(&other.intent(), "handled before app dispatch")),
        }
    }

    fn resolve_plain(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
        let trimmed = text.trim();

        if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
            let value = rest.trim();
            if value.is_empty() {
                return Err(unresolvable(trimmed, "nothing to type"));
            }
            let mut actions = Vec::new();
            for c in value.chars() {
                let (automation_id, label) = digit_button(c).ok_or_else(|| {
                    unresolvable(trimmed, format!("'{c}' is not a digit or decimal point"))
                })?;
                actions.push(ResolvedAction::Press {
                    target: Target::AutomationId(automation_id),
                    label,
                });
            }
            return Ok(actions);
        }

        if let Some(rest) = strip_prefix_ci(trimmed, "press ") {
            let word = rest.trim().to_lowercase();
            let (automation_id, label) = operator_button(&word)
                .ok_or_else(|| unresolvable(trimmed, format!("unknown calculator key '{word}'")))?;
            return Ok(vec![ResolvedAction::Press {
                target: Target::id(automation_id),
                label: label.into(),
            }]);
        }

        Err(unresolvable(
            trimmed,
            "expected 'Type <digits>' or 'Press <key>'",
        ))
    }

    fn resolve_assert(text: &str) -> Result<ResolvedAction, RulesError> {
        let trimmed = text.trim();
        if let Some(rest) = strip_prefix_ci(trimmed, "display shows ") {
            let expected = rest.trim();
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected value"));
            }
            return Ok(ResolvedAction::AssertText {
                target: Target::id(CALC_DISPLAY_ID),
                expected: expected.to_string(),
                matcher: TextMatch::NumericEquals,
                timeout_ms: ASSERT_TIMEOUT_MS,
            });
        }
        Err(unresolvable(trimmed, "expected 'display shows <value>'"))
    }
}

mod notepad {
    use super::*;

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => {
                let trimmed = text.trim();
                if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
                    let value = rest.trim();
                    if value.is_empty() {
                        return Err(unresolvable(trimmed, "nothing to type"));
                    }
                    return Ok(vec![ResolvedAction::TypeText {
                        target: Target::id(NOTEPAD_EDITOR_ID),
                        text: value.to_string(),
                    }]);
                }
                Err(unresolvable(trimmed, "expected 'Type <text>'"))
            }
            SpecStep::Assert { assert } => {
                let trimmed = assert.trim();
                if let Some(rest) = strip_prefix_ci(trimmed, "document contains ") {
                    let expected = rest.trim();
                    if expected.is_empty() {
                        return Err(unresolvable(trimmed, "no expected text"));
                    }
                    return Ok(vec![ResolvedAction::AssertText {
                        target: Target::id(NOTEPAD_EDITOR_ID),
                        expected: expected.to_string(),
                        matcher: TextMatch::Contains,
                        timeout_ms: ASSERT_TIMEOUT_MS,
                    }]);
                }
                // Shared grammar: negatives, counts, field values, presence
                // — the engine evaluates them on UIA like anywhere else.
                assertions::resolve(trimmed)
            }
            // Out-of-band steps are dispatched before app resolution.
            other => Err(unresolvable(&other.intent(), "handled before app dispatch")),
        }
    }
}

mod sap {
    use super::*;

    /// SAP GUI shares the generic plain-step grammar (quoted labels are
    /// text anchors; `"id:wnd[0]/…"` addresses a scripting id directly)
    /// and the shared assertion grammar. `Go to /nVA01` navigates by
    /// transaction code — the sap-com driver types it into the command
    /// field, exactly how a user navigates SAP.
    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => web::resolve_plain(text),
            SpecStep::Assert { assert } => assertions::resolve(assert),
            // Out-of-band steps are dispatched before app resolution.
            other => Err(unresolvable(&other.intent(), "handled before app dispatch")),
        }
    }
}

mod web {
    use super::*;

    pub(super) fn resolve(step: &SpecStep) -> Result<Vec<ResolvedAction>, RulesError> {
        match step {
            SpecStep::Plain(text) => resolve_plain(text),
            SpecStep::Assert { assert } => assertions::resolve(assert),
            // Out-of-band steps are dispatched before app resolution.
            other => Err(unresolvable(&other.intent(), "handled before app dispatch")),
        }
    }

    /// Keys addressable by `Press <Key>`, in canonical (CDP) spelling.
    const NAMED_KEYS: &[&str] = &[
        "Enter",
        "Escape",
        "Tab",
        "Backspace",
        "Delete",
        "Space",
        "ArrowUp",
        "ArrowDown",
        "ArrowLeft",
        "ArrowRight",
        "Home",
        "End",
        "PageUp",
        "PageDown",
    ];

    fn parse_modifier(word: &str) -> Option<KeyModifier> {
        match word.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => Some(KeyModifier::Ctrl),
            "alt" | "option" => Some(KeyModifier::Alt),
            "shift" => Some(KeyModifier::Shift),
            "meta" | "win" | "cmd" | "command" => Some(KeyModifier::Win),
            _ => None,
        }
    }

    /// Parse `Enter`, `Escape`, `Control+V`, `Alt+Shift+Backspace` into a
    /// canonical key plus modifiers. Returns None for anything that isn't a
    /// key chord, so ordinary sentences never match.
    fn parse_key_chord(text: &str) -> Option<(String, Vec<KeyModifier>)> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        if parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        let (key, mod_parts) = parts.split_last()?;
        let mut modifiers = Vec::with_capacity(mod_parts.len());
        for part in mod_parts {
            modifiers.push(parse_modifier(part)?);
        }
        if let Some(named) = NAMED_KEYS.iter().find(|k| k.eq_ignore_ascii_case(key)) {
            return Some(((*named).to_string(), modifiers));
        }
        // Single characters only make sense as part of a chord (Ctrl+V).
        if !modifiers.is_empty() && key.chars().count() == 1 {
            return Some((key.to_ascii_lowercase(), modifiers));
        }
        None
    }

    /// The generic plain-step grammar. Nothing here is web-specific — the
    /// sap module reuses it verbatim; targets resolve per-provenance.
    pub(super) fn resolve_plain(text: &str) -> Result<Vec<ResolvedAction>, RulesError> {
        let trimmed = text.trim();

        // `Go to /path` / `Navigate to /path` → navigate mid-flow.
        if let Some(rest) =
            strip_prefix_ci(trimmed, "go to ").or_else(|| strip_prefix_ci(trimmed, "navigate to "))
        {
            let path = rest.trim();
            if path.is_empty() {
                return Err(unresolvable(trimmed, "no path or URL to go to"));
            }
            return Ok(vec![ResolvedAction::Navigate {
                path: path.to_string(),
            }]);
        }

        // `Reload the page`.
        if trimmed.eq_ignore_ascii_case("reload the page") {
            return Ok(vec![ResolvedAction::Reload]);
        }

        // `Wait until page shows <text> [within <N>s]` → an auto-waiting
        // assert with a long default, for slow backend operations.
        if let Some(rest) = strip_prefix_ci(trimmed, "wait until page shows ") {
            let (expected, timeout) = split_within(rest.trim());
            if expected.is_empty() {
                return Err(unresolvable(trimmed, "no expected text"));
            }
            return Ok(vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: expected.trim().to_string(),
                matcher: TextMatch::Contains,
                timeout_ms: timeout.unwrap_or(WAIT_STEP_TIMEOUT_MS),
            }]);
        }

        // `Type <text> into the [Nth ]"<label>" field` → text anchor (or
        // `css:` selector) target; `Type <text> into the <id> field` →
        // `#<id>`; bare `Type <text>` → the focused element.
        if let Some(rest) = strip_prefix_ci(trimmed, "type ") {
            let lower = rest.to_lowercase();
            let Some(pos) = lower.rfind(" into the ") else {
                let value = rest.trim();
                if value.is_empty() {
                    return Err(unresolvable(trimmed, "nothing to type"));
                }
                return Ok(vec![ResolvedAction::TypeFocused {
                    text: value.to_string(),
                }]);
            };
            let value = rest[..pos].trim();
            let field = rest[pos + " into the ".len()..].trim();
            if value.is_empty() {
                return Err(unresolvable(trimmed, "missing text to type"));
            }
            let (nth, field) = split_ordinal(field);
            if let Some(quoted) = field.strip_prefix('"') {
                if let Some((label, tail)) = quoted_label(quoted) {
                    if tail.eq_ignore_ascii_case("field") {
                        return Ok(vec![ResolvedAction::TypeText {
                            target: with_nth(nth, target_from_label(label)),
                            text: value.to_string(),
                        }]);
                    }
                }
            } else if nth.is_none() {
                if let Some(id) = strip_suffix_ci(field, " field").map(str::trim) {
                    if !id.is_empty() {
                        return Ok(vec![ResolvedAction::TypeText {
                            target: Target::id(id),
                            text: value.to_string(),
                        }]);
                    }
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Type <text> into the [2nd ]\"<label>\" field' or \
                 'Type <text> into the <id> field'",
            ));
        }

        // `Select <option> from|in the [Nth ]"<label>" field|dropdown` —
        // native dropdowns. Encoded as TypeText: each adapter commits the
        // option its own way (the web driver goes through the select's
        // native value setter and fires input+change).
        if let Some(rest) = strip_prefix_ci(trimmed, "select ") {
            let lower = rest.to_lowercase();
            let split = lower
                .rfind(" from the ")
                .map(|p| (p, " from the ".len()))
                .or_else(|| lower.rfind(" in the ").map(|p| (p, " in the ".len())));
            if let Some((pos, sep_len)) = split {
                let value = rest[..pos].trim();
                let (nth, field) = split_ordinal(rest[pos + sep_len..].trim());
                let target = if let Some(quoted) = field.strip_prefix('"') {
                    quoted_label(quoted).and_then(|(label, tail)| {
                        (tail.eq_ignore_ascii_case("field")
                            || tail.eq_ignore_ascii_case("dropdown"))
                        .then(|| with_nth(nth, target_from_label(label)))
                    })
                } else if nth.is_none() {
                    strip_suffix_ci(field, " field")
                        .or_else(|| strip_suffix_ci(field, " dropdown"))
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(Target::id)
                } else {
                    None
                };
                if let (Some(target), false) = (target, value.is_empty()) {
                    return Ok(vec![ResolvedAction::TypeText {
                        target,
                        text: value.to_string(),
                    }]);
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Select <option> from the [2nd ]\"<label>\" field|dropdown' or \
                 'Select <option> from the <id> field|dropdown'",
            ));
        }

        // `Replace the [Nth ]"<label>" field with <text>` — clear + type as
        // ONE step, because "set this field to X" is one thought.
        if let Some(rest) = strip_prefix_ci(trimmed, "replace the ") {
            let (nth, rest) = split_ordinal(rest.trim());
            let parsed = if let Some(quoted) = rest.strip_prefix('"') {
                quoted_label(quoted).and_then(|(label, tail)| {
                    strip_prefix_ci(tail, "field with ")
                        .map(|value| (with_nth(nth, target_from_label(label)), value))
                })
            } else if nth.is_none() {
                rest.find(" field with ").and_then(|pos| {
                    let id = rest[..pos].trim();
                    (!id.is_empty()).then(|| (Target::id(id), &rest[pos + " field with ".len()..]))
                })
            } else {
                None
            };
            if let Some((target, value)) = parsed {
                let value = value.trim();
                if value.is_empty() {
                    return Err(unresolvable(trimmed, "missing replacement text"));
                }
                return Ok(vec![
                    ResolvedAction::Clear {
                        target: target.clone(),
                    },
                    ResolvedAction::TypeText {
                        target,
                        text: value.to_string(),
                    },
                ]);
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Replace the [2nd ]\"<label>\" field with <text>' or \
                 'Replace the <id> field with <text>'",
            ));
        }

        // `Clear the [Nth ]"<label>" field` / `Clear the <id> field`.
        if let Some(rest) = strip_prefix_ci(trimmed, "clear the ") {
            let (nth, field) = split_ordinal(rest.trim());
            if let Some(quoted) = field.strip_prefix('"') {
                if let Some((label, tail)) = quoted_label(quoted) {
                    if tail.eq_ignore_ascii_case("field") {
                        return Ok(vec![ResolvedAction::Clear {
                            target: with_nth(nth, target_from_label(label)),
                        }]);
                    }
                }
            } else if nth.is_none() {
                if let Some(id) = strip_suffix_ci(field, " field").map(str::trim) {
                    if !id.is_empty() {
                        return Ok(vec![ResolvedAction::Clear {
                            target: Target::id(id),
                        }]);
                    }
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Clear the [2nd ]\"<label>\" field' or 'Clear the <id> field'",
            ));
        }

        // `Press the [Nth ]"<label>" button` → the button showing <label>;
        // `Press the <id> button` → `#<id>`.
        if let Some(rest) = strip_prefix_ci(trimmed, "press the ") {
            let (nth, target_text) = split_ordinal(rest.trim());
            if let Some(quoted) = target_text.strip_prefix('"') {
                if let Some((label, tail)) = quoted_label(quoted) {
                    if tail.eq_ignore_ascii_case("button") {
                        return Ok(vec![ResolvedAction::Press {
                            target: with_nth(nth, target_from_label(label)),
                            label: label.to_string(),
                        }]);
                    }
                }
            } else if nth.is_none() {
                if let Some(id) = strip_suffix_ci(target_text, " button").map(str::trim) {
                    if !id.is_empty() {
                        return Ok(vec![ResolvedAction::Press {
                            target: Target::id(id),
                            label: id.to_string(),
                        }]);
                    }
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Press the [2nd ]\"<label>\" button' or 'Press the <id> button'",
            ));
        }

        // `Press <Key>` / `Press <Mod>+<Key>` → a keyboard press on the
        // focused element (Enter, Escape, Control+V, Alt+Shift+Backspace).
        if let Some(rest) = strip_prefix_ci(trimmed, "press ") {
            if let Some((key, modifiers)) = parse_key_chord(rest.trim()) {
                return Ok(vec![ResolvedAction::PressKey { key, modifiers }]);
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Press the \"<label>\" button' or a key like \
                 'Press Enter' / 'Press Control+V'",
            ));
        }

        // `Click [the [Nth ]]"<text>"` → any interactable element showing
        // <text> (tabs, links, menu options, list rows), or a `css:` target.
        if let Some(rest) = strip_prefix_ci(trimmed, "click ") {
            let rest = rest.trim();
            let (nth, rest) = match strip_prefix_ci(rest, "the ") {
                Some(after_the) => split_ordinal(after_the.trim()),
                None => (None, rest),
            };
            if let Some(quoted) = rest.strip_prefix('"') {
                if let Some((label, tail)) = quoted_label(quoted) {
                    if tail.is_empty() {
                        return Ok(vec![ResolvedAction::Press {
                            target: with_nth(nth, target_from_label(label)),
                            label: label.to_string(),
                        }]);
                    }
                }
            }
            return Err(unresolvable(
                trimmed,
                "expected 'Click \"<text>\"' or 'Click the [2nd ]\"<text>\"'",
            ));
        }

        Err(unresolvable(
            trimmed,
            "expected 'Type <text> into the <id>|\"<label>\" field', 'Press the \
             <id>|\"<label>\" button', 'Click \"<text>\"', 'Clear the … field', \
             'Press <Key>', or 'Wait until page shows <text>'",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calc_type_expands_per_digit() {
        let actions =
            resolve_step("calc", &SpecStep::Plain("Type 53".into())).expect("digits resolve");
        assert_eq!(
            actions,
            vec![
                ResolvedAction::Press {
                    target: Target::id("num5Button"),
                    label: "Five".into()
                },
                ResolvedAction::Press {
                    target: Target::id("num3Button"),
                    label: "Three".into()
                },
            ]
        );
    }

    #[test]
    fn calc_press_maps_operators() {
        let actions =
            resolve_step("calc", &SpecStep::Plain("Press plus".into())).expect("plus resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                target: Target::id("plusButton"),
                label: "Plus".into()
            }]
        );
    }

    #[test]
    fn calc_assert_is_numeric_equals() {
        let actions = resolve_step(
            "calc",
            &SpecStep::Assert {
                assert: "display shows 8".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::id(CALC_DISPLAY_ID),
                expected: "8".into(),
                matcher: TextMatch::NumericEquals,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn notepad_type_preserves_case() {
        let actions = resolve_step(
            "notepad",
            &SpecStep::Plain("Type Hello from FlowProof".into()),
        )
        .expect("type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::id(NOTEPAD_EDITOR_ID),
                text: "Hello from FlowProof".into(),
            }]
        );
    }

    #[test]
    fn notepad_assert_is_contains() {
        let actions = resolve_step(
            "notepad",
            &SpecStep::Assert {
                assert: "document contains Hello".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::id(NOTEPAD_EDITOR_ID),
                expected: "Hello".into(),
                matcher: TextMatch::Contains,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn web_type_and_press_map_to_native_ids() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type Ada into the name field".into()),
        )
        .expect("type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::id("name"),
                text: "Ada".into(),
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Press the greet button".into()))
            .expect("press resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                target: Target::id("greet"),
                label: "greet".into(),
            }]
        );
    }

    #[test]
    fn web_assert_targets_the_surface() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows Hello, Ada".into(),
            },
        )
        .expect("assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "Hello, Ada".into(),
                matcher: TextMatch::Contains,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn wait_until_maps_to_a_long_auto_waiting_assert() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Wait until page shows Generation complete".into()),
        )
        .expect("wait resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "Generation complete".into(),
                matcher: TextMatch::Contains,
                timeout_ms: WAIT_STEP_TIMEOUT_MS,
            }]
        );
    }

    #[test]
    fn within_qualifier_overrides_the_timeout() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Wait until page shows Done within 90s".into()),
        )
        .expect("wait resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: 90_000, .. } if expected == "Done"
        ));

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows Ready within 3 seconds".into(),
            },
        )
        .expect("assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: 3000, .. } if expected == "Ready"
        ));
    }

    #[test]
    fn within_only_strips_a_valid_qualifier() {
        // "within" followed by a non-number stays part of the expectation.
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows delivered within budget".into(),
            },
        )
        .expect("assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, timeout_ms: ASSERT_TIMEOUT_MS, .. }
                if expected == "delivered within budget"
        ));
    }

    #[test]
    fn quoted_labels_map_to_text_anchors() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type CI Pipeline Key into the \"Enter key name\" field".into()),
        )
        .expect("quoted type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::text("Enter key name"),
                text: "CI Pipeline Key".into(),
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Press the \"Save\" button".into()))
            .expect("quoted press resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                target: Target::text("Save"),
                label: "Save".into(),
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Click \"Templates\"".into()))
            .expect("click resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Press {
                target: Target::text("Templates"),
                label: "Templates".into(),
            }]
        );

        // Unquoted id forms are untouched.
        let actions = resolve_step("web", &SpecStep::Plain("Press the greet button".into()))
            .expect("id press resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::Press { target: Target::AutomationId(id), .. } if id == "greet"
        ));
    }

    #[test]
    fn css_prefix_in_quoted_labels_targets_by_selector() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Click \"css:svg.h-7[name='form_options']\"".into()),
        )
        .expect("css click resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::Press { target: Target::Css(css), .. }
                if css == "svg.h-7[name='form_options']"
        ));

        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type name into the \"css:[name='params.0.name']\" field".into()),
        )
        .expect("css type resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::TypeText { target: Target::Css(css), text }
                if css == "[name='params.0.name']" && text == "name"
        ));
    }

    #[test]
    fn key_presses_and_chords_resolve() {
        let actions = resolve_step("web", &SpecStep::Plain("Press Enter".into()))
            .expect("named key resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::PressKey {
                key: "Enter".into(),
                modifiers: vec![],
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Press Control+V".into()))
            .expect("chord resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::PressKey {
                key: "v".into(),
                modifiers: vec![KeyModifier::Ctrl],
            }]
        );

        let actions = resolve_step("web", &SpecStep::Plain("Press Alt+Shift+Backspace".into()))
            .expect("multi-modifier chord resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::PressKey {
                key: "Backspace".into(),
                modifiers: vec![KeyModifier::Alt, KeyModifier::Shift],
            }]
        );

        // A sentence after `Press ` is NOT a key chord.
        resolve_step("web", &SpecStep::Plain("Press hard on the app".into()))
            .expect_err("non-key press must fail");
    }

    #[test]
    fn clear_forms_resolve() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Clear the \"Field Name\" field".into()),
        )
        .expect("quoted clear resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Clear {
                target: Target::text("Field Name"),
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Clear the templateName field".into()),
        )
        .expect("id clear resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Clear {
                target: Target::id("templateName"),
            }]
        );
    }

    #[test]
    fn bare_type_targets_the_focused_element() {
        let actions = resolve_step("web", &SpecStep::Plain("Type First Name".into()))
            .expect("focused type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeFocused {
                text: "First Name".into(),
            }]
        );

        // A malformed into-the form errors instead of silently typing the
        // whole sentence into whatever has focus.
        resolve_step(
            "web",
            &SpecStep::Plain("Type x into the \"Field Name\" box".into()),
        )
        .expect_err("bad tail must fail");
    }

    #[test]
    fn ordinals_narrow_targets() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type email into the 2nd \"Field Name\" field".into()),
        )
        .expect("ordinal type resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::nth(2, Target::text("Field Name")),
                text: "email".into(),
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Click the 2nd \"css:[data-test='data-type']\"".into()),
        )
        .expect("ordinal css click resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::Press { target: Target::Nth(2, inner), .. }
                if matches!(&**inner, Target::Css(css) if css == "[data-test='data-type']")
        ));

        // Unquoted id forms preserve their case (#templateName).
        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Type Customers into the templateName field".into()),
        )
        .expect("camelCase id resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::TypeText { target: Target::AutomationId(id), .. } if id == "templateName"
        ));
    }

    #[test]
    fn negative_and_count_asserts_resolve() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page does not show TestConnection within 15s".into(),
            },
        )
        .expect("negative assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "TestConnection".into(),
                matcher: TextMatch::NotContains,
                timeout_ms: 15_000,
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows playwrightTemplateRoot 2 times".into(),
            },
        )
        .expect("count assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::Surface,
                expected: "playwrightTemplateRoot".into(),
                matcher: TextMatch::CountEquals(2),
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );

        // Text that merely ENDS in "times" without a number stays intact.
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "page shows good times".into(),
            },
        )
        .expect("plain assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { expected, matcher: TextMatch::Contains, .. }
                if expected == "good times"
        ));
    }

    #[test]
    fn element_scoped_and_field_value_asserts_resolve() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the \"css:#live_preview\" shows Street".into(),
            },
        )
        .expect("element-scoped assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::css("#live_preview"),
                expected: "Street".into(),
                matcher: TextMatch::Contains,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the templateName field contains playwrightTemplateRoot within 10s".into(),
            },
        )
        .expect("id field-value assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertText {
                target: Target::id("templateName"),
                expected: "playwrightTemplateRoot".into(),
                matcher: TextMatch::Contains,
                timeout_ms: 10_000,
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the \"Field Name\" field contains Street".into(),
            },
        )
        .expect("labelled field-value assert resolves");
        assert!(matches!(
            &actions[0],
            ResolvedAction::AssertText { target: Target::Text(t), expected, .. }
                if t == "Field Name" && expected == "Street"
        ));
    }

    #[test]
    fn visibility_asserts_resolve() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the \"css:#live_preview\" is visible".into(),
            },
        )
        .expect("visible assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertPresence {
                target: Target::css("#live_preview"),
                present: true,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the \"css:#live_preview\" is not visible within 10s".into(),
            },
        )
        .expect("not-visible assert resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertPresence {
                target: Target::css("#live_preview"),
                present: false,
                timeout_ms: 10_000,
            }]
        );
    }

    #[test]
    fn unknown_steps_and_apps_error_clearly() {
        let err = resolve_step("calc", &SpecStep::Plain("Wave at the screen".into()))
            .expect_err("unknown step must fail");
        assert!(err.to_string().contains("Wave at the screen"));

        let err = resolve_step("oracle-forms", &SpecStep::Plain("Type 5".into()))
            .expect_err("unknown app must fail");
        assert!(matches!(err, RulesError::UnsupportedApp(_)));
    }

    #[test]
    fn sap_shares_the_generic_grammar() {
        let actions = resolve_step(
            "sap",
            &SpecStep::Plain(r#"Type ZOR into the "Order Type" field"#.into()),
        )
        .expect("labelled field resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::text("Order Type"),
                text: "ZOR".into()
            }]
        );

        let actions =
            resolve_step("sap", &SpecStep::Plain("Go to /nVA01".into())).expect("tcode resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Navigate {
                path: "/nVA01".into()
            }]
        );

        let actions = resolve_step(
            "sap",
            &SpecStep::Assert {
                assert: "page shows Order 4711 saved".into(),
            },
        )
        .expect("shared assertion grammar");
        assert!(
            matches!(&actions[0], ResolvedAction::AssertText { target, .. } if *target == Target::Surface)
        );
    }

    /// Every example in docs/authoring.md must actually parse — the doc
    /// and the grammar are not allowed to drift (the evaluation that
    /// prompted the doc had to recover the grammar from this source file).
    #[test]
    fn documented_grammar_examples_all_resolve() {
        let plain: &[(&str, &str)] = &[
            ("web", r#"Type Ada into the "Full name" field"#),
            ("web", r#"Type email into the 2nd "Field Name" field"#),
            ("web", "Type Ada into the name field"),
            ("web", "Type Berlin"),
            ("web", r#"Replace the "Search" field with Berlin"#),
            ("web", "Replace the taskName field with Weekly report"),
            ("web", r#"Clear the "Search" field"#),
            ("web", "Clear the taskName field"),
            ("web", r#"Select Admin from the "Role" field"#),
            ("web", r#"Select Admin in the "Role" dropdown"#),
            ("web", r#"Press the "Save" button"#),
            ("web", "Press the submitButton button"),
            ("web", r#"Click "Templates""#),
            ("web", r#"Click the 2nd "Templates""#),
            ("web", "Press Enter"),
            ("web", "Press Control+V"),
            ("web", "Press Alt+Shift+Backspace"),
            ("web", "Go to /settings"),
            ("web", "Navigate to /settings"),
            ("web", "Reload the page"),
            ("web", "Wait until page shows templates found within 30s"),
            ("sap", "Go to /nVA01"),
            ("sap", r#"Type ZOR into the "Order Type" field"#),
            ("vision", r#"Press the "Submit" button"#),
            ("calc", "Type 53"),
            ("calc", "Press plus"),
            ("notepad", "Type hello"),
        ];
        for (app, step) in plain {
            resolve_step(app, &SpecStep::Plain((*step).to_string()))
                .unwrap_or_else(|e| panic!("documented step '{step}' ({app}) must parse: {e}"));
        }
        let asserts: &[(&str, &str)] = &[
            ("web", "page shows Welcome"),
            ("web", "the page shows Welcome"),
            ("web", "page shows templates found 2 times"),
            ("web", "page does not show Error"),
            ("web", "the page does not show Error"),
            ("web", r#"the "Field Name" field contains Street"#),
            ("web", "the templateName field contains Draft"),
            ("web", r#"the 2nd "Amount" field contains 10"#),
            ("web", r#"the "css:#live_preview" shows Street"#),
            ("web", r#"the "css:#modal" is visible"#),
            ("web", r#"the "css:#modal" is not visible within 15s"#),
            ("web", r#"the "Save" is enabled"#),
            ("web", r#"the "Save" is disabled"#),
            ("calc", "display shows 8"),
            ("notepad", "document contains hello"),
        ];
        for (app, assert) in asserts {
            resolve_step(
                app,
                &SpecStep::Assert {
                    assert: (*assert).to_string(),
                },
            )
            .unwrap_or_else(|e| panic!("documented assert '{assert}' ({app}) must parse: {e}"));
        }
    }

    #[test]
    fn navigate_synonym_and_page_article_are_accepted() {
        let actions = resolve_step("web", &SpecStep::Plain("Navigate to /settings".into()))
            .expect("synonym resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::Navigate {
                path: "/settings".into()
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the page shows Welcome".into(),
            },
        )
        .expect("article form resolves");
        assert!(
            matches!(&actions[0], ResolvedAction::AssertText { target, matcher, .. }
                if *target == Target::Surface && *matcher == TextMatch::Contains)
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: "the page does not show Error".into(),
            },
        )
        .expect("negated article form resolves");
        assert!(
            matches!(&actions[0], ResolvedAction::AssertText { matcher, .. }
                if *matcher == TextMatch::NotContains)
        );
    }

    #[test]
    fn enabled_and_disabled_are_first_class_assertions() {
        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: r#"the "Save" is disabled"#.into(),
            },
        )
        .expect("disabled resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertEnabled {
                target: Target::text("Save"),
                enabled: false,
                timeout_ms: ASSERT_TIMEOUT_MS,
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Assert {
                assert: r#"the "Save" is enabled within 5s"#.into(),
            },
        )
        .expect("enabled with timeout resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::AssertEnabled {
                target: Target::text("Save"),
                enabled: true,
                timeout_ms: 5000,
            }]
        );
    }

    #[test]
    fn replace_is_one_step_clear_plus_type() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain(r#"Replace the "Search" field with Berlin"#.into()),
        )
        .expect("replace resolves");
        assert_eq!(
            actions,
            vec![
                ResolvedAction::Clear {
                    target: Target::text("Search")
                },
                ResolvedAction::TypeText {
                    target: Target::text("Search"),
                    text: "Berlin".into()
                },
            ]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Plain("Replace the taskName field with Weekly report".into()),
        )
        .expect("id form resolves");
        assert_eq!(
            actions,
            vec![
                ResolvedAction::Clear {
                    target: Target::id("taskName")
                },
                ResolvedAction::TypeText {
                    target: Target::id("taskName"),
                    text: "Weekly report".into()
                },
            ]
        );
    }

    #[test]
    fn select_resolves_to_type_text_on_the_dropdown() {
        let actions = resolve_step(
            "web",
            &SpecStep::Plain(r#"Select Admin from the "Role" dropdown"#.into()),
        )
        .expect("select resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::text("Role"),
                text: "Admin".into()
            }]
        );

        let actions = resolve_step(
            "web",
            &SpecStep::Plain(r#"Select Admin in the "css:#role" field"#.into()),
        )
        .expect("css escape hatch works");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::css("#role"),
                text: "Admin".into()
            }]
        );
    }

    #[test]
    fn id_labels_address_native_ids_directly() {
        let actions = resolve_step(
            "sap",
            &SpecStep::Plain(r#"Type 4711 into the "id:wnd[0]/usr/txtVBAK-KUNNR" field"#.into()),
        )
        .expect("id: label resolves");
        assert_eq!(
            actions,
            vec![ResolvedAction::TypeText {
                target: Target::id("wnd[0]/usr/txtVBAK-KUNNR"),
                text: "4711".into()
            }]
        );
    }
}
