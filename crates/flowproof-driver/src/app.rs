//! Application driving via UI Automation: launch a target app, find elements
//! by native selector, invoke them, and read their text. This is the surface
//! the recorder and replayer use.
//!
//! The Windows implementation wraps the `uiautomation` crate (a safe wrapper
//! over the Win32 UIA COM API). Elsewhere the stub returns
//! [`DriverError::UnsupportedPlatform`].

use std::time::Duration;

use crate::DriverError;

/// A native element selector. UIA drivers match the UIA properties (all set
/// fields must match); browser drivers match `css` (falling back to
/// `#automation_id`). A rename to `ElementSelector` is planned once the
/// surface settles.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiaSelector {
    pub automation_id: Option<String>,
    pub name: Option<String>,
    pub control_type: Option<String>,
    pub css: Option<String>,
    /// 1-based ordinal when several elements match (`the 2nd "Field Name"`).
    pub nth: Option<u32>,
    /// Spatial hint for pixels-only drivers: where the ACTION POINT sits
    /// relative to the matched text anchor (`inside`, `right_of`, …).
    /// Tree-backed drivers (UIA, web, SAP) ignore it — their match IS the
    /// element.
    pub relation: Option<String>,
    /// A table cell addressed by identity (#58). Web-only; resolved against
    /// the live DOM by column header text and row anchor, never position.
    pub cell: Option<CellQuery>,
    /// An element addressed INSIDE a container identified by an anchor
    /// (`the "Amount" in the item containing "Invoice 4711"`). Web-only,
    /// like `cell`: the container is found first, then the ordinary
    /// resolution ladder runs rooted at it.
    pub scope: Option<ScopeQuery>,
}

/// A table cell to resolve by IDENTITY, not position (#58). `column` is the
/// header's visible text and `anchor` is text identifying the row; the
/// optional hints (`column_field` = a `data-field`/`col-id`/`column-{field}`
/// value, `row_id` = a row's `id`/`data-id`/`row-id`) are harvested at
/// record time and used as fallbacks so a renamed header or a text-edited
/// anchor still resolves.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellQuery {
    pub column: String,
    pub anchor: String,
    pub column_field: Option<String>,
    pub row_id: Option<String>,
}

/// Record-time hints harvested from a resolved cell (#58): the column's
/// field attribute and the row's id, recorded into the trace so replay can
/// fall back to them when the header text or the row anchor has since
/// changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CellHints {
    pub column_field: Option<String>,
    pub row_id: Option<String>,
}

/// An element addressed inside a CONTAINER identified by an anchor. The
/// container is either the bare word `item` (a closed list of list-ish
/// roles) or an explicit `css:`/`id:` selector, written exactly as the spec
/// wrote it; `anchor` is text identifying WHICH container. The inner target
/// is the ordinary one - css, native id, or text anchor - resolved rooted
/// at the container, never page-wide.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeQuery {
    pub container: String,
    pub anchor: String,
    pub inner_css: Option<String>,
    pub inner_id: Option<String>,
    pub inner_text: Option<String>,
    /// Record-time hint: the container's own id, used as a fallback when
    /// the anchor text has since changed (the `row_id` analog).
    pub container_id: Option<String>,
}

impl ScopeQuery {
    /// The CSS selector for the inner target, if it is addressed that way.
    pub fn inner_css_selector(&self) -> Option<String> {
        self.inner_css
            .clone()
            .or_else(|| self.inner_id.as_ref().map(|id| format!("#{id}")))
    }
}

/// Record-time hint and failure diagnostic for a scoped-container target:
/// the container's id (the `row_id` analog), plus whether the anchor is on
/// the surface without sitting in any container at all - the one miss whose
/// timeout message should name the fix rather than say "not found".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScopeHints {
    pub container_id: Option<String>,
    pub anchor_without_container: bool,
}

impl UiaSelector {
    pub fn automation_id(id: impl Into<String>) -> Self {
        Self {
            automation_id: Some(id.into()),
            ..Self::default()
        }
    }

    pub fn css(selector: impl Into<String>) -> Self {
        Self {
            css: Some(selector.into()),
            ..Self::default()
        }
    }

    /// The CSS selector a browser driver should use, if any.
    pub fn css_selector(&self) -> Option<String> {
        self.css
            .clone()
            .or_else(|| self.automation_id.as_ref().map(|id| format!("#{id}")))
    }

    pub fn is_empty(&self) -> bool {
        self.automation_id.is_none()
            && self.name.is_none()
            && self.control_type.is_none()
            && self.css.is_none()
            && self.cell.is_none()
            && self.scope.is_none()
    }

    pub fn with_nth(mut self, nth: Option<u32>) -> Self {
        self.nth = nth;
        self
    }

    pub fn with_relation(mut self, relation: Option<String>) -> Self {
        self.relation = relation;
        self
    }
}

impl std::fmt::Display for UiaSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();
        if let Some(id) = &self.automation_id {
            parts.push(format!("automation_id={id}"));
        }
        if let Some(name) = &self.name {
            parts.push(format!("name={name}"));
        }
        if let Some(ct) = &self.control_type {
            parts.push(format!("control_type={ct}"));
        }
        if let Some(css) = &self.css {
            parts.push(format!("css={css}"));
        }
        if let Some(nth) = self.nth {
            parts.push(format!("nth={nth}"));
        }
        if let Some(scope) = &self.scope {
            let inner = scope
                .inner_css_selector()
                .or_else(|| scope.inner_text.clone())
                .unwrap_or_default();
            parts.push(format!(
                "\"{inner}\" in the {} containing \"{}\"",
                scope.container, scope.anchor
            ));
        }
        write!(f, "{}", parts.join(","))
    }
}

/// `scheme://host[:port]` of `url`, if it has a scheme.
pub fn url_origin(url: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let rest = &url[scheme_end + 3..];
    let host_end = rest.find('/').unwrap_or(rest.len());
    Some(format!("{}{}", &url[..scheme_end + 3], &rest[..host_end]))
}

/// Absolutize `path` against `base`'s origin: `/settings` on
/// `http://host:3000/templates` → `http://host:3000/settings`. Full URLs
/// pass through unchanged.
pub fn absolute_url(path: &str, base: &str) -> String {
    if path.contains("://") {
        return path.to_string();
    }
    match url_origin(base) {
        Some(origin) if path.starts_with('/') => format!("{origin}{path}"),
        Some(origin) => format!("{origin}/{path}"),
        None => path.to_string(),
    }
}

/// Pre-launch session state with RESOLVED values (secret references are
/// resolved by the caller before staging — the driver never sees `${VAR}`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebSession {
    /// `(name, value, domain)` — domain None derives from the launch URL.
    pub cookies: Vec<(String, String, Option<String>)>,
    /// Seeded into localStorage before any page script runs.
    pub local_storage: Vec<(String, String)>,
}

/// A keyboard modifier held while pressing a key (`Ctrl+V`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyMod {
    Ctrl,
    Alt,
    Shift,
    Meta,
}

impl std::fmt::Display for KeyMod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            KeyMod::Ctrl => "Ctrl",
            KeyMod::Alt => "Alt",
            KeyMod::Shift => "Shift",
            KeyMod::Meta => "Meta",
        })
    }
}

/// Where a `Scroll` step moves the surface. `Top`/`Bottom` scroll a
/// container (or the page, when the target is absent) to its edge; `IntoView`
/// brings an in-DOM element into the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollTo {
    Top,
    Bottom,
    IntoView,
}

/// Drives a single application window through UIA.
pub trait AppDriver {
    /// Record-time hints for a resolved table cell (#58). Only the web
    /// adapter overrides this - a cell is a DOM concept - and it does so
    /// only for a `selector` carrying a `cell` query. The default (every
    /// other adapter, and a non-cell selector) harvests nothing, which is
    /// valid: a text-only cell payload still resolves by identity.
    fn cell_hints(&mut self, _selector: &UiaSelector) -> Result<Option<CellHints>, DriverError> {
        Ok(None)
    }

    /// Record-time hints and failure diagnostics for a scoped-container
    /// target. Web-only for the same reason `cell_hints` is: a container is
    /// a DOM concept. The default harvests nothing, which is valid - a
    /// text-only scoped payload still resolves by identity.
    fn scope_hints(&mut self, _selector: &UiaSelector) -> Result<Option<ScopeHints>, DriverError> {
        Ok(None)
    }

    /// Launch (or attach to) the target application and wait until a window
    /// whose name contains `window_name` exists.
    fn launch(
        &mut self,
        command: &str,
        window_name: &str,
        timeout: Duration,
    ) -> Result<(), DriverError>;

    /// Whether an element matching `selector` currently exists in the target
    /// window.
    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError>;

    /// Invoke (click) the element matching `selector`.
    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError>;

    /// Read the visible text of the element matching `selector` (Value
    /// pattern when available, element Name otherwise).
    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError>;

    /// Type `text` into the element matching `selector` (focus + keystrokes).
    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError>;

    /// Clear the current value of the input matching `selector`.
    fn clear_text(&mut self, _selector: &UiaSelector) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "clear_text is not supported by this driver".into(),
        ))
    }

    /// Type `text` into whatever element currently has keyboard focus
    /// (dropdown search boxes, rename inputs that appear pre-focused).
    fn type_focused(&mut self, _text: &str) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "type_focused is not supported by this driver".into(),
        ))
    }

    /// Press a named key (`Enter`, `Escape`, `Backspace`, `V`, …) with the
    /// given modifiers held.
    fn press_key(&mut self, _key: &str, _modifiers: &[KeyMod]) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "press_key is not supported by this driver".into(),
        ))
    }

    /// Whether the element matching `selector` is interactive right now —
    /// backs `the "<target>" is enabled|disabled` assertions. Disabled
    /// means the platform's own notion: `disabled`/`aria-disabled` on the
    /// web, UIA IsEnabled on desktop.
    fn element_enabled(&mut self, _selector: &UiaSelector) -> Result<bool, DriverError> {
        Err(DriverError::Uia(
            "enabled/disabled assertions are not supported by this driver".into(),
        ))
    }

    /// All text currently readable on the app's surface — the whole page
    /// for a browser, the foreground window's subtree for a desktop app,
    /// the OCR'd frame for a vision adapter. Backs surface-level assertions
    /// (`page shows X`) without tying them to any one provenance.
    fn surface_text(&mut self) -> Result<String, DriverError> {
        Err(DriverError::Uia(
            "surface_text is not supported by this driver".into(),
        ))
    }

    /// Size and position the driven window, returning what was ACTUALLY
    /// applied. When the caller passes no position, the implementation
    /// reports where the window landed, so the trace can pin it for replay:
    /// an unpinned position becomes a pinned one for free.
    ///
    /// Geometry is a determinism precondition for visual assertions, so a
    /// failure to apply it is an error, never a warning - a silently
    /// unsized window mints a flaky baseline.
    fn set_window_geometry(
        &mut self,
        _width: u32,
        _height: u32,
        _position: Option<(i32, i32)>,
    ) -> Result<(u32, u32, i32, i32), DriverError> {
        Err(DriverError::Uia(
            "window geometry is not supported by this driver".into(),
        ))
    }

    /// Whether a checkbox-like control is checked. `None` when the target
    /// is not one, so an assertion can say precisely that instead of
    /// reporting a confident `false`.
    fn element_checked(&mut self, _selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        Err(DriverError::Uia(
            "element_checked is not supported by this driver".into(),
        ))
    }

    /// Drive a checkbox-like control to `checked`. SET-state, not toggle:
    /// idempotent, so a reseeded or drifted environment cannot silently
    /// invert what the step means. Implementations act like a user - a real
    /// click, so the app's own handlers fire - and then verify the state
    /// actually took.
    fn set_checked(&mut self, _selector: &UiaSelector, _checked: bool) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "set_checked is not supported by this driver".into(),
        ))
    }

    /// The raw value of the element's `<name>` DOM attribute, ASCII
    /// case-insensitively (`getAttribute`). `None` = the attribute is ABSENT;
    /// `Some("")` = present with an empty value (`download=""`) - the two are
    /// distinct answers and a value assertion needs both. Only the web
    /// adapter has DOM attributes; the default refuses with a reason rather
    /// than inventing an absence, exactly like `current_url` (the `page url
    /// is` precedent).
    fn element_attribute(
        &mut self,
        _selector: &UiaSelector,
        _name: &str,
    ) -> Result<Option<String>, DriverError> {
        Err(DriverError::Uia(
            "this app's elements have no DOM attributes: attribute assertions are for web flows"
                .into(),
        ))
    }

    /// The element's COMPUTED value for a single CSS property (`color`,
    /// `background-color`, `text-transform`), read via `getComputedStyle`.
    /// Web-only: the default refuses with a reason - a UIA/SAP/vision element
    /// has no computed style - rather than inventing a value.
    fn element_computed_style(
        &mut self,
        _selector: &UiaSelector,
        _prop: &str,
    ) -> Result<String, DriverError> {
        Err(DriverError::Uia(
            "this app's elements have no computed style: style assertions are for web flows".into(),
        ))
    }

    /// Scroll the element matching `selector` (or the page, when `selector`
    /// is `None`) per `to`. Instant, with no settle-wait: the next assertion
    /// auto-waits. Implementations verify the scroll took (position reached
    /// the edge, or the element's rect is within the viewport), like
    /// `set_checked`, so a scroll that did nothing fails the step.
    fn scroll(
        &mut self,
        _selector: Option<&UiaSelector>,
        _to: ScrollTo,
    ) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "scrolling is not supported by this driver (web flows only)".into(),
        ))
    }

    /// The surface's current location, for `page url is|contains`. Only a
    /// browser has one: a UIA window, a SAP session and an OCR'd frame do
    /// not, so the default refuses with a reason rather than inventing an
    /// empty string that would make assertions quietly pass.
    fn current_url(&mut self) -> Result<String, DriverError> {
        Err(DriverError::Uia(
            "this app has no URL: `page url` assertions are for web flows".into(),
        ))
    }

    /// Stage session state (cookies, localStorage) to be applied by the
    /// NEXT `launch` before the page loads.
    fn stage_session(&mut self, _session: WebSession) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "stage_session is not supported by this driver".into(),
        ))
    }

    /// Navigate the current page to `url` (mid-flow `Go to /path`).
    fn navigate(&mut self, _url: &str) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "navigate is not supported by this driver".into(),
        ))
    }

    /// Reload the current page.
    fn reload(&mut self) -> Result<(), DriverError> {
        Err(DriverError::Uia(
            "reload is not supported by this driver".into(),
        ))
    }

    /// Primary screen size in physical pixels (used for trace headers).
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError>;

    /// Capture the current frame. `Ok(None)` means this driver cannot
    /// capture (recording is skipped gracefully, never silently faked).
    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        Ok(None)
    }

    /// Screen rectangle of the element matching `selector`, if present.
    fn element_rect(&mut self, _selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        Ok(None)
    }

    /// Screen rectangles of every password field currently on screen —
    /// these are ALWAYS masked in persisted frames.
    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        Ok(Vec::new())
    }

    /// Structured observation of the current UI for authoring: a JSON array
    /// of interactable elements (selector, role, label/text). `Ok(None)`
    /// means this driver cannot describe its scene yet — LLM authoring is
    /// unavailable on it.
    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        Ok(None)
    }

    /// What the app looked like at the moment a step failed — captured
    /// best-effort by the replayer into the run bundle so the first
    /// question ("what was actually on screen?") is answered without a
    /// re-run. `Ok(None)` = this driver has nothing beyond the recording.
    fn debug_bundle(&mut self) -> Result<Option<DebugBundle>, DriverError> {
        Ok(None)
    }

    /// Would a click at the element's center actually reach it (not a
    /// toast, modal backdrop, or overlay)? `Ok(None)` = this driver cannot
    /// tell — the actionability gate treats unknown as satisfied.
    fn element_receives_events(
        &mut self,
        _selector: &UiaSelector,
    ) -> Result<Option<bool>, DriverError> {
        Ok(None)
    }

    /// Stage network mocks to apply at the next `launch` (before the page
    /// loads). Drivers without a network layer must REJECT non-empty rules
    /// — silently ignoring a mock would change what the flow tests.
    fn stage_mocks(&mut self, rules: Vec<WebMock>) -> Result<(), DriverError> {
        if rules.is_empty() {
            return Ok(());
        }
        Err(DriverError::Uia(
            "network mocks are not supported by this driver (web flows only)".into(),
        ))
    }

    /// Set the files of a file-chooser input (Playwright's
    /// `setInputFiles`). The input is commonly hidden behind a styled
    /// button, so implementations must NOT require visibility.
    fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError> {
        let _ = paths;
        Err(DriverError::Uia(format!(
            "file upload is not supported by this driver (web flows only): [{selector}]"
        )))
    }

    /// Right-click an element (open its context menu).
    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        Err(DriverError::Uia(format!(
            "right-click is not supported by this driver yet: [{selector}]"
        )))
    }

    /// Stage browser launch/emulation config (viewport, user-agent, extra
    /// flags) to apply at the next `launch`. Drivers without a browser
    /// must REJECT it — silently ignoring emulation would change what the
    /// flow tests.
    fn stage_browser(&mut self, config: WebBrowserConfig) -> Result<(), DriverError> {
        let what = if config.clock.is_some() {
            "clock control"
        } else {
            "browser emulation"
        };
        Err(DriverError::Uia(format!(
            "{what} is not supported by this driver (web flows only)"
        )))
    }
}

/// Fully-resolved browser launch/emulation config for the web driver:
/// the driver-side mirror of the trace's `BrowserSetup`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebBrowserConfig {
    /// `(width, height, device_scale_factor, mobile, touch)`.
    pub viewport: Option<WebViewport>,
    pub user_agent: Option<String>,
    /// Extra Chrome flags — forces a private (non-shared) browser, since
    /// flags only apply at process start.
    pub args: Vec<String>,
    /// A pinned clock, applied before navigation (GAP-P).
    pub clock: Option<WebClock>,
}

/// A pinned browser clock: the literal instant the page reads as "now" and
/// an optional timezone. Applied identically at record and replay.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebClock {
    pub at: String,
    pub timezone: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WebViewport {
    pub width: u32,
    pub height: u32,
    pub device_scale_factor: f64,
    pub mobile: bool,
    pub touch: bool,
}

impl WebBrowserConfig {
    /// Build from the trace-format setup parts, applying the defaults ONCE
    /// for record AND replay: device scale 1.0, desktop, no touch.
    /// `viewport` is `(width, height, device_scale_factor, mobile, touch)`.
    #[allow(clippy::type_complexity)]
    pub fn from_setup_parts(
        viewport: Option<(u32, u32, Option<f64>, Option<bool>, Option<bool>)>,
        user_agent: Option<&str>,
        args: &[String],
        clock: Option<WebClock>,
    ) -> Self {
        Self {
            viewport: viewport.map(|(width, height, dsf, mobile, touch)| WebViewport {
                width,
                height,
                device_scale_factor: dsf.unwrap_or(1.0),
                mobile: mobile.unwrap_or(false),
                touch: touch.unwrap_or(false),
            }),
            user_agent: user_agent.map(str::to_string),
            args: args.to_vec(),
            clock,
        }
    }
}

/// One fully-resolved network mock the web driver serves in place of a
/// live response: match by URL substring (+ optional uppercase method),
/// answer with the canned status/content-type/body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebMock {
    pub url_contains: String,
    pub method: Option<String>,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
}

impl WebMock {
    /// Build from the trace-format rule parts, applying the body
    /// conventions once for record AND replay: a JSON string body is
    /// served verbatim (`text/plain` default), any other JSON serializes
    /// (`application/json` default); an explicit content type overrides.
    pub fn from_rule_parts(
        url_contains: &str,
        method: Option<&str>,
        status: u16,
        content_type: Option<&str>,
        body: Option<&serde_json::Value>,
    ) -> Self {
        let (default_ct, bytes) = match body {
            None => ("text/plain", Vec::new()),
            Some(serde_json::Value::String(s)) => ("text/plain", s.clone().into_bytes()),
            Some(other) => (
                "application/json",
                serde_json::to_vec(other).unwrap_or_default(),
            ),
        };
        Self {
            url_contains: url_contains.to_string(),
            method: method.map(|m| m.to_ascii_uppercase()),
            status,
            content_type: content_type.unwrap_or(default_ct).to_string(),
            body: bytes,
        }
    }
}

/// Failure-time diagnostics a driver can capture. All fields best-effort.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DebugBundle {
    /// Full serialized DOM (web) or equivalent structural dump.
    pub dom_html: Option<String>,
    /// Recent console/log lines, oldest first (bounded ring buffer).
    pub console: Vec<String>,
}

/// `(x, y, width, height)` in frame pixels.
pub type PixelRect = (i32, i32, u32, u32);

/// How to launch a known application id from a flow spec. For the `web`
/// pseudo-app, `command` is the URL to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTarget {
    pub command: String,
    pub window_name: String,
}

/// Extract the trailing numeric value from display text like
/// `Display is 8` or `1,234.5`. Shared by record and replay so both phases
/// judge display values identically.
/// Split a command LINE into the program and the rest, honouring quotes so
/// a path containing spaces survives: `"C:\\Program Files\\app.exe" --flag`.
///
/// `app.command` is a command line, not a program name, because that is what
/// a person pastes from a shortcut. Pure string logic, so it is tested on
/// every platform even though only the Windows driver uses it.
pub fn split_command_line(command: &str) -> Option<(String, String)> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    if let Some(rest) = command.strip_prefix('"') {
        let (program, tail) = rest.split_once('"')?;
        if program.is_empty() {
            return None;
        }
        return Some((program.to_string(), tail.trim_start().to_string()));
    }
    match command.split_once(char::is_whitespace) {
        Some((program, tail)) => Some((program.to_string(), tail.trim_start().to_string())),
        None => Some((command.to_string(), String::new())),
    }
}

/// Compare a live reading against a CAPTURED value, optionally offset by a
/// literal (`${captured.balance} - 100`).
///
/// Lives here beside [`numeric_value`] and [`url_matches`] so record and
/// replay share ONE implementation: two copies of a matcher drift, and a
/// drifted matcher is how a trace gets minted that cannot replay.
///
/// With no offset this is a text comparison with the same exact-then-ASCII
/// -case-insensitive ladder every `shows` assertion uses. With an offset
/// both sides are read as numbers, because "the balance dropped by 100" is
/// arithmetic and nothing else. `Err` names which side failed to parse, so
/// the failure says what to fix rather than just "no match".
pub fn capture_matches(captured: &str, offset: Option<f64>, actual: &str) -> Result<bool, String> {
    let Some(offset) = offset else {
        return Ok(text_contains(actual, captured));
    };
    let Some(base) = numeric_value(captured) else {
        return Err(format!(
            "the captured value '{captured}' is not numeric, so it cannot be offset"
        ));
    };
    let Some(seen) = numeric_value(actual) else {
        return Err(format!("the live text '{actual}' is not numeric"));
    };
    let expected = base + offset;
    // Decimal subtraction on f64 leaves representation noise (0.1 + 0.2),
    // and money is the whole point of this feature, so compare with a
    // relative epsilon rather than demanding bit equality.
    Ok((seen - expected).abs() <= 1e-6 * expected.abs().max(1.0))
}

/// Substring match with the ASCII case-insensitive fallback rung: exact
/// first, lowercased only if that misses. Widening-only, so it can never
/// turn a passing trace into a failing one.
pub fn text_contains(actual: &str, expected: &str) -> bool {
    actual.contains(expected) || actual.to_lowercase().contains(&expected.to_lowercase())
}

/// Does `actual` (a full URL) satisfy a `page url` expectation?
///
/// Shared by record and replay - like [`numeric_value`] - because a URL
/// assertion that holds while recording must hold when replayed, and two
/// copies of a matcher drift (that is exactly how the round-3 field
/// migration produced traces that could not replay).
///
/// `exact = false` is `page url contains <text>`: a plain substring test
/// over the whole href.
///
/// `exact = true` is `page url is <expected>`, which is deliberately
/// path-shaped, because `cy.location("pathname").should("equal", "/signin")`
/// is the assertion people actually write:
///
/// - `expected` containing `://` compares against the WHOLE url, exactly;
/// - `expected` starting with `/` compares against the pathname, and
///   includes the query only when `expected` carries a `?`, the fragment
///   only when it carries a `#`. So `/orders` ignores `?page=2`, while
///   `/orders?page=2` does not;
/// - anything else compares against the whole url, exactly. There is no
///   guessing: an expectation that is neither a path nor a full URL is
///   most likely a mistake, and an exact comparison says so loudly.
pub fn url_matches(expected: &str, exact: bool, actual: &str) -> bool {
    if !exact {
        return actual.contains(expected);
    }
    if expected.contains("://") || !expected.starts_with('/') {
        return actual == expected;
    }
    let (want_query, want_hash) = (expected.contains('?'), expected.contains('#'));
    // Split the live URL into path / query / fragment without pulling in a
    // URL crate: after the scheme's `://`, the path starts at the first
    // `/`, and `?` / `#` bound it from the right.
    let after_scheme = match actual.find("://") {
        Some(i) => &actual[i + 3..],
        None => actual,
    };
    let from_path = match after_scheme.find('/') {
        Some(i) => &after_scheme[i..],
        // No path at all ("https://example.test") means the root.
        None => "/",
    };
    let hash_at = from_path.find('#');
    let without_hash = match hash_at {
        Some(i) => &from_path[..i],
        None => from_path,
    };
    let hash = hash_at.map(|i| &from_path[i..]).unwrap_or("");
    let query_at = without_hash.find('?');
    let path = match query_at {
        Some(i) => &without_hash[..i],
        None => without_hash,
    };
    let query = query_at.map(|i| &without_hash[i..]).unwrap_or("");
    let mut candidate = String::from(path);
    if want_query {
        candidate.push_str(query);
    }
    if want_hash {
        candidate.push_str(hash);
    }
    candidate == expected
}

pub fn numeric_value(text: &str) -> Option<f64> {
    text.split_whitespace().rev().find_map(|token| {
        // Grouping separators and a currency symbol are formatting, not
        // value: "$1,000.00" and "1000.00" are the same number, and the
        // motivating case for computed assertions is an account balance.
        // Widening only - a token that parsed before still parses to the
        // same number, so no passing assertion can start failing.
        let cleaned: String = token
            .chars()
            .filter(|c| !matches!(c, ',' | '$' | '€' | '£' | '¥' | '\u{a0}'))
            .collect();
        cleaned.parse::<f64>().ok()
    })
}

/// The CLOSED allowlist of CSS properties a `style` assertion may name.
/// Anything else is a parse error - `style` is not a generic css escape
/// hatch (that is `css:` on the selector). Geometry belongs in
/// `assert_screenshot`, visibility in `is visible`.
pub const STYLE_PROPS: [&str; 3] = ["color", "background-color", "text-transform"];

/// A `style <prop>` value assertion. `color`/`background-color` compare
/// CANONICALLY (parse both sides to RGBA); `text-transform` compares its
/// keyword ASCII case-insensitively. Shared by record and replay so the two
/// executions cannot disagree - the same reason `url_matches` and
/// `numeric_value` live here.
///
/// `Err` is a hard, no-wait failure that names what was seen: an unparseable
/// COMPUTED color cannot become a color by waiting (it fails regardless of
/// negation), and a non-color EXPECTED value is a spec mistake.
pub fn style_matches(
    prop: &str,
    expected: &str,
    negate: bool,
    actual: &str,
) -> Result<bool, String> {
    if prop.eq_ignore_ascii_case("color") || prop.eq_ignore_ascii_case("background-color") {
        let seen = parse_css_color(actual).ok_or_else(|| {
            format!("the computed {prop} is '{actual}', which is not a color value")
        })?;
        let want = parse_css_color(expected)
            .ok_or_else(|| format!("'{expected}' is not a color value"))?;
        let eq = seen == want;
        return Ok(if negate { !eq } else { eq });
    }
    // text-transform (and any future keyword prop): a plain keyword compare.
    let eq = actual.trim().eq_ignore_ascii_case(expected.trim());
    Ok(if negate { !eq } else { eq })
}

/// `attribute <name> is [not] <value>`: EXACT, case-SENSITIVE (attributes are
/// machine strings - no text ladder, no substring). An absent attribute never
/// equals a value, so the negative form passes when the attribute is missing
/// OR present with a different value. Shared by record and replay.
pub fn attribute_value_matches(expected: &str, negate: bool, actual: Option<&str>) -> bool {
    let eq = actual == Some(expected);
    if negate {
        !eq
    } else {
        eq
    }
}

/// Parse a CSS color to `[r, g, b, a]` (each 0..=255). Understands named CSS
/// colors, `#rgb`/`#rrggbb` hex, and `rgb()`/`rgba()` (comma- or
/// space-separated, alpha as a 0..1 float or a percentage). Two colors are
/// equal iff their RGBA quadruples are equal, so `red`, `#f00`, `#ff0000` and
/// `rgb(255, 0, 0)` all match the computed `rgb(255, 0, 0)`.
pub fn parse_css_color(input: &str) -> Option<[u8; 4]> {
    let s = input.trim();
    if let Some(hex) = s.strip_prefix('#') {
        return parse_hex_color(hex);
    }
    let lower = s.to_ascii_lowercase();
    if let Some(inner) = func_body(&lower, "rgba").or_else(|| func_body(&lower, "rgb")) {
        return parse_rgb_components(inner);
    }
    named_css_color(&lower)
}

/// The `…` inside `name(…)`, trimmed - or `None` if `s` is not that call.
fn func_body<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    s.strip_prefix(name)?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
        .map(str::trim)
}

fn parse_hex_color(hex: &str) -> Option<[u8; 4]> {
    let bytes = hex.as_bytes();
    let hx = |c: u8| (c as char).to_digit(16);
    match bytes.len() {
        // #rgb -> each nibble doubled.
        3 => {
            let r = hx(bytes[0])? as u8;
            let g = hx(bytes[1])? as u8;
            let b = hx(bytes[2])? as u8;
            Some([r * 17, g * 17, b * 17, 255])
        }
        // #rrggbb.
        6 => {
            let pair = |i: usize| Some((hx(bytes[i])? * 16 + hx(bytes[i + 1])?) as u8);
            Some([pair(0)?, pair(2)?, pair(4)?, 255])
        }
        _ => None,
    }
}

fn parse_rgb_components(inner: &str) -> Option<[u8; 4]> {
    // Accept both the legacy `r, g, b[, a]` and the CSS4 space form
    // `r g b / a`: split on commas, whitespace, and the alpha slash.
    let parts: Vec<&str> = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|t| !t.is_empty())
        .collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let channel = |t: &str| -> Option<u8> {
        if let Some(pct) = t.strip_suffix('%') {
            let v: f64 = pct.trim().parse().ok()?;
            Some((v / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8)
        } else {
            let v: f64 = t.parse().ok()?;
            Some(v.round().clamp(0.0, 255.0) as u8)
        }
    };
    let r = channel(parts[0])?;
    let g = channel(parts[1])?;
    let b = channel(parts[2])?;
    let a = match parts.get(3) {
        None => 255u8,
        Some(t) => {
            if let Some(pct) = t.strip_suffix('%') {
                let v: f64 = pct.trim().parse().ok()?;
                (v / 100.0 * 255.0).round().clamp(0.0, 255.0) as u8
            } else {
                let v: f64 = t.parse().ok()?;
                (v * 255.0).round().clamp(0.0, 255.0) as u8
            }
        }
    };
    Some([r, g, b, a])
}

/// The CSS named colors (extended keyword set), each as `[r, g, b]`. Only the
/// EXPECTED side of a comparison is ever a name - `getComputedStyle` returns
/// `rgb()`/`rgba()` - but a spec writes `green`, so the mapping must be here.
fn named_css_color(name: &str) -> Option<[u8; 4]> {
    let rgb: [u8; 3] = match name {
        "transparent" => return Some([0, 0, 0, 0]),
        "black" => [0, 0, 0],
        "silver" => [192, 192, 192],
        "gray" | "grey" => [128, 128, 128],
        "white" => [255, 255, 255],
        "maroon" => [128, 0, 0],
        "red" => [255, 0, 0],
        "purple" => [128, 0, 128],
        "fuchsia" | "magenta" => [255, 0, 255],
        "green" => [0, 128, 0],
        "lime" => [0, 255, 0],
        "olive" => [128, 128, 0],
        "yellow" => [255, 255, 0],
        "navy" => [0, 0, 128],
        "blue" => [0, 0, 255],
        "teal" => [0, 128, 128],
        "aqua" | "cyan" => [0, 255, 255],
        "orange" => [255, 165, 0],
        "aliceblue" => [240, 248, 255],
        "antiquewhite" => [250, 235, 215],
        "aquamarine" => [127, 255, 212],
        "azure" => [240, 255, 255],
        "beige" => [245, 245, 220],
        "bisque" => [255, 228, 196],
        "blanchedalmond" => [255, 235, 205],
        "blueviolet" => [138, 43, 226],
        "brown" => [165, 42, 42],
        "burlywood" => [222, 184, 135],
        "cadetblue" => [95, 158, 160],
        "chartreuse" => [127, 255, 0],
        "chocolate" => [210, 105, 30],
        "coral" => [255, 127, 80],
        "cornflowerblue" => [100, 149, 237],
        "cornsilk" => [255, 248, 220],
        "crimson" => [220, 20, 60],
        "darkblue" => [0, 0, 139],
        "darkcyan" => [0, 139, 139],
        "darkgoldenrod" => [184, 134, 11],
        "darkgray" | "darkgrey" => [169, 169, 169],
        "darkgreen" => [0, 100, 0],
        "darkkhaki" => [189, 183, 107],
        "darkmagenta" => [139, 0, 139],
        "darkolivegreen" => [85, 107, 47],
        "darkorange" => [255, 140, 0],
        "darkorchid" => [153, 50, 204],
        "darkred" => [139, 0, 0],
        "darksalmon" => [233, 150, 122],
        "darkseagreen" => [143, 188, 143],
        "darkslateblue" => [72, 61, 139],
        "darkslategray" | "darkslategrey" => [47, 79, 79],
        "darkturquoise" => [0, 206, 209],
        "darkviolet" => [148, 0, 211],
        "deeppink" => [255, 20, 147],
        "deepskyblue" => [0, 191, 255],
        "dimgray" | "dimgrey" => [105, 105, 105],
        "dodgerblue" => [30, 144, 255],
        "firebrick" => [178, 34, 34],
        "floralwhite" => [255, 250, 240],
        "forestgreen" => [34, 139, 34],
        "gainsboro" => [220, 220, 220],
        "ghostwhite" => [248, 248, 255],
        "gold" => [255, 215, 0],
        "goldenrod" => [218, 165, 32],
        "greenyellow" => [173, 255, 47],
        "honeydew" => [240, 255, 240],
        "hotpink" => [255, 105, 180],
        "indianred" => [205, 92, 92],
        "indigo" => [75, 0, 130],
        "ivory" => [255, 255, 240],
        "khaki" => [240, 230, 140],
        "lavender" => [230, 230, 250],
        "lavenderblush" => [255, 240, 245],
        "lawngreen" => [124, 252, 0],
        "lemonchiffon" => [255, 250, 205],
        "lightblue" => [173, 216, 230],
        "lightcoral" => [240, 128, 128],
        "lightcyan" => [224, 255, 255],
        "lightgoldenrodyellow" => [250, 250, 210],
        "lightgray" | "lightgrey" => [211, 211, 211],
        "lightgreen" => [144, 238, 144],
        "lightpink" => [255, 182, 193],
        "lightsalmon" => [255, 160, 122],
        "lightseagreen" => [32, 178, 170],
        "lightskyblue" => [135, 206, 250],
        "lightslategray" | "lightslategrey" => [119, 136, 153],
        "lightsteelblue" => [176, 196, 222],
        "lightyellow" => [255, 255, 224],
        "limegreen" => [50, 205, 50],
        "linen" => [250, 240, 230],
        "mediumaquamarine" => [102, 205, 170],
        "mediumblue" => [0, 0, 205],
        "mediumorchid" => [186, 85, 211],
        "mediumpurple" => [147, 112, 219],
        "mediumseagreen" => [60, 179, 113],
        "mediumslateblue" => [123, 104, 238],
        "mediumspringgreen" => [0, 250, 154],
        "mediumturquoise" => [72, 209, 204],
        "mediumvioletred" => [199, 21, 133],
        "midnightblue" => [25, 25, 112],
        "mintcream" => [245, 255, 250],
        "mistyrose" => [255, 228, 225],
        "moccasin" => [255, 228, 181],
        "navajowhite" => [255, 222, 173],
        "oldlace" => [253, 245, 230],
        "olivedrab" => [107, 142, 35],
        "orangered" => [255, 69, 0],
        "orchid" => [218, 112, 214],
        "palegoldenrod" => [238, 232, 170],
        "palegreen" => [152, 251, 152],
        "paleturquoise" => [175, 238, 238],
        "palevioletred" => [219, 112, 147],
        "papayawhip" => [255, 239, 213],
        "peachpuff" => [255, 218, 185],
        "peru" => [205, 133, 63],
        "pink" => [255, 192, 203],
        "plum" => [221, 160, 221],
        "powderblue" => [176, 224, 230],
        "rosybrown" => [188, 143, 143],
        "royalblue" => [65, 105, 225],
        "saddlebrown" => [139, 69, 19],
        "salmon" => [250, 128, 114],
        "sandybrown" => [244, 164, 96],
        "seagreen" => [46, 139, 87],
        "seashell" => [255, 245, 238],
        "sienna" => [160, 82, 45],
        "skyblue" => [135, 206, 235],
        "slateblue" => [106, 90, 205],
        "slategray" | "slategrey" => [112, 128, 144],
        "snow" => [255, 250, 250],
        "springgreen" => [0, 255, 127],
        "steelblue" => [70, 130, 180],
        "tan" => [210, 180, 140],
        "thistle" => [216, 191, 216],
        "tomato" => [255, 99, 71],
        "turquoise" => [64, 224, 208],
        "violet" => [238, 130, 238],
        "wheat" => [245, 222, 179],
        "whitesmoke" => [245, 245, 245],
        "yellowgreen" => [154, 205, 50],
        "rebeccapurple" => [102, 51, 153],
        _ => return None,
    };
    Some([rgb[0], rgb[1], rgb[2], 255])
}

/// How many times `expected` occurs in `text`, under the same widening the
/// count matcher applies: a nonzero case-sensitive count IS the count, and
/// the lowercased count is consulted only when the case-sensitive one found
/// nothing.
///
/// Shared so a count failure REPORTS the number the matcher COMPARED.
/// Record and replay both format their message from this, for the same
/// reason they already share `numeric_value` and `url_matches`: a diagnostic
/// that disagreed with the predicate would send a caller healing a trace
/// that is not broken.
pub fn text_occurrences(expected: &str, text: &str) -> usize {
    let sensitive = text.matches(expected).count();
    if sensitive > 0 {
        sensitive
    } else {
        text.to_lowercase()
            .matches(&expected.to_lowercase())
            .count()
    }
}

/// A driver for `app: api` flows: no UI, ever. `launch` is a no-op and
/// `screen_size` is nominal; any actual UI operation errors, because an
/// out-of-band flow that reached for the screen is a spec mistake worth
/// surfacing. Out-of-band assertions (`assert_sql`/`assert_api`) never
/// touch the driver — they run through the oob probes — so an api flow
/// executes entirely without one.
#[derive(Debug, Default)]
pub struct NoOpDriver;

impl NoOpDriver {
    pub fn new() -> Self {
        Self
    }

    fn no_ui(op: &str) -> DriverError {
        DriverError::Uia(format!(
            "app 'api' has no UI: '{op}' is not available — an api flow may only \
             use out-of-band steps (assert_api / assert_sql)"
        ))
    }
}

impl AppDriver for NoOpDriver {
    fn launch(&mut self, _command: &str, _window: &str, _t: Duration) -> Result<(), DriverError> {
        Ok(())
    }

    fn element_exists(&mut self, _selector: &UiaSelector) -> Result<bool, DriverError> {
        Ok(false)
    }

    fn invoke(&mut self, _selector: &UiaSelector) -> Result<(), DriverError> {
        Err(Self::no_ui("click"))
    }

    fn read_text(&mut self, _selector: &UiaSelector) -> Result<String, DriverError> {
        Err(Self::no_ui("read_text"))
    }

    fn type_text(&mut self, _selector: &UiaSelector, _text: &str) -> Result<(), DriverError> {
        Err(Self::no_ui("type_text"))
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        Ok((1, 1))
    }
}

/// Resolve a spec `app:` id to a launch target. Shared by record and replay
/// so a trace only needs to carry the app id.
/// How many elements match this anchor, asked as "is there a 1st? a 2nd?"
/// until one is missing, and never more than `cap` questions.
///
/// Counting rides on the ordinal every adapter ALREADY implements rather
/// than on a new driver capability, so a count means exactly what
/// `the 2nd "Row"` means on that adapter - web, UIA, SAP and vision stay
/// consistent by construction, and no adapter had to change to gain it.
///
/// Shared between record and replay for the usual reason: a count that
/// disagreed across the two would turn a passing recording into a
/// failing replay with nothing to point at.
pub fn count_matching<D: AppDriver + ?Sized>(
    driver: &mut D,
    selector: &UiaSelector,
    cap: usize,
) -> Result<usize, DriverError> {
    let mut found = 0;
    for n in 1..=cap {
        let mut probe = selector.clone();
        probe.nth = Some(n as u32);
        if !driver.element_exists(&probe)? {
            break;
        }
        found = n;
    }
    Ok(found)
}

/// When a count assertion FAILS, how far to keep counting so the error can
/// say what was actually there. Only paid on the failure path; a passing
/// assertion asks `expected + 1` questions and stops.
pub const COUNT_DIAGNOSTIC_CAP: usize = 50;

pub fn resolve_app(app_id: &str) -> Option<AppTarget> {
    match app_id {
        "calc" => Some(AppTarget {
            command: "calc.exe".into(),
            window_name: "Calculator".into(),
        }),
        "notepad" => Some(AppTarget {
            command: "notepad.exe".into(),
            window_name: "Notepad".into(),
        }),
        _ => None,
    }
}

// Allow callers to select a driver implementation at runtime.
impl AppDriver for Box<dyn AppDriver> {
    fn cell_hints(&mut self, selector: &UiaSelector) -> Result<Option<CellHints>, DriverError> {
        (**self).cell_hints(selector)
    }

    // Every trait method needs its delegation here: a missing one silently
    // falls back to the DEFAULT body, which no mock-driver test can catch.
    fn scope_hints(&mut self, selector: &UiaSelector) -> Result<Option<ScopeHints>, DriverError> {
        (**self).scope_hints(selector)
    }

    fn launch(
        &mut self,
        command: &str,
        window_name: &str,
        timeout: Duration,
    ) -> Result<(), DriverError> {
        (**self).launch(command, window_name, timeout)
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        (**self).element_exists(selector)
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        (**self).invoke(selector)
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        (**self).read_text(selector)
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        (**self).type_text(selector, text)
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        (**self).clear_text(selector)
    }

    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        (**self).type_focused(text)
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        (**self).press_key(key, modifiers)
    }

    fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        (**self).element_enabled(selector)
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        (**self).surface_text()
    }

    fn set_window_geometry(
        &mut self,
        width: u32,
        height: u32,
        position: Option<(i32, i32)>,
    ) -> Result<(u32, u32, i32, i32), DriverError> {
        (**self).set_window_geometry(width, height, position)
    }

    fn element_checked(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        (**self).element_checked(selector)
    }

    fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError> {
        (**self).set_checked(selector, checked)
    }

    // Must forward explicitly, like `debug_bundle` below: a boxed driver
    // otherwise hits the trait DEFAULT and reports the web adapter's DOM
    // capabilities as unsupported.
    fn element_attribute(
        &mut self,
        selector: &UiaSelector,
        name: &str,
    ) -> Result<Option<String>, DriverError> {
        (**self).element_attribute(selector, name)
    }

    fn element_computed_style(
        &mut self,
        selector: &UiaSelector,
        prop: &str,
    ) -> Result<String, DriverError> {
        (**self).element_computed_style(selector, prop)
    }

    fn scroll(&mut self, selector: Option<&UiaSelector>, to: ScrollTo) -> Result<(), DriverError> {
        (**self).scroll(selector, to)
    }

    fn current_url(&mut self) -> Result<String, DriverError> {
        (**self).current_url()
    }

    fn stage_session(&mut self, session: WebSession) -> Result<(), DriverError> {
        (**self).stage_session(session)
    }

    fn navigate(&mut self, url: &str) -> Result<(), DriverError> {
        (**self).navigate(url)
    }

    fn reload(&mut self) -> Result<(), DriverError> {
        (**self).reload()
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        (**self).screen_size()
    }

    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        (**self).capture()
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        (**self).element_rect(selector)
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        (**self).password_rects()
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        (**self).scene()
    }

    fn debug_bundle(&mut self) -> Result<Option<DebugBundle>, DriverError> {
        // Must forward explicitly: a boxed driver otherwise hits the trait
        // DEFAULT (None) and silently drops the web driver's capture.
        (**self).debug_bundle()
    }

    fn element_receives_events(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<bool>, DriverError> {
        (**self).element_receives_events(selector)
    }

    fn stage_mocks(&mut self, rules: Vec<WebMock>) -> Result<(), DriverError> {
        (**self).stage_mocks(rules)
    }

    fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError> {
        (**self).set_files(selector, paths)
    }

    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        (**self).context_click(selector)
    }

    fn stage_browser(&mut self, config: WebBrowserConfig) -> Result<(), DriverError> {
        (**self).stage_browser(config)
    }
}

#[cfg(windows)]
pub use windows_impl::UiaAppDriver;

#[cfg(not(windows))]
pub use stub_impl::UiaAppDriver;

#[cfg(windows)]
mod windows_impl {
    use std::time::{Duration, Instant};

    use uiautomation::patterns::{UIInvokePattern, UIValuePattern};
    use uiautomation::types::ControlType;
    use uiautomation::{UIAutomation, UIElement};

    use super::{AppDriver, UiaSelector};
    use crate::DriverError;

    fn uia_err(context: &str, err: uiautomation::Error) -> DriverError {
        DriverError::Uia(format!("{context}: {err}"))
    }

    /// Windows UIA implementation of [`AppDriver`].
    pub struct UiaAppDriver {
        automation: UIAutomation,
        window: Option<UIElement>,
    }

    impl UiaAppDriver {
        pub fn new() -> Result<Self, DriverError> {
            let automation =
                UIAutomation::new().map_err(|e| uia_err("initializing UIA COM client", e))?;
            Ok(Self {
                automation,
                window: None,
            })
        }

        fn window(&self) -> Result<&UIElement, DriverError> {
            self.window
                .as_ref()
                .ok_or_else(|| DriverError::Uia("no target window: call launch first".into()))
        }

        fn find(&self, selector: &UiaSelector, timeout_ms: u64) -> Result<UIElement, DriverError> {
            if selector.automation_id.is_none()
                && selector.name.is_none()
                && selector.control_type.is_none()
            {
                return Err(DriverError::Uia(format!(
                    "selector [{selector}] has no UIA-matchable fields"
                )));
            }
            let window = self.window()?;
            let mut matcher = self
                .automation
                .create_matcher()
                .from_ref(window)
                .depth(16)
                .timeout(timeout_ms);
            if let Some(id) = selector.automation_id.clone() {
                matcher = matcher.filter_fn(Box::new(move |e: &UIElement| {
                    Ok(e.get_automation_id()? == id)
                }));
            }
            if let Some(name) = &selector.name {
                matcher = matcher.name(name.clone());
            }
            if let Some(control_type) = &selector.control_type {
                let ct = parse_control_type(control_type)?;
                matcher = matcher.control_type(ct);
            }
            matcher
                .find_first()
                .map_err(|e| uia_err(&format!("finding element [{selector}]"), e))
        }
    }

    fn parse_control_type(name: &str) -> Result<ControlType, DriverError> {
        match name {
            "Button" => Ok(ControlType::Button),
            "Edit" => Ok(ControlType::Edit),
            "Text" => Ok(ControlType::Text),
            "Window" => Ok(ControlType::Window),
            other => Err(DriverError::Uia(format!(
                "unsupported control_type '{other}' in selector"
            ))),
        }
    }

    impl UiaAppDriver {
        /// The driven window, by the title `launch` matched on.
        fn driven_window(&self) -> Result<crate::window::WindowInfo, DriverError> {
            let title = self
                .window
                .as_ref()
                .and_then(|w| w.get_name().ok())
                .ok_or_else(|| {
                    DriverError::Uia("no window is attached: call launch first".into())
                })?;
            crate::window::find_window(&title)?
                .ok_or_else(|| DriverError::Uia(format!("window '{title}' vanished")))
        }
    }

    impl AppDriver for UiaAppDriver {
        fn set_window_geometry(
            &mut self,
            width: u32,
            height: u32,
            position: Option<(i32, i32)>,
        ) -> Result<(u32, u32, i32, i32), DriverError> {
            self.driven_window()?.set_geometry(width, height, position)
        }

        fn launch(
            &mut self,
            command: &str,
            window_name: &str,
            timeout: Duration,
        ) -> Result<(), DriverError> {
            // Attach if the window already exists; otherwise spawn and wait.
            let deadline = Instant::now() + timeout;
            let existing = self
                .automation
                .create_matcher()
                .depth(2)
                .timeout(0)
                .contains_name(window_name)
                .find_first();
            if existing.is_err() {
                // `command` is a command LINE: split off the program, then
                // hand the remainder over verbatim so the app sees exactly
                // the arguments the spec wrote, quoting and all.
                let (program, args) = crate::app::split_command_line(command)
                    .ok_or_else(|| DriverError::Uia("app.command is empty".into()))?;
                let mut spawn = std::process::Command::new(&program);
                if !args.is_empty() {
                    use std::os::windows::process::CommandExt;
                    spawn.raw_arg(&args);
                }
                spawn
                    .spawn()
                    .map_err(|e| DriverError::Uia(format!("launching '{command}': {e}")))?;
            }
            loop {
                let found = self
                    .automation
                    .create_matcher()
                    .depth(2)
                    .timeout(1000)
                    .contains_name(window_name)
                    .find_first();
                match found {
                    Ok(window) => {
                        let _ = window.try_focus();
                        self.window = Some(window);
                        return Ok(());
                    }
                    Err(e) if Instant::now() >= deadline => {
                        return Err(uia_err(
                            &format!("waiting for window containing '{window_name}'"),
                            e,
                        ));
                    }
                    Err(_) => {}
                }
            }
        }

        fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
            match self.find(selector, 0) {
                Ok(_) => Ok(true),
                Err(_) => Ok(false),
            }
        }

        fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
            let element = self.find(selector, 3000)?;
            element
                .is_enabled()
                .map_err(|e| uia_err(&format!("reading enabled state of [{selector}]"), e))
        }

        fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
            let element = self.find(selector, 3000)?;
            match element.get_pattern::<UIInvokePattern>() {
                Ok(pattern) => pattern
                    .invoke()
                    .map_err(|e| uia_err(&format!("invoking [{selector}]"), e)),
                // Fall back to a real click for elements without Invoke.
                Err(_) => element
                    .click()
                    .map_err(|e| uia_err(&format!("clicking [{selector}]"), e)),
            }
        }

        fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
            let element = self.find(selector, 3000)?;
            if let Ok(value) = element.get_pattern::<UIValuePattern>() {
                if let Ok(text) = value.get_value() {
                    if !text.is_empty() {
                        return Ok(text);
                    }
                }
            }
            element
                .get_name()
                .map_err(|e| uia_err(&format!("reading text of [{selector}]"), e))
        }

        fn surface_text(&mut self) -> Result<String, DriverError> {
            // The desktop reading of "the surface": every name and value in
            // the foreground window's subtree, top-down. The vision adapter
            // will answer the same question with full-frame OCR.
            let window = self.window()?;
            let elements = self
                .automation
                .create_matcher()
                .from_ref(window)
                .depth(16)
                .timeout(0)
                .filter_fn(Box::new(|_: &UIElement| Ok(true)))
                .find_all()
                .unwrap_or_default();
            let mut parts: Vec<String> = Vec::new();
            for element in elements {
                if let Ok(name) = element.get_name() {
                    if !name.is_empty() {
                        parts.push(name);
                    }
                }
                if let Ok(value) = element.get_pattern::<UIValuePattern>() {
                    if let Ok(text) = value.get_value() {
                        if !text.is_empty() {
                            parts.push(text);
                        }
                    }
                }
            }
            Ok(parts.join("\n"))
        }

        fn scene(&mut self) -> Result<Option<String>, DriverError> {
            // The desktop grounding set for LLM authoring: the same window
            // subtree walk as surface_text, filtered to control types a
            // model can act on. Each entry carries the TARGET TOKEN the
            // model must echo — the stable automation id when the element
            // has one, its accessible name otherwise. Elements with
            // neither cannot be addressed and are skipped.
            const INTERACTABLE: &[ControlType] = &[
                ControlType::Button,
                ControlType::Edit,
                ControlType::ComboBox,
                ControlType::CheckBox,
                ControlType::RadioButton,
                ControlType::ListItem,
                ControlType::TabItem,
                ControlType::MenuItem,
                ControlType::Hyperlink,
            ];
            let window = self.window()?;
            let elements = self
                .automation
                .create_matcher()
                .from_ref(window)
                .depth(16)
                .timeout(0)
                .filter_fn(Box::new(|e: &UIElement| {
                    Ok(INTERACTABLE.contains(&e.get_control_type()?))
                }))
                .find_all()
                .unwrap_or_default();
            let mut entries: Vec<serde_json::Value> = Vec::new();
            for element in elements {
                // Same cap as the web scene: enough for any real window,
                // bounded for the model's context.
                if entries.len() >= 100 {
                    break;
                }
                let id = element.get_automation_id().unwrap_or_default();
                let name = element.get_name().unwrap_or_default();
                let target = if !id.is_empty() {
                    format!("id:{id}")
                } else if !name.is_empty() {
                    format!("text:{name}")
                } else {
                    continue;
                };
                let mut entry = serde_json::json!({ "target": target });
                if let Ok(control_type) = element.get_control_type() {
                    entry["control_type"] = format!("{control_type:?}").into();
                }
                if !name.is_empty() {
                    entry["text"] = name.into();
                }
                if let Ok(value) = element.get_pattern::<UIValuePattern>() {
                    if let Ok(text) = value.get_value() {
                        if !text.is_empty() {
                            entry["value"] = text.into();
                        }
                    }
                }
                entries.push(entry);
            }
            serde_json::to_string(&entries)
                .map(Some)
                .map_err(|e| DriverError::Uia(format!("serializing scene: {e}")))
        }

        /// Type into whatever currently holds focus, addressing nothing.
        /// A freshly launched app puts focus on its primary input (Notepad
        /// lands in the editor), so this is what an untargeted `Type ...`
        /// means for an app whose control tree the spec has never named.
        /// It is also the only honest reading: with no selector there is no
        /// element to gate on, so the app decides where the text lands.
        fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
            let element = self
                .automation
                .get_focused_element()
                .map_err(|e| uia_err("getting the focused element", e))?;
            // Same 10ms keystroke interval as the targeted path: slow Win32
            // message pumps drop text sent faster than they can consume it.
            element
                .send_text(text, 10)
                .map_err(|e| uia_err("typing into the focused element", e))
        }

        /// A key chord to whatever holds focus, via SendInput — UIA has no
        /// "press key on element" primitive, and the focused element is
        /// what a real keyboard hits. Deliberately does NOT re-focus the
        /// window first: `Press Escape` right after `Click "Edit"` must
        /// reach the open menu, and stealing focus back would close it.
        fn press_key(&mut self, key: &str, modifiers: &[crate::KeyMod]) -> Result<(), DriverError> {
            let vk = crate::virtual_key(key)
                .ok_or_else(|| DriverError::Uia(format!("no virtual key for '{key}'")))?;
            let mut backend = crate::PlatformBackend::new();
            let mut inject = |event| crate::Input::inject(&mut backend, &event);
            for m in modifiers {
                inject(crate::InputEvent::KeyDown {
                    virtual_key: crate::modifier_virtual_key(m),
                })?;
            }
            inject(crate::InputEvent::KeyDown { virtual_key: vk })?;
            inject(crate::InputEvent::KeyUp { virtual_key: vk })?;
            for m in modifiers.iter().rev() {
                inject(crate::InputEvent::KeyUp {
                    virtual_key: crate::modifier_virtual_key(m),
                })?;
            }
            // Give the app's message pump a beat to consume the chord
            // before the next action observes its effect — the same
            // settling interval the vision driver uses.
            std::thread::sleep(std::time::Duration::from_millis(120));
            Ok(())
        }

        fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
            let element = self.find(selector, 3000)?;
            element
                .set_focus()
                .map_err(|e| uia_err(&format!("focusing [{selector}]"), e))?;
            // 10ms between keystrokes keeps slow Win32 message pumps reliable.
            element
                .send_text(text, 10)
                .map_err(|e| uia_err(&format!("typing into [{selector}]"), e))
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            let root = self
                .automation
                .get_root_element()
                .map_err(|e| uia_err("getting desktop root element", e))?;
            let rect = root
                .get_bounding_rectangle()
                .map_err(|e| uia_err("reading desktop bounds", e))?;
            Ok((
                rect.get_width().unsigned_abs(),
                rect.get_height().unsigned_abs(),
            ))
        }

        fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
            crate::gdi::capture_screen().map(Some)
        }

        fn element_rect(
            &mut self,
            selector: &UiaSelector,
        ) -> Result<Option<crate::app::PixelRect>, DriverError> {
            match self.find(selector, 0) {
                Ok(element) => {
                    let rect = element
                        .get_bounding_rectangle()
                        .map_err(|e| uia_err(&format!("bounds of [{selector}]"), e))?;
                    Ok(Some((
                        rect.get_left(),
                        rect.get_top(),
                        rect.get_width().unsigned_abs(),
                        rect.get_height().unsigned_abs(),
                    )))
                }
                Err(_) => Ok(None),
            }
        }

        fn password_rects(&mut self) -> Result<Vec<crate::app::PixelRect>, DriverError> {
            let Some(window) = self.window.as_ref() else {
                return Ok(Vec::new());
            };
            let fields = self
                .automation
                .create_matcher()
                .from_ref(window)
                .depth(16)
                .timeout(0)
                .filter_fn(Box::new(|e: &UIElement| e.is_password()))
                .find_all()
                .unwrap_or_default();
            let mut rects = Vec::new();
            for field in fields {
                let rect = field
                    .get_bounding_rectangle()
                    .map_err(|e| uia_err("bounds of password field", e))?;
                rects.push((
                    rect.get_left(),
                    rect.get_top(),
                    rect.get_width().unsigned_abs(),
                    rect.get_height().unsigned_abs(),
                ));
            }
            Ok(rects)
        }
    }
}

#[cfg(not(windows))]
mod stub_impl {
    use std::time::Duration;

    use super::{AppDriver, UiaSelector};
    use crate::DriverError;

    /// Non-Windows stub: constructing it works (so cross-platform code paths
    /// are testable), every operation reports the platform gap.
    #[derive(Debug, Default)]
    pub struct UiaAppDriver {
        _private: (),
    }

    impl UiaAppDriver {
        pub fn new() -> Result<Self, DriverError> {
            Ok(Self::default())
        }
    }

    impl AppDriver for UiaAppDriver {
        fn launch(
            &mut self,
            _command: &str,
            _window_name: &str,
            _timeout: Duration,
        ) -> Result<(), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn element_exists(&mut self, _selector: &UiaSelector) -> Result<bool, DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn invoke(&mut self, _selector: &UiaSelector) -> Result<(), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn read_text(&mut self, _selector: &UiaSelector) -> Result<String, DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn type_text(&mut self, _selector: &UiaSelector, _text: &str) -> Result<(), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn type_focused(&mut self, _text: &str) -> Result<(), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_compare_canonically() {
        // Every spelling of pure red parses to the same RGBA as the computed
        // `rgb(255, 0, 0)`, so a spec may write any of them.
        let red = parse_css_color("rgb(255, 0, 0)").expect("computed red");
        for spelling in [
            "red",
            "#f00",
            "#ff0000",
            "#FF0000",
            "rgb(255,0,0)",
            "rgba(255, 0, 0, 1)",
        ] {
            assert_eq!(parse_css_color(spelling), Some(red), "'{spelling}' == red");
        }
        // Different colors are not equal.
        assert_ne!(parse_css_color("green"), parse_css_color("lime"));
        assert_eq!(parse_css_color("green"), Some([0, 128, 0, 255]));
        // Alpha participates: translucent red is not opaque red.
        assert_ne!(parse_css_color("rgba(255,0,0,0.5)"), Some(red));
        // Non-colors do not parse.
        assert_eq!(parse_css_color("not-a-color"), None);
        assert_eq!(parse_css_color("100px"), None);
    }

    #[test]
    fn style_matches_colors_and_keywords() {
        // Canonical color equality, both polarities.
        assert_eq!(
            style_matches("color", "red", false, "rgb(255, 0, 0)"),
            Ok(true)
        );
        assert_eq!(
            style_matches("color", "green", true, "rgb(255, 0, 0)"),
            Ok(true)
        );
        assert_eq!(
            style_matches("color", "green", false, "rgb(255, 0, 0)"),
            Ok(false)
        );
        // text-transform is a case-insensitive keyword compare.
        assert_eq!(
            style_matches("text-transform", "UPPERCASE", false, "uppercase"),
            Ok(true)
        );
        // An unparseable computed color fails naming what was seen, whatever
        // the polarity.
        assert!(style_matches("color", "red", false, "chartreuseish").is_err());
        assert!(style_matches("color", "red", true, "chartreuseish").is_err());
    }

    #[test]
    fn attribute_value_matcher_is_exact_and_case_sensitive() {
        // Exact, case-sensitive: no text ladder.
        assert!(attribute_value_matches("/new", false, Some("/new")));
        assert!(!attribute_value_matches("/new", false, Some("/NEW")));
        // A missing attribute never equals a value...
        assert!(!attribute_value_matches("/new", false, None));
        // ...so the negative form passes when absent OR different.
        assert!(attribute_value_matches("/new", true, None));
        assert!(attribute_value_matches("/new", true, Some("/old")));
        assert!(!attribute_value_matches("/new", true, Some("/new")));
    }

    #[test]
    fn web_mock_body_conventions() {
        // String body: served verbatim as text/plain.
        let m = WebMock::from_rule_parts(
            "/api/x",
            None,
            200,
            None,
            Some(&serde_json::Value::String("hello".into())),
        );
        assert_eq!(m.content_type, "text/plain");
        assert_eq!(m.body, b"hello");
        // Structured body: serialized as application/json.
        let m = WebMock::from_rule_parts(
            "/api/x",
            Some("post"),
            503,
            None,
            Some(&serde_json::json!({"ok": false})),
        );
        assert_eq!(m.content_type, "application/json");
        assert_eq!(m.body, br#"{"ok":false}"#);
        assert_eq!(m.method.as_deref(), Some("POST"), "method uppercased");
        assert_eq!(m.status, 503);
        // Explicit content type wins; no body = empty.
        let m = WebMock::from_rule_parts("/x", None, 204, Some("text/event-stream"), None);
        assert_eq!(m.content_type, "text/event-stream");
        assert!(m.body.is_empty());
    }

    #[test]
    fn non_web_drivers_reject_mocks_loudly() {
        let mut driver = NoOpDriver::new();
        assert!(driver.stage_mocks(Vec::new()).is_ok(), "empty is fine");
        let err = driver
            .stage_mocks(vec![WebMock::from_rule_parts("/x", None, 200, None, None)])
            .expect_err("silently ignoring a mock would change the test");
        assert!(err.to_string().contains("not supported"));
    }

    #[test]
    fn selector_display_lists_set_fields() {
        let sel = UiaSelector {
            automation_id: Some("num5Button".into()),
            name: Some("Five".into()),
            ..UiaSelector::default()
        };
        assert_eq!(sel.to_string(), "automation_id=num5Button,name=Five");
        assert!(!sel.is_empty());
        assert!(UiaSelector::default().is_empty());
        assert_eq!(sel.css_selector().as_deref(), Some("#num5Button"));
        assert_eq!(
            UiaSelector::css("#name").css_selector().as_deref(),
            Some("#name")
        );
    }

    #[test]
    fn numeric_value_parses_display_text() {
        assert_eq!(numeric_value("Display is 8"), Some(8.0));
        assert_eq!(numeric_value("Display is 1,234.5"), Some(1234.5));
        assert_eq!(numeric_value("8"), Some(8.0));
        assert_eq!(numeric_value("Display is"), None);
    }

    #[cfg(not(windows))]
    #[test]
    fn stub_driver_reports_unsupported() {
        let mut driver = UiaAppDriver::new().expect("stub constructs");
        let err = driver
            .launch("calc.exe", "Calculator", Duration::from_secs(1))
            .expect_err("stub cannot launch");
        assert!(matches!(err, DriverError::UnsupportedPlatform));
    }
}

#[cfg(test)]
mod command_line_tests {
    use super::split_command_line;

    #[test]
    fn a_bare_program_has_no_arguments() {
        assert_eq!(
            split_command_line("notepad.exe"),
            Some(("notepad.exe".into(), String::new()))
        );
    }

    #[test]
    fn arguments_are_kept_verbatim() {
        assert_eq!(
            split_command_line("app.exe --flag --name=value"),
            Some(("app.exe".into(), "--flag --name=value".into()))
        );
    }

    /// The case that makes a naive split wrong: Windows paths have spaces,
    /// so the program itself is quoted.
    #[test]
    fn a_quoted_program_path_survives_its_spaces() {
        assert_eq!(
            split_command_line("\"C:\\Program Files\\My App\\app.exe\" --flag"),
            Some(("C:\\Program Files\\My App\\app.exe".into(), "--flag".into()))
        );
        assert_eq!(
            split_command_line("\"C:\\Program Files\\app.exe\""),
            Some(("C:\\Program Files\\app.exe".into(), String::new()))
        );
    }

    #[test]
    fn empty_and_malformed_are_rejected_not_guessed() {
        assert_eq!(split_command_line(""), None);
        assert_eq!(split_command_line("   "), None);
        // An unterminated quote is a typo, not a program name.
        assert_eq!(split_command_line("\"C:\\app.exe --flag"), None);
        assert_eq!(split_command_line("\"\" --flag"), None);
    }

    #[test]
    fn non_ascii_paths_are_handled() {
        assert_eq!(
            split_command_line("\"C:\\Programme\\Büro\\app.exe\" --öffnen"),
            Some(("C:\\Programme\\Büro\\app.exe".into(), "--öffnen".into()))
        );
    }
}

#[cfg(test)]
mod numeric_value_tests {
    use super::numeric_value;

    #[test]
    fn plain_numbers_are_unchanged() {
        assert_eq!(numeric_value("8"), Some(8.0));
        assert_eq!(numeric_value("Display is 8"), Some(8.0));
        assert_eq!(numeric_value("1,234"), Some(1234.0));
        assert_eq!(numeric_value("-5.5"), Some(-5.5));
        assert_eq!(numeric_value("no digits here"), None);
    }

    /// Money is the motivating case for computed assertions, and a currency
    /// symbol used to defeat parsing entirely.
    #[test]
    fn currency_formatting_is_ignored() {
        assert_eq!(numeric_value("$1,000.00"), Some(1000.0));
        assert_eq!(numeric_value("-$50.00"), Some(-50.0));
        assert_eq!(numeric_value("Account Balance $930.26"), Some(930.26));
        assert_eq!(numeric_value("€1.234"), Some(1.234));
        assert_eq!(numeric_value("£12"), Some(12.0));
    }
}

#[cfg(test)]
mod url_matches_tests {
    use super::url_matches;

    /// `page url is /path` is path-shaped on purpose: it maps
    /// `cy.location("pathname").should("equal", "/signin")` one to one.
    #[test]
    fn a_path_expectation_compares_the_pathname_only() {
        assert!(url_matches("/signin", true, "http://localhost:3000/signin"));
        assert!(url_matches("/signin", true, "https://app.test/signin#top"));
        // The query is ignored unless the expectation asks for one.
        assert!(url_matches(
            "/orders",
            true,
            "https://app.test/orders?page=2"
        ));
        assert!(!url_matches("/signin", true, "https://app.test/signup"));
        // A path expectation must not match a mere prefix.
        assert!(!url_matches("/order", true, "https://app.test/orders"));
    }

    #[test]
    fn a_query_or_fragment_in_the_expectation_makes_it_significant() {
        assert!(url_matches(
            "/orders?page=2",
            true,
            "https://app.test/orders?page=2"
        ));
        assert!(!url_matches(
            "/orders?page=2",
            true,
            "https://app.test/orders?page=3"
        ));
        assert!(!url_matches(
            "/orders?page=2",
            true,
            "https://app.test/orders"
        ));
        assert!(url_matches(
            "/docs#install",
            true,
            "https://app.test/docs#install"
        ));
        assert!(!url_matches(
            "/docs#install",
            true,
            "https://app.test/docs#usage"
        ));
    }

    #[test]
    fn a_full_url_expectation_compares_the_whole_url() {
        assert!(url_matches(
            "https://app.test/signin",
            true,
            "https://app.test/signin"
        ));
        // Different scheme or host is a different URL, even at the same path.
        assert!(!url_matches(
            "https://app.test/signin",
            true,
            "http://app.test/signin"
        ));
    }

    #[test]
    fn contains_is_a_plain_substring_of_the_whole_url() {
        assert!(url_matches(
            "checkout",
            false,
            "https://app.test/cart/checkout?step=1"
        ));
        assert!(url_matches("app.test", false, "https://app.test/cart"));
        assert!(!url_matches("checkout", false, "https://app.test/cart"));
    }

    /// Edge shapes that must not panic or silently mis-compare: a bare
    /// origin has the root path, and non-ASCII paths are byte-safe.
    #[test]
    fn origins_and_multibyte_paths_are_handled() {
        assert!(url_matches("/", true, "https://app.test"));
        assert!(url_matches("/", true, "https://app.test/"));
        assert!(url_matches(
            "/konto/überweisung",
            true,
            "https://app.test/konto/überweisung"
        ));
        assert!(url_matches(
            "überweisung",
            false,
            "https://app.test/konto/überweisung"
        ));
        assert!(!url_matches(
            "/konto/überweisung",
            true,
            "https://app.test/konto/uberweisung"
        ));
    }
}
