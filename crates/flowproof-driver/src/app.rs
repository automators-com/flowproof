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

/// Drives a single application window through UIA.
pub trait AppDriver {
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
        let _ = config;
        Err(DriverError::Uia(
            "browser emulation is not supported by this driver (web flows only)".into(),
        ))
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

    fn element_checked(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        (**self).element_checked(selector)
    }

    fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError> {
        (**self).set_checked(selector, checked)
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

    impl AppDriver for UiaAppDriver {
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
                std::process::Command::new(command)
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

        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            Err(DriverError::UnsupportedPlatform)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
