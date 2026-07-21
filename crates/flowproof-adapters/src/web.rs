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
use headless_chrome::protocol::cdp::{Network, Page};
use headless_chrome::{Browser, LaunchOptions};

use crate::AdapterError;

const FIND_TIMEOUT: Duration = Duration::from_secs(5);

fn web_err(context: &str, err: impl std::fmt::Display) -> DriverError {
    DriverError::Uia(format!("web: {context}: {err}"))
}

/// Launch a fresh headless Chromium (`CHROME` env var overrides the binary).
fn launch_browser() -> Result<Browser, AdapterError> {
    let mut options = LaunchOptions::default_builder();
    options.headless(true).sandbox(false);
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
    let browser = launch_browser()?;
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
}

/// Cap on retained console lines — enough context for a failure, bounded
/// so a chatty app can't balloon the run bundle.
const CONSOLE_TAIL_CAP: usize = 100;

impl WebAppDriver {
    /// A driver on the shared browser (isolated context per flow), or a
    /// private browser when `FLOWPROOF_NO_SHARED_BROWSER=1`.
    pub fn new() -> Result<Self, AdapterError> {
        if std::env::var_os("FLOWPROOF_NO_SHARED_BROWSER").is_some() {
            return Ok(Self {
                browser: launch_browser()?,
                context_id: None,
                tab: None,
                staged_session: None,
                console: Default::default(),
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
            .ok_or_else(|| DriverError::Uia("web: no page open: call launch first".into()))
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
            DriverError::Uia(format!(
                "web: selector [{selector}] has no css, automation_id, or text"
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
                return Err(DriverError::Uia(format!("web: no element for {locator}")));
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
/// subtree text (covers labels wrapped in spans); (3) and (4) the same two
/// as prefix matches (a leading match is accepted when the accessible name
/// carries trailing detail — catalog cards, chips like `ID: …`).
fn text_xpaths(text: &str) -> [String; 4] {
    let lit = xpath_literal(text);
    let build = |by_text: String, by_label: String, by_placeholder: String| {
        format!(
            "//*[self::button or self::a or self::summary or @role='button' or \
             @role='tab' or @role='option' or @type='submit']\
             [{by_text} or {by_label}] | \
             //input[{by_placeholder} or {by_label}] | \
             //textarea[{by_placeholder} or {by_label}]"
        )
    };
    [
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
        if let Some(session) = self.staged_session.take() {
            Self::apply_session(&tab, &session, command)?;
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
        let value = self
            .tab()?
            .evaluate("document.body ? document.body.innerText : ''", false)
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

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        self.with_element(&locator, &format!("clicking [{selector}]"), |element| {
            element.click().map(|_| ())
        })
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
            .ok_or_else(|| DriverError::Uia("web: scene script returned no value".into()))?;
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
