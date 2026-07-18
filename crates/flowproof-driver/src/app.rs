//! Application driving via UI Automation: launch a target app, find elements
//! by native selector, invoke them, and read their text. This is the surface
//! the recorder and replayer use.
//!
//! The Windows implementation wraps the `uiautomation` crate (a safe wrapper
//! over the Win32 UIA COM API). Elsewhere the stub returns
//! [`DriverError::UnsupportedPlatform`].

use std::time::Duration;

use crate::DriverError;

/// A native element selector: any combination of UIA properties; all set
/// fields must match.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiaSelector {
    pub automation_id: Option<String>,
    pub name: Option<String>,
    pub control_type: Option<String>,
}

impl UiaSelector {
    pub fn automation_id(id: impl Into<String>) -> Self {
        Self {
            automation_id: Some(id.into()),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.automation_id.is_none() && self.name.is_none() && self.control_type.is_none()
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
        write!(f, "{}", parts.join(","))
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

    /// Primary screen size in physical pixels (used for trace headers).
    fn screen_size(&mut self) -> Result<(u32, u32), DriverError>;
}

/// How to launch a known application id from a flow spec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppTarget {
    pub command: &'static str,
    pub window_name: &'static str,
}

/// Resolve a spec `app:` id to a launch target. Shared by record and replay
/// so a trace only needs to carry the app id.
pub fn resolve_app(app_id: &str) -> Option<AppTarget> {
    match app_id {
        "calc" => Some(AppTarget {
            command: "calc.exe",
            window_name: "Calculator",
        }),
        _ => None,
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
        };
        assert_eq!(sel.to_string(), "automation_id=num5Button,name=Five");
        assert!(!sel.is_empty());
        assert!(UiaSelector::default().is_empty());
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
