//! Browser adapter: drives a page in headless Chromium over the DevTools
//! protocol, implementing the same [`AppDriver`] surface the UIA driver
//! exposes — so the recorder and replayer work unchanged.
//!
//! Selector mapping: `css` payload key, else `#<automation_id>`. `launch`
//! interprets `command` as the URL to open. The Chromium binary is found via
//! the `CHROME` env var or platform auto-detection.

use std::sync::Arc;
use std::time::Duration;

use flowproof_driver::{AppDriver, DriverError, UiaSelector};
use headless_chrome::browser::tab::Tab;
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

    fn css_of(selector: &UiaSelector) -> Result<String, DriverError> {
        selector.css_selector().ok_or_else(|| {
            DriverError::Uia(format!(
                "web: selector [{selector}] has no css or automation_id"
            ))
        })
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
        let Some(css) = selector.css_selector() else {
            return Ok(false); // non-web ladder rungs simply don't match
        };
        Ok(self.tab()?.find_element(&css).is_ok())
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let css = Self::css_of(selector)?;
        self.tab()?
            .wait_for_element_with_custom_timeout(&css, FIND_TIMEOUT)
            .map_err(|e| web_err(&format!("finding {css}"), e))?
            .click()
            .map_err(|e| web_err(&format!("clicking {css}"), e))?;
        Ok(())
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        let css = Self::css_of(selector)?;
        let element = self
            .tab()?
            .wait_for_element_with_custom_timeout(&css, FIND_TIMEOUT)
            .map_err(|e| web_err(&format!("finding {css}"), e))?;
        // Inner text covers most elements; inputs expose their value instead.
        let text = element
            .get_inner_text()
            .map_err(|e| web_err(&format!("reading text of {css}"), e))?;
        Ok(text)
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        let css = Self::css_of(selector)?;
        self.tab()?
            .wait_for_element_with_custom_timeout(&css, FIND_TIMEOUT)
            .map_err(|e| web_err(&format!("finding {css}"), e))?
            .click()
            .map_err(|e| web_err(&format!("focusing {css}"), e))?
            .type_into(text)
            .map_err(|e| web_err(&format!("typing into {css}"), e))?;
        Ok(())
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        // Headless default viewport; good enough for trace metadata.
        Ok((1280, 720))
    }
}
