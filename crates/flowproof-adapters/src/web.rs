//! Browser adapter: drives a page in headless Chromium over the DevTools
//! protocol, implementing the same [`AppDriver`] surface the UIA driver
//! exposes — so the recorder and replayer work unchanged.
//!
//! Selector mapping: `css` payload key, else `#<automation_id>`. `launch`
//! interprets `command` as the URL to open. The Chromium binary is found via
//! the `CHROME` env var or platform auto-detection.

use std::sync::Arc;
use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, PixelRect, UiaSelector};
use headless_chrome::browser::tab::Tab;
use headless_chrome::protocol::cdp::Page;
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
        Ok(Self { browser, tab: None })
    }

    fn tab(&self) -> Result<&Arc<Tab>, DriverError> {
        self.tab
            .as_ref()
            .ok_or_else(|| DriverError::Uia("web: no page open: call launch first".into()))
    }

    fn locator_of(selector: &UiaSelector) -> Option<WebLocator> {
        if let Some(css) = selector.css_selector() {
            return Some(WebLocator::Css(css));
        }
        // Text anchor: find by visible text / accessible label / placeholder
        // — how humans (and Playwright's getByText/getByPlaceholder/getByRole
        // name matching) address elements on pages without ids.
        selector
            .name
            .as_ref()
            .map(|text| WebLocator::XPath(text_xpath(text)))
    }

    fn locator(selector: &UiaSelector) -> Result<WebLocator, DriverError> {
        Self::locator_of(selector).ok_or_else(|| {
            DriverError::Uia(format!(
                "web: selector [{selector}] has no css, automation_id, or text"
            ))
        })
    }

    fn find(&self, locator: &WebLocator) -> Result<headless_chrome::Element<'_>, DriverError> {
        let tab = self.tab()?;
        match locator {
            WebLocator::Css(css) => tab
                .wait_for_element_with_custom_timeout(css, FIND_TIMEOUT)
                .map_err(|e| web_err(&format!("finding {css}"), e)),
            WebLocator::XPath(xpath) => tab
                .wait_for_xpath_with_custom_timeout(xpath, FIND_TIMEOUT)
                .map_err(|e| web_err(&format!("finding element by text ({xpath})"), e)),
        }
    }

    fn exists(&self, locator: &WebLocator) -> Result<bool, DriverError> {
        let tab = self.tab()?;
        Ok(match locator {
            WebLocator::Css(css) => tab.find_element(css).is_ok(),
            WebLocator::XPath(xpath) => tab.find_element_by_xpath(xpath).is_ok(),
        })
    }
}

/// How a [`UiaSelector`] resolves on a page.
enum WebLocator {
    Css(String),
    XPath(String),
}

/// XPath matching an interactable element by its visible text, accessible
/// label, or placeholder — Playwright's text/placeholder addressing.
fn text_xpath(text: &str) -> String {
    let lit = xpath_literal(text);
    format!(
        "//*[self::button or self::a or self::summary or @role='button' or \
         @role='tab' or @role='option' or @type='submit']\
         [normalize-space()={lit} or @aria-label={lit}] | \
         //input[@placeholder={lit} or @aria-label={lit}] | \
         //textarea[@placeholder={lit} or @aria-label={lit}]"
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
        tab.navigate_to(command)
            .map_err(|e| web_err(&format!("navigating to {command}"), e))?;
        tab.wait_until_navigated()
            .map_err(|e| web_err("waiting for page load", e))?;
        self.tab = Some(tab);
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
        // Inner text covers most elements; inputs expose their value instead.
        let text = element
            .get_inner_text()
            .map_err(|e| web_err(&format!("reading text of [{selector}]"), e))?;
        Ok(text)
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
        let tab = self.tab()?;
        let found = match &locator {
            WebLocator::Css(css) => tab.find_element(css),
            WebLocator::XPath(xpath) => tab.find_element_by_xpath(xpath),
        };
        let Ok(element) = found else {
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
