//! Browser adapter: drives a page in headless Chromium over the DevTools
//! protocol, implementing the same [`AppDriver`] surface the UIA driver
//! exposes — so the recorder and replayer work unchanged.
//!
//! Selector mapping: `css` payload key, else `#<automation_id>`. `launch`
//! interprets `command` as the URL to open. The Chromium binary is found via
//! the `CHROME` env var or platform auto-detection.

use std::sync::Arc;
use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, KeyMod, PixelRect, UiaSelector, WebSession};
use headless_chrome::browser::tab::{ModifierKey, Tab};
use headless_chrome::protocol::cdp::{Network, Page};
use headless_chrome::{Browser, LaunchOptions};

use crate::AdapterError;

const FIND_TIMEOUT: Duration = Duration::from_secs(5);

fn web_err(context: &str, err: impl std::fmt::Display) -> DriverError {
    DriverError::Uia(format!("web: {context}: {err}"))
}

/// Browser-backed [`AppDriver`].
pub struct WebAppDriver {
    browser: Browser,
    tab: Option<Arc<Tab>>,
    /// Session staged via [`AppDriver::stage_session`], applied by the next
    /// `launch` before the page loads.
    staged_session: Option<WebSession>,
}

impl WebAppDriver {
    /// Launch headless Chromium (`CHROME` env var overrides the binary).
    pub fn new() -> Result<Self, AdapterError> {
        let mut options = LaunchOptions::default_builder();
        options.headless(true).sandbox(false);
        if let Ok(path) = std::env::var("CHROME") {
            options.path(Some(path.into()));
        }
        let options = options
            .build()
            .map_err(|e| AdapterError::Web(format!("building launch options: {e}")))?;
        let browser = Browser::new(options)
            .map_err(|e| AdapterError::Web(format!("launching browser: {e}")))?;
        Ok(Self {
            browser,
            tab: None,
            staged_session: None,
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
            for xpath in [text_xpath(text, false), text_xpath(text, true)] {
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

/// XPath matching an interactable element by its visible text, accessible
/// label, or placeholder — Playwright's text/placeholder addressing. With
/// `prefix`, the text only has to START with the anchor (used as a second
/// pass when no exact match exists).
fn text_xpath(text: &str, prefix: bool) -> String {
    let lit = xpath_literal(text);
    let (by_text, by_label, by_placeholder) = if prefix {
        (
            format!("starts-with(normalize-space(), {lit})"),
            format!("starts-with(@aria-label, {lit})"),
            format!("starts-with(@placeholder, {lit})"),
        )
    } else {
        (
            format!("normalize-space()={lit}"),
            format!("@aria-label={lit}"),
            format!("@placeholder={lit}"),
        )
    };
    format!(
        "//*[self::button or self::a or self::summary or @role='button' or \
         @role='tab' or @role='option' or @type='submit']\
         [{by_text} or {by_label}] | \
         //input[{by_placeholder} or {by_label}] | \
         //textarea[{by_placeholder} or {by_label}]"
    )
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

impl AppDriver for WebAppDriver {
    /// `command` is the URL to open; `window_name` is unused for web.
    fn launch(
        &mut self,
        command: &str,
        _window_name: &str,
        _timeout: Duration,
    ) -> Result<(), DriverError> {
        let tab = self
            .browser
            .new_tab()
            .map_err(|e| web_err("opening tab", e))?;
        if let Some(session) = self.staged_session.take() {
            Self::apply_session(&tab, &session, command)?;
        }
        tab.navigate_to(command)
            .map_err(|e| web_err(&format!("navigating to {command}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        self.tab = Some(tab);
        Ok(())
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

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        self.find(&locator)?
            .click()
            .map_err(|e| web_err(&format!("clicking [{selector}]"), e))?;
        Ok(())
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        let locator = Self::locator(selector)?;
        let element = self.find(&locator)?;
        // Inner text covers most elements; inputs expose their VALUE — the
        // text a user sees in the box (Playwright's toHaveValue reading).
        let value = element
            .call_js_fn(
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
            .map_err(|e| web_err(&format!("reading text of [{selector}]"), e))?;
        Ok(value
            .value
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default())
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        self.find(&locator)?
            .click()
            .map_err(|e| web_err(&format!("focusing [{selector}]"), e))?
            .type_into(text)
            .map_err(|e| web_err(&format!("typing into [{selector}]"), e))?;
        Ok(())
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let locator = Self::locator(selector)?;
        let element = self.find(&locator)?;
        // Go through the native value setter so framework-controlled inputs
        // (React et al.) see the change, then fire the events they listen to.
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
            .map_err(|e| web_err(&format!("clearing [{selector}]"), e))?;
        Ok(())
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
