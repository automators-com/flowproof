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
pub fn numeric_value(text: &str) -> Option<f64> {
    text.split_whitespace()
        .rev()
        .find_map(|token| token.replace(',', "").parse::<f64>().ok())
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
    fn selector_display_lists_set_fields() {
        let sel = UiaSelector {
            automation_id: Some("num5Button".into()),
            name: Some("Five".into()),
            control_type: None,
            css: None,
            nth: None,
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
