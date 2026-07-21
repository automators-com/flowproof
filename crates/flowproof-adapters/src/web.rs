//! Browser adapter: drives a page in headless Chromium over the DevTools
//! protocol, implementing the same [`AppDriver`] surface the UIA driver
//! exposes — so the recorder and replayer work unchanged.
//!
//! Selector mapping: `css` payload key, else `#<automation_id>`. `launch`
//! interprets `command` as the URL to open. The Chromium binary is found via
//! the `CHROME` env var or platform auto-detection.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, KeyMod, PixelRect, UiaSelector, WebSession};
use headless_chrome::browser::tab::{ModifierKey, Tab};
use headless_chrome::protocol::cdp::Target::CreateTarget;
use headless_chrome::protocol::cdp::{Emulation, Input, Network, Page};
use headless_chrome::{Browser, LaunchOptions};

use crate::AdapterError;

const FIND_TIMEOUT: Duration = Duration::from_secs(5);

/// Wrap a browser-driver failure. Transport faults (a dead CDP websocket,
/// a dropped event) are classified apart from app observations: an
/// assertion polling inside its recorded wait budget tolerates the former
/// as a miss, because a call that never reached the page learned nothing
/// about it.
fn web_err(context: &str, err: impl std::fmt::Display) -> DriverError {
    let message = format!("{context}: {err}");
    if is_transport_fault(&message) {
        DriverError::Transport(message)
    } else {
        DriverError::Browser(message)
    }
}

/// How long the CDP transport may sit without a BROWSER-level event before
/// headless_chrome reaps its listener thread. Its default is 30 seconds,
/// which is a live grenade for real test flows:
///
/// 1. a flow spends 30+ seconds doing page-level work (typing, polling an
///    auto-waiting assertion) without producing a single browser-level
///    event, so the listener thread times out and exits;
/// 2. the next navigation fires `TargetInfoChanged` - a browser-level
///    event - and the transport cannot deliver it to the receiver that
///    just went away;
/// 3. it treats that undeliverable event as fatal, shuts the whole message
///    loop down, and every later call fails with "Unable to make method
///    calls because underlying connection is closed", permanently.
///
/// That is the entire mechanism behind the round-3 field blocker: EVERY
/// flow that logged in recorded fine and then failed to replay, because
/// the login redirect is exactly a post-idle navigation. Silence is not
/// evidence of a dead browser - a browser that actually dies closes the
/// socket, which surfaces immediately and through a different path - so
/// this reaper only ever fires on healthy long-running flows. Set it well
/// past any plausible run.
const BROWSER_IDLE_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Launch a fresh headless Chromium (`CHROME` env var overrides the
/// binary), optionally with extra command-line flags.
fn launch_browser(extra_args: &[String]) -> Result<Browser, AdapterError> {
    let os_args: Vec<std::ffi::OsString> = extra_args.iter().map(Into::into).collect();
    let mut options = LaunchOptions::default_builder();
    options.headless(true).sandbox(false);
    options.idle_browser_timeout(BROWSER_IDLE_TIMEOUT);
    options.args(os_args.iter().map(AsRef::as_ref).collect());
    if let Ok(path) = std::env::var("CHROME") {
        options.path(Some(path.into()));
    }
    let options = options
        .build()
        .map_err(|e| AdapterError::Web(format!("building launch options: {e}")))?;
    Browser::new(options).map_err(|e| AdapterError::Web(format!("launching browser: {e}")))
}

/// One Chromium process for the whole run, reused across flows. Each flow
/// gets an isolated incognito CONTEXT (its own cookies/cache), so reuse is
/// invisible to specs but the ~seconds-long cold start is paid ONCE per
/// suite instead of once per flow. `Browser` is a cloneable Arc handle;
/// holding one in the static keeps the process alive until the test binary
/// exits. Opt out with `FLOWPROOF_NO_SHARED_BROWSER=1`.
fn shared_browser() -> Result<Browser, AdapterError> {
    // Hold a keep-alive blank tab forever: headless Chrome exits when its
    // LAST target closes, so as flows open and close their own tabs this
    // one keeps the process — and its warm connection — alive (Playwright
    // keeps the browser independent of pages the same way).
    type SharedCell = Mutex<Option<(Browser, Arc<Tab>)>>;
    static SHARED: OnceLock<SharedCell> = OnceLock::new();
    let cell = SHARED.get_or_init(|| Mutex::new(None));
    let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
    // Reuse only while the process is actually alive: a cheap CDP round
    // trip proves the transport. If Chrome exited (or the socket died),
    // relaunch transparently — the caller never sees a dead handle.
    if let Some((browser, _keepalive)) = guard.as_ref() {
        if browser.get_version().is_ok() {
            return Ok(browser.clone());
        }
    }
    let browser = launch_browser(&[])?;
    let keepalive = browser
        .new_tab()
        .map_err(|e| AdapterError::Web(format!("opening keep-alive tab: {e}")))?;
    *guard = Some((browser.clone(), keepalive));
    Ok(browser)
}

/// Browser-backed [`AppDriver`].
pub struct WebAppDriver {
    browser: Browser,
    /// Incognito context isolating this flow on the shared browser; `None`
    /// when the driver owns a private browser (the opt-out path), where a
    /// plain tab is already isolated.
    context_id: Option<String>,
    tab: Option<Arc<Tab>>,
    /// Session staged via [`AppDriver::stage_session`], applied by the next
    /// `launch` before the page loads.
    staged_session: Option<WebSession>,
    /// Recent console/log lines from the page (bounded ring buffer),
    /// filled by a CDP event listener registered at launch — read
    /// retroactively when a step fails ([`AppDriver::debug_bundle`]).
    console: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    /// Network mocks staged via [`AppDriver::stage_mocks`], installed by
    /// the next `launch` before navigation (CDP Fetch interception).
    staged_mocks: Vec<flowproof_driver::WebMock>,
    /// Browser config staged via [`AppDriver::stage_browser`], applied by
    /// the next `launch`: viewport/UA per-tab; extra flags swap in a
    /// private browser (flags only apply at process start).
    staged_browser: Option<flowproof_driver::WebBrowserConfig>,
}

/// Cap on retained console lines — enough context for a failure, bounded
/// so a chatty app can't balloon the run bundle.
const CONSOLE_TAIL_CAP: usize = 100;

/// Standard base64 for CDP `Fetch.fulfillRequest` bodies — hand-rolled
/// (~15 lines) rather than pulling a crate into the adapter for one call.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

impl WebAppDriver {
    /// A driver on the shared browser (isolated context per flow), or a
    /// private browser when `FLOWPROOF_NO_SHARED_BROWSER=1`.
    pub fn new() -> Result<Self, AdapterError> {
        if std::env::var_os("FLOWPROOF_NO_SHARED_BROWSER").is_some() {
            return Ok(Self {
                browser: launch_browser(&[])?,
                context_id: None,
                tab: None,
                staged_session: None,
                console: Default::default(),
                staged_mocks: Vec::new(),
                staged_browser: None,
            });
        }
        let browser = shared_browser()?;
        let context = browser
            .new_context()
            .map_err(|e| AdapterError::Web(format!("creating browser context: {e}")))?;
        let context_id = context.get_id().to_string();
        Ok(Self {
            browser,
            context_id: Some(context_id),
            tab: None,
            staged_session: None,
            console: Default::default(),
            staged_mocks: Vec::new(),
            staged_browser: None,
        })
    }

    /// Apply staged session state to a fresh tab BEFORE navigation: cookies
    /// via CDP, localStorage via an on-new-document script (Playwright's
    /// addInitScript pattern — it runs before any page script on every
    /// navigation, so the app boots already seeded).
    fn apply_session(tab: &Arc<Tab>, session: &WebSession, url: &str) -> Result<(), DriverError> {
        if !session.local_storage.is_empty() {
            let mut source = String::from("try{");
            for (key, value) in &session.local_storage {
                let key = serde_json::to_string(key).unwrap_or_default();
                let value = serde_json::to_string(value).unwrap_or_default();
                source.push_str(&format!("localStorage.setItem({key},{value});"));
            }
            source.push_str("}catch(e){}");
            tab.call_method(Page::AddScriptToEvaluateOnNewDocument {
                source,
                world_name: None,
                include_command_line_api: None,
                run_immediately: None,
            })
            .map_err(|e| web_err("seeding localStorage", e))?;
        }
        if !session.cookies.is_empty() {
            let cookies = session
                .cookies
                .iter()
                .map(|(name, value, domain)| Network::CookieParam {
                    name: name.clone(),
                    value: value.clone(),
                    // Without an explicit domain the cookie binds to the
                    // launch URL's host.
                    url: domain.is_none().then(|| url.to_string()),
                    domain: domain.clone(),
                    path: None,
                    secure: None,
                    http_only: None,
                    same_site: None,
                    expires: None,
                    priority: None,
                    same_party: None,
                    source_scheme: None,
                    source_port: None,
                    partition_key: None,
                })
                .collect();
            tab.set_cookies(cookies)
                .map_err(|e| web_err("setting session cookies", e))?;
        }
        Ok(())
    }

    fn tab(&self) -> Result<&Arc<Tab>, DriverError> {
        self.tab
            .as_ref()
            .ok_or_else(|| DriverError::Browser("no page open: call launch first".into()))
    }

    fn locator_of(selector: &UiaSelector) -> Option<WebLocator> {
        let nth = selector.nth;
        if let Some(css) = selector.css_selector() {
            return Some(WebLocator {
                css: Some(css),
                text: None,
                nth,
            });
        }
        // Text anchor: find by visible text / accessible label / placeholder
        // — how humans (and Playwright's getByText/getByPlaceholder/getByRole
        // name matching) address elements on pages without ids.
        selector.name.as_ref().map(|text| WebLocator {
            css: None,
            text: Some(text.clone()),
            nth,
        })
    }

    fn locator(selector: &UiaSelector) -> Result<WebLocator, DriverError> {
        Self::locator_of(selector).ok_or_else(|| {
            DriverError::Browser(format!(
                "selector [{selector}] has no css, automation_id, or text"
            ))
        })
    }

    /// One resolution attempt, in preference order: css, then exact text
    /// anchor, then prefix text anchor (Playwright's name matching accepts
    /// a leading match when the accessible name carries trailing detail —
    /// catalog cards, chips like `ID: …`).
    fn try_find(
        &self,
        locator: &WebLocator,
    ) -> Result<Option<headless_chrome::Element<'_>>, DriverError> {
        let tab = self.tab()?;
        if let Some(css) = &locator.css {
            return Ok(match locator.nth {
                None => tab.find_element(css).ok(),
                Some(n) => tab
                    .find_elements(css)
                    .ok()
                    .and_then(|found| found.into_iter().nth(n.saturating_sub(1) as usize)),
            });
        }
        if let Some(text) = &locator.text {
            for xpath in text_xpaths(text) {
                let xpath = match locator.nth {
                    Some(n) => format!("({xpath})[{n}]"),
                    None => xpath,
                };
                if let Ok(element) = tab.find_element_by_xpath(&xpath) {
                    return Ok(Some(element));
                }
            }
        }
        Ok(None)
    }

    fn find(&self, locator: &WebLocator) -> Result<headless_chrome::Element<'_>, DriverError> {
        let deadline = std::time::Instant::now() + FIND_TIMEOUT;
        loop {
            if let Some(element) = self.try_find(locator)? {
                return Ok(element);
            }
            if std::time::Instant::now() >= deadline {
                return Err(DriverError::Browser(format!("no element for {locator}")));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn exists(&self, locator: &WebLocator) -> Result<bool, DriverError> {
        Ok(self.try_find(locator)?.is_some())
    }

    /// Run an element operation with ONE retry on a CDP transport fault:
    /// re-resolve the element (its object id may be gone with the dead
    /// connection) and try again before failing the step.
    fn with_element<T>(
        &self,
        locator: &WebLocator,
        context: &str,
        op: impl Fn(&headless_chrome::Element<'_>) -> Result<T, anyhow::Error>,
    ) -> Result<T, DriverError> {
        let mut retried = false;
        loop {
            let element = self.find(locator)?;
            match op(&element) {
                Ok(value) => return Ok(value),
                Err(e) if !retried && is_transport_fault(&e.to_string()) => {
                    retried = true;
                    std::thread::sleep(Duration::from_millis(300));
                }
                Err(e) => return Err(web_err(context, e)),
            }
        }
    }
}

/// Faults of the CDP transport itself (dead websocket, dropped event) —
/// distinct from "element not found": worth one retry with a fresh handle.
fn is_transport_fault(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("connection is closed")
        || m.contains("the event waited for never came")
        || m.contains("unable to make method calls")
}

/// How a [`UiaSelector`] resolves on a page: a CSS selector or a text
/// anchor, optionally narrowed to the nth match (1-based).
struct WebLocator {
    css: Option<String>,
    text: Option<String>,
    nth: Option<u32>,
}

impl std::fmt::Display for WebLocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match (&self.css, &self.text) {
            (Some(css), _) => write!(f, "css '{css}'")?,
            (None, Some(text)) => write!(f, "text '{text}'")?,
            (None, None) => write!(f, "empty locator")?,
        }
        if let Some(n) = self.nth {
            write!(f, " (match #{n})")?;
        }
        Ok(())
    }
}

/// XPaths matching an interactable element by its visible text, accessible
/// label, or placeholder — Playwright's text/placeholder addressing —
/// tried in order: (1) exact match on the element's DIRECT text nodes (its
/// own text, so a sibling avatar's initials can never fuse with a label
/// into "ETE2E Test Runner's Team"); (2) exact match on the concatenated
/// subtree text (covers labels wrapped in spans); (3) `<label>` association
/// by the label's exact text — both the wrapping form
/// `<label>Name: <input/></label>` and `<label for>`/`id` pairing; (4) and
/// (5) the own-text/subtree rungs as prefix matches (a leading match is
/// accepted when the accessible name carries trailing detail — catalog
/// cards, chips like `ID: …`); (6) label association as a prefix match
/// (so `Name` finds the field labelled `Name:`); (7) and (8) ASCII
/// case-insensitive fallbacks of the exact and prefix rungs — role names
/// are case-insensitive in Playwright, and real pages disagree with specs
/// about capitalization ("Close Account" vs "Close account"). A
/// case-sensitive match always wins over a case-insensitive one.
fn text_xpaths(text: &str) -> Vec<String> {
    const UPPER: &str = "'ABCDEFGHIJKLMNOPQRSTUVWXYZ'";
    const LOWER: &str = "'abcdefghijklmnopqrstuvwxyz'";
    let lit = xpath_literal(text);
    let lower_lit = xpath_literal(&text.to_ascii_lowercase());
    let ci = |expr: &str| format!("translate({expr}, {UPPER}, {LOWER})={lower_lit}");
    let ci_prefix =
        |expr: &str| format!("starts-with(translate({expr}, {UPPER}, {LOWER}), {lower_lit})");
    let build = |by_text: String, by_label: String, by_placeholder: String| {
        format!(
            "//*[self::button or self::a or self::summary or @role='button' or \
             @role='tab' or @role='option' or @type='submit']\
             [{by_text} or {by_label}] | \
             //input[{by_placeholder} or {by_label}] | \
             //textarea[{by_placeholder} or {by_label}]"
        )
    };
    // Fields addressed by their <label>: the wrapping form associates by
    // containment, the `for` form by id. XPath 1.0 node-set comparison
    // makes `@id = //label[…]/@for` "any label whose for equals this id".
    let by_label_assoc = |label_text: String| {
        ["input", "textarea", "select"]
            .map(|tag| {
                format!(
                    "//label[{label_text}]//{tag} | \
                     //{tag}[@id = //label[{label_text}]/@for]"
                )
            })
            .join(" | ")
    };
    vec![
        build(
            format!("text()[normalize-space(.)={lit}]"),
            format!("@aria-label={lit}"),
            format!("@placeholder={lit}"),
        ),
        build(
            format!("normalize-space()={lit}"),
            format!("@aria-label={lit}"),
            format!("@placeholder={lit}"),
        ),
        by_label_assoc(format!("normalize-space()={lit}")),
        build(
            format!("text()[starts-with(normalize-space(.), {lit})]"),
            format!("starts-with(@aria-label, {lit})"),
            format!("starts-with(@placeholder, {lit})"),
        ),
        build(
            format!("starts-with(normalize-space(), {lit})"),
            format!("starts-with(@aria-label, {lit})"),
            format!("starts-with(@placeholder, {lit})"),
        ),
        by_label_assoc(format!("starts-with(normalize-space(), {lit})")),
        format!(
            "{} | {}",
            build(
                ci("normalize-space()"),
                ci("@aria-label"),
                ci("@placeholder"),
            ),
            by_label_assoc(ci("normalize-space()")),
        ),
        format!(
            "{} | {}",
            build(
                ci_prefix("normalize-space()"),
                ci_prefix("@aria-label"),
                ci_prefix("@placeholder"),
            ),
            by_label_assoc(ci_prefix("normalize-space()")),
        ),
    ]
}

/// Quote `text` as an XPath string literal, handling embedded quotes.
fn xpath_literal(text: &str) -> String {
    if !text.contains('\'') {
        format!("'{text}'")
    } else if !text.contains('"') {
        format!("\"{text}\"")
    } else {
        let parts: Vec<String> = text.split('\'').map(|p| format!("'{p}'")).collect();
        format!("concat({})", parts.join(", \"'\", "))
    }
}

impl Drop for WebAppDriver {
    fn drop(&mut self) {
        // The shared browser outlives this driver: close the tab so pages
        // don't accumulate across a suite. A private browser (opt-out) is
        // torn down with its own process, so nothing to do there.
        if self.context_id.is_some() {
            if let Some(tab) = self.tab.take() {
                let _ = tab.close(false);
            }
        }
    }
}

impl AppDriver for WebAppDriver {
    /// `command` is the URL to open; `window_name` is unused for web.
    fn launch(
        &mut self,
        command: &str,
        _window_name: &str,
        _timeout: Duration,
    ) -> Result<(), DriverError> {
        let staged_browser = self.staged_browser.take();
        // Extra Chrome flags only apply at process start: swap in a
        // PRIVATE browser for this flow (a plain tab on it is already
        // isolated), paying its cold start instead of sharing.
        if let Some(config) = &staged_browser {
            if !config.args.is_empty() {
                self.browser = launch_browser(&config.args)
                    .map_err(|e| DriverError::Browser(e.to_string()))?;
                self.context_id = None;
            }
        }
        // On the shared browser, open the tab INSIDE this flow's isolated
        // context so its cookies/storage never touch another flow's.
        let tab = match &self.context_id {
            Some(id) => self.browser.new_tab_with_options(CreateTarget {
                url: "about:blank".to_string(),
                browser_context_id: Some(id.clone()),
                left: None,
                top: None,
                width: None,
                height: None,
                window_state: None,
                enable_begin_frame_control: None,
                new_window: None,
                background: None,
                for_tab: None,
                hidden: None,
            }),
            None => self.browser.new_tab(),
        }
        .map_err(|e| web_err("opening tab", e))?;
        // Viewport/UA emulation BEFORE navigation, so the app boots into
        // the emulated device (responsive breakpoints, UA sniffing). NOT
        // best-effort: a flow recorded mobile must never run desktop.
        if let Some(config) = &staged_browser {
            if let Some(vp) = &config.viewport {
                tab.call_method(Emulation::SetDeviceMetricsOverride {
                    width: vp.width,
                    height: vp.height,
                    device_scale_factor: vp.device_scale_factor,
                    mobile: vp.mobile,
                    scale: None,
                    screen_width: None,
                    screen_height: None,
                    position_x: None,
                    position_y: None,
                    dont_set_visible_size: None,
                    screen_orientation: None,
                    viewport: None,
                    display_feature: None,
                    device_posture: None,
                })
                .map_err(|e| web_err("emulating viewport", e))?;
                if vp.touch {
                    tab.call_method(Emulation::SetTouchEmulationEnabled {
                        enabled: true,
                        max_touch_points: Some(1),
                    })
                    .map_err(|e| web_err("emulating touch", e))?;
                }
            }
            if let Some(ua) = &config.user_agent {
                tab.set_user_agent(ua, None, None)
                    .map_err(|e| web_err("overriding user agent", e))?;
            }
        }
        if let Some(session) = self.staged_session.take() {
            Self::apply_session(&tab, &session, command)?;
        }
        // Network mocks: install interception BEFORE navigation so even the
        // first document's subresources are answerable. Unlike the console
        // listener this is NOT best-effort — a mock that silently failed to
        // install would change what the flow tests.
        if !self.staged_mocks.is_empty() {
            let mocks = std::mem::take(&mut self.staged_mocks);
            tab.enable_fetch(None, None)
                .map_err(|e| web_err("enabling network interception", e))?;
            tab.enable_request_interception(Arc::new(
                move |_transport: Arc<headless_chrome::browser::transport::Transport>,
                      _session: headless_chrome::browser::transport::SessionId,
                      event: headless_chrome::protocol::cdp::Fetch::events::RequestPausedEvent| {
                    use headless_chrome::browser::tab::RequestPausedDecision;
                    use headless_chrome::protocol::cdp::Fetch;
                    let url = &event.params.request.url;
                    let method = event.params.request.method.to_ascii_uppercase();
                    // Mocked responses must carry permissive CORS headers:
                    // the page's origin differs from the mocked host, and a
                    // fulfilled response is still subject to CORS — without
                    // them the fetch rejects and the mock looks dead.
                    let cors = |ct: Option<&str>| {
                        let mut headers = vec![
                            Fetch::HeaderEntry {
                                name: "access-control-allow-origin".into(),
                                value: "*".into(),
                            },
                            Fetch::HeaderEntry {
                                name: "access-control-allow-methods".into(),
                                value: "*".into(),
                            },
                            Fetch::HeaderEntry {
                                name: "access-control-allow-headers".into(),
                                value: "*".into(),
                            },
                        ];
                        if let Some(ct) = ct {
                            headers.push(Fetch::HeaderEntry {
                                name: "content-type".into(),
                                value: ct.to_string(),
                            });
                        }
                        headers
                    };
                    let any_match = mocks.iter().any(|m| url.contains(&m.url_contains));
                    // CORS preflight for a mocked URL: answer it ourselves —
                    // the real host may not even exist.
                    if method == "OPTIONS" && any_match {
                        return RequestPausedDecision::Fulfill(Fetch::FulfillRequest {
                            request_id: event.params.request_id.clone(),
                            response_code: 204,
                            response_headers: Some(cors(None)),
                            binary_response_headers: None,
                            body: None,
                            response_phrase: None,
                        });
                    }
                    let rule = mocks.iter().find(|m| {
                        url.contains(&m.url_contains)
                            && m.method.as_ref().is_none_or(|want| *want == method)
                    });
                    match rule {
                        Some(m) => RequestPausedDecision::Fulfill(Fetch::FulfillRequest {
                            request_id: event.params.request_id.clone(),
                            response_code: u32::from(m.status),
                            response_headers: Some(cors(Some(&m.content_type))),
                            binary_response_headers: None,
                            body: Some(base64_encode(&m.body)),
                            response_phrase: None,
                        }),
                        None => RequestPausedDecision::Continue(None),
                    }
                },
            ))
            .map_err(|e| web_err("installing network mocks", e))?;
        }
        // Console tail: subscribe BEFORE navigation so boot-time errors are
        // captured too. Best-effort — a page without console history still
        // yields a DOM snapshot on failure.
        if tab.enable_log().and_then(|t| t.enable_runtime()).is_ok() {
            let buffer = self.console.clone();
            let listener = move |event: &headless_chrome::protocol::cdp::types::Event| {
                use headless_chrome::protocol::cdp::types::Event;
                let line = match event {
                    Event::LogEntryAdded(e) => Some(format!(
                        "[{:?}] {}",
                        e.params.entry.level, e.params.entry.text
                    )),
                    Event::RuntimeExceptionThrown(e) => {
                        Some(format!("[exception] {}", e.params.exception_details.text))
                    }
                    _ => None,
                };
                if let Some(line) = line {
                    let mut buf = buffer.lock().unwrap_or_else(|e| e.into_inner());
                    if buf.len() >= CONSOLE_TAIL_CAP {
                        buf.pop_front();
                    }
                    buf.push_back(line);
                }
            };
            tab.add_event_listener(Arc::new(listener)).ok();
        }
        tab.navigate_to(command)
            .map_err(|e| web_err(&format!("navigating to {command}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        self.tab = Some(tab);
        Ok(())
    }

    fn debug_bundle(&mut self) -> Result<Option<flowproof_driver::DebugBundle>, DriverError> {
        // Best-effort by contract: a half-captured bundle still beats none.
        let dom_html = self
            .tab()
            .ok()
            .and_then(|tab| {
                tab.evaluate("document.documentElement.outerHTML", false)
                    .ok()
            })
            .and_then(|v| v.value)
            .and_then(|v| v.as_str().map(str::to_string));
        let console = self
            .console
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        Ok(Some(flowproof_driver::DebugBundle { dom_html, console }))
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        // Visible text PLUS the accessible names of visible elements:
        // icon-only buttons (a command palette, an account menu) exist on
        // the page only as aria-labels, and `page shows` must see them.
        let value = self
            .tab()?
            .evaluate(
                r#"(() => {
                    const text = document.body ? document.body.innerText : '';
                    const names = [];
                    for (const el of document.querySelectorAll('[aria-label]')) {
                        const r = el.getBoundingClientRect();
                        if (r.width > 0 && r.height > 0) {
                            names.push(el.getAttribute('aria-label'));
                        }
                    }
                    return names.length ? text + '\n' + names.join('\n') : text;
                })()"#,
                false,
            )
            .map_err(|e| web_err("reading page text", e))?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn stage_session(&mut self, session: WebSession) -> Result<(), DriverError> {
        self.staged_session = Some(session);
        Ok(())
    }

    fn stage_mocks(&mut self, rules: Vec<flowproof_driver::WebMock>) -> Result<(), DriverError> {
        self.staged_mocks = rules;
        Ok(())
    }

    fn stage_browser(
        &mut self,
        config: flowproof_driver::WebBrowserConfig,
    ) -> Result<(), DriverError> {
        self.staged_browser = Some(config);
        Ok(())
    }

    fn navigate(&mut self, url: &str) -> Result<(), DriverError> {
        let tab = self.tab()?;
        tab.navigate_to(url)
            .map_err(|e| web_err(&format!("navigating to {url}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        Ok(())
    }

    fn reload(&mut self) -> Result<(), DriverError> {
        let tab = self.tab()?;
        tab.reload(false, None)
            .map_err(|e| web_err("reloading the page", e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for reload", e))?;
        Ok(())
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        let Some(locator) = Self::locator_of(selector) else {
            return Ok(false); // non-web ladder rungs simply don't match
        };
        self.exists(&locator)
    }

    fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        let locator = Self::locator(selector)?;
        let value = self.with_element(
            &locator,
            &format!("reading enabled state of [{selector}]"),
            |element| {
                element.call_js_fn(
                    r#"function() {
                        if (this.disabled === true) { return false; }
                        if (this.getAttribute('aria-disabled') === 'true') { return false; }
                        return !this.closest('fieldset[disabled]');
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(value.value.and_then(|v| v.as_bool()).unwrap_or(true))
    }

    fn element_receives_events(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<bool>, DriverError> {
        let locator = Self::locator(selector)?;
        let value =
            self.with_element(&locator, &format!("hit-testing [{selector}]"), |element| {
                // Playwright's obscured check: does elementFromPoint at the
                // element's center resolve to it (or a relative)? A toast or
                // modal backdrop on top makes the click land elsewhere.
                element.call_js_fn(
                    r#"function() {
                        const r = this.getBoundingClientRect();
                        if (r.width === 0 || r.height === 0) { return false; }
                        const t = document.elementFromPoint(
                            r.x + r.width / 2, r.y + r.height / 2);
                        return !!(t && (t === this || this.contains(t) || t.contains(this)));
                    }"#,
                    vec![],
                    false,
                )
            })?;
        Ok(value.value.and_then(|v| v.as_bool()))
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        self.with_element(&locator, &format!("clicking [{selector}]"), |element| {
            element.click().map(|_| ())
        })
    }

    fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError> {
        // Absolute paths: Chrome resolves DOM.setFileInputFiles against ITS
        // working directory, not ours — canonicalize (which also fails
        // loudly on a missing file, before the step "passes" emptily).
        let mut absolute = Vec::with_capacity(paths.len());
        for path in paths {
            let canonical = std::fs::canonicalize(path)
                .map_err(|e| web_err(&format!("upload file '{path}'"), e))?;
            absolute.push(canonical.to_string_lossy().into_owned());
        }
        let locator = Self::locator(selector)?;
        self.with_element(
            &locator,
            &format!("setting files on [{selector}]"),
            |element| {
                let refs: Vec<&str> = absolute.iter().map(String::as_str).collect();
                element.set_input_files(&refs).map(|_| ())
            },
        )
    }

    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let tab = self.tab()?.clone();
        self.with_element(
            &locator,
            &format!("right-clicking [{selector}]"),
            |element| {
                element.scroll_into_view()?;
                let point = element.get_midpoint()?;
                for kind in [
                    Input::DispatchMouseEventTypeOption::MousePressed,
                    Input::DispatchMouseEventTypeOption::MouseReleased,
                ] {
                    tab.call_method(Input::DispatchMouseEvent {
                        Type: kind,
                        x: point.x,
                        y: point.y,
                        button: Some(Input::MouseButton::Right),
                        click_count: Some(1),
                        modifiers: None,
                        timestamp: None,
                        buttons: None,
                        force: None,
                        tangential_pressure: None,
                        tilt_x: None,
                        tilt_y: None,
                        twist: None,
                        delta_x: None,
                        delta_y: None,
                        pointer_Type: None,
                    })?;
                }
                Ok(())
            },
        )
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        let locator = Self::locator(selector)?;
        // Inner text covers most elements; inputs expose their VALUE — the
        // text a user sees in the box (Playwright's toHaveValue reading).
        let value = self.with_element(
            &locator,
            &format!("reading text of [{selector}]"),
            |element| {
                element.call_js_fn(
                    r#"function() {
                        const tag = this.tagName;
                        if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') {
                            return this.value;
                        }
                        return this.innerText !== undefined ? this.innerText : (this.textContent || '');
                    }"#,
                    vec![],
                    false,
                )
            },
        )?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        // A native <select> cannot be committed by clicks or keystrokes in
        // headless Chromium (and a coordinate click never fires React's
        // onChange). Committing a value IS a property set + events: match
        // an option by value, then visible text, set through the native
        // setter, and fire input+change like a user's selection would.
        let handled =
            self.with_element(&locator, &format!("selecting in [{selector}]"), |element| {
                element.call_js_fn(
                    r#"function(wanted) {
                        if (this.tagName !== 'SELECT') { return false; }
                        const w = String(wanted).trim();
                        const options = Array.from(this.options);
                        const match = options.find(o => o.value === w)
                            || options.find(o => o.textContent.trim() === w)
                            || options.find(o => o.textContent.trim().startsWith(w));
                        if (!match) {
                            throw new Error('no <option> matches "' + w + '"');
                        }
                        const desc = Object.getOwnPropertyDescriptor(
                            HTMLSelectElement.prototype, 'value');
                        if (desc && desc.set) { desc.set.call(this, match.value); }
                        else { this.value = match.value; }
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                        return true;
                    }"#,
                    vec![serde_json::json!(text)],
                    false,
                )
            })?;
        if handled.value.and_then(|v| v.as_bool()) == Some(true) {
            return Ok(());
        }
        self.with_element(&locator, &format!("typing into [{selector}]"), |element| {
            element.click()?.type_into(text).map(|_| ())
        })
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        // Go through the native value setter so framework-controlled inputs
        // (React et al.) see the change, then fire the events they listen to.
        self.with_element(&locator, &format!("clearing [{selector}]"), |element| {
            element
                .call_js_fn(
                    r#"function() {
                        this.focus();
                        if ('value' in this) {
                            const proto = this.tagName === 'TEXTAREA'
                                ? HTMLTextAreaElement.prototype
                                : HTMLInputElement.prototype;
                            const setter = Object.getOwnPropertyDescriptor(proto, 'value');
                            if (setter && setter.set) { setter.set.call(this, ''); }
                            else { this.value = ''; }
                        } else {
                            this.textContent = '';
                        }
                        this.dispatchEvent(new Event('input', { bubbles: true }));
                        this.dispatchEvent(new Event('change', { bubbles: true }));
                    }"#,
                    vec![],
                    false,
                )
                .map(|_| ())
        })
    }

    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        self.tab()?
            .type_str(text)
            .map_err(|e| web_err("typing into the focused element", e))?;
        Ok(())
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        let mods: Vec<ModifierKey> = modifiers
            .iter()
            .map(|m| match m {
                KeyMod::Ctrl => ModifierKey::Ctrl,
                KeyMod::Alt => ModifierKey::Alt,
                KeyMod::Shift => ModifierKey::Shift,
                KeyMod::Meta => ModifierKey::Meta,
            })
            .collect();
        self.tab()?
            .press_key_with_modifiers(key, (!mods.is_empty()).then_some(mods.as_slice()))
            .map_err(|e| web_err(&format!("pressing key '{key}'"), e))?;
        Ok(())
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        // Headless default viewport; good enough for trace metadata.
        Ok((1280, 720))
    }

    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        let png = self
            .tab()?
            .capture_screenshot(Page::CaptureScreenshotFormatOption::Png, None, None, true)
            .map_err(|e| web_err("capturing screenshot", e))?;
        let frame = image::load_from_memory(&png)
            .map_err(|e| web_err("decoding screenshot", e))?
            .to_rgba8();
        Ok(Some(frame))
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        let Some(locator) = Self::locator_of(selector) else {
            return Ok(None);
        };
        let Some(element) = self.try_find(&locator)? else {
            return Ok(None);
        };
        let quad = element
            .get_box_model()
            .map_err(|e| web_err(&format!("box model of [{selector}]"), e))?
            .content;
        Ok(Some((
            quad.most_left().floor() as i32,
            quad.most_top().floor() as i32,
            quad.width().ceil() as u32,
            quad.height().ceil() as u32,
        )))
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        // Enumerate visible interactable elements with stable selectors —
        // the grounding set an authoring model must choose targets from.
        // `target` is the provenance-neutral token the model echoes; the
        // bare `css` key is kept one release for older agents.
        const SCENE_JS: &str = r#"
            JSON.stringify(Array.from(document.querySelectorAll(
                'input, button, a, select, textarea, [role=button], [id]'
            )).filter(el => {
                const r = el.getBoundingClientRect();
                return r.width > 0 && r.height > 0;
            }).slice(0, 100).map((el, i) => {
                const css = el.id ? '#' + el.id
                    : el.tagName.toLowerCase() + ':nth-of-type(' +
                      (Array.from(document.querySelectorAll(el.tagName)).indexOf(el) + 1) + ')';
                const label = el.labels && el.labels[0] ? el.labels[0].textContent.trim()
                    : (el.getAttribute('aria-label') || el.getAttribute('placeholder') || '');
                return {
                    target: 'css:' + css,
                    css,
                    tag: el.tagName.toLowerCase(),
                    type: el.getAttribute('type') || undefined,
                    text: (el.textContent || '').trim().slice(0, 80) || undefined,
                    label: label || undefined,
                };
            }))
        "#;
        let value = self
            .tab()?
            .evaluate(SCENE_JS, false)
            .map_err(|e| web_err("evaluating scene script", e))?;
        let json = value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .ok_or_else(|| DriverError::Browser("scene script returned no value".into()))?;
        Ok(Some(json))
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        let tab = self.tab()?;
        let fields = tab
            .find_elements("input[type=password]")
            .unwrap_or_default();
        let mut rects = Vec::new();
        for field in fields {
            let quad = field
                .get_box_model()
                .map_err(|e| web_err("box model of password field", e))?
                .content;
            rects.push((
                quad.most_left().floor() as i32,
                quad.most_top().floor() as i32,
                quad.width().ceil() as u32,
                quad.height().ceil() as u32,
            ));
        }
        Ok(rects)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn text_xpath_ladder_orders_exact_label_prefix_then_case_insensitive() {
        let rungs = super::text_xpaths("Close Account");
        assert_eq!(rungs.len(), 8);
        // Rung 1: exact own-text — unchanged from the original ladder.
        assert!(rungs[0].contains("text()[normalize-space(.)='Close Account']"));
        // Rung 3: label association — wrapping form and for/id pairing.
        assert!(rungs[2].contains("//label[normalize-space()='Close Account']//input"));
        assert!(rungs[2].contains("//input[@id = //label[normalize-space()='Close Account']/@for]"));
        assert!(rungs[2].contains("//select"));
        // Rung 6: label prefix — `Name` finds the field labelled `Name:`.
        assert!(rungs[5].contains("starts-with(normalize-space(), 'Close Account')"));
        // Rungs 7-8: case-insensitive fallbacks compare lowercased text.
        assert!(rungs[6].contains("translate(normalize-space(), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz')='close account'"));
        assert!(rungs[6].contains("translate(@aria-label"));
        assert!(rungs[7].contains("starts-with(translate(normalize-space()"));
        // No case-sensitive rung mentions translate: exact always wins.
        for rung in &rungs[..6] {
            assert!(
                !rung.contains("translate("),
                "case-sensitive rung uses translate: {rung}"
            );
        }
    }

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        // RFC 4648 vectors.
        for (input, want) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(super::base64_encode(input), want);
        }
        // Binary-safe (high bytes map into +/ territory).
        assert_eq!(super::base64_encode(&[0xfb, 0xff, 0xfe]), "+//+");
    }
}
