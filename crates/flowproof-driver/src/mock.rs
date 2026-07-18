//! A scriptable in-memory [`AppDriver`] used by unit tests across the
//! workspace (recorder and replayer logic is driver-generic, so the full
//! record→replay pipeline is testable on any platform).

use std::collections::HashMap;
use std::time::Duration;

use crate::app::{AppDriver, PixelRect, UiaSelector};
use crate::DriverError;

#[derive(Debug, Default)]
pub struct MockAppDriver {
    /// `(command, window_name)` captured by `launch`.
    pub launched: Option<(String, String)>,
    /// Automation ids that exist in the fake UI tree.
    pub elements: Vec<String>,
    /// Text returned by `read_text`, keyed by automation id.
    pub texts: HashMap<String, String>,
    /// Automation ids invoked, in order.
    pub invoked: Vec<String>,
    /// `(automation_id, text)` pairs typed, in order.
    pub typed: Vec<(String, String)>,
    pub screen: (u32, u32),
    /// Frame returned by `capture` (None = capture unsupported).
    pub frame: Option<image::RgbaImage>,
    /// Element rects by automation id / css key, for redaction tests.
    pub rects: HashMap<String, PixelRect>,
    /// Password-field rects, always masked.
    pub password_fields: Vec<PixelRect>,
    /// When set, `element_rect` fails — exercises fail-closed redaction.
    pub fail_element_rect: bool,
}

impl MockAppDriver {
    pub fn new(elements: &[&str]) -> Self {
        Self {
            elements: elements.iter().map(|s| s.to_string()).collect(),
            screen: (1920, 1080),
            ..Self::default()
        }
    }

    pub fn with_text(mut self, automation_id: &str, text: &str) -> Self {
        self.texts.insert(automation_id.into(), text.into());
        self
    }

    fn id_of(selector: &UiaSelector) -> Result<&str, DriverError> {
        selector
            .automation_id
            .as_deref()
            .or(selector.css.as_deref())
            .ok_or_else(|| {
                DriverError::Uia("mock driver only matches automation ids or css".into())
            })
    }
}

impl AppDriver for MockAppDriver {
    fn launch(
        &mut self,
        command: &str,
        window_name: &str,
        _timeout: Duration,
    ) -> Result<(), DriverError> {
        self.launched = Some((command.into(), window_name.into()));
        Ok(())
    }

    fn element_exists(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        // Ladder rungs without a matchable key (e.g. control-type fallbacks)
        // simply don't match in the mock, rather than erroring.
        match selector.automation_id.as_ref().or(selector.css.as_ref()) {
            Some(id) => Ok(self.elements.contains(id)),
            None => Ok(false),
        }
    }

    fn invoke(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?;
        if !self.elements.iter().any(|e| e == id) {
            return Err(DriverError::Uia(format!("mock element '{id}' not found")));
        }
        self.invoked.push(id.to_string());
        Ok(())
    }

    fn read_text(&mut self, selector: &UiaSelector) -> Result<String, DriverError> {
        let id = Self::id_of(selector)?;
        self.texts
            .get(id)
            .cloned()
            .ok_or_else(|| DriverError::Uia(format!("mock element '{id}' has no text")))
    }

    fn type_text(&mut self, selector: &UiaSelector, text: &str) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?;
        if !self.elements.iter().any(|e| e == id) {
            return Err(DriverError::Uia(format!("mock element '{id}' not found")));
        }
        // Typing appends to the element's text, like a real edit control.
        self.texts.entry(id.to_string()).or_default().push_str(text);
        self.typed.push((id.to_string(), text.to_string()));
        Ok(())
    }

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        Ok(self.screen)
    }

    fn capture(&mut self) -> Result<Option<image::RgbaImage>, DriverError> {
        Ok(self.frame.clone())
    }

    fn element_rect(&mut self, selector: &UiaSelector) -> Result<Option<PixelRect>, DriverError> {
        if self.fail_element_rect {
            return Err(DriverError::Uia("mock element_rect failure".into()));
        }
        let key = selector
            .automation_id
            .as_deref()
            .or(selector.css.as_deref())
            .unwrap_or_default();
        Ok(self.rects.get(key).copied())
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        Ok(self.password_fields.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_scripts_a_ui() {
        let mut driver = MockAppDriver::new(&["num5Button", "CalculatorResults"])
            .with_text("CalculatorResults", "Display is 8");
        driver
            .launch("calc.exe", "Calculator", Duration::from_secs(1))
            .expect("mock launch");
        let five = UiaSelector::automation_id("num5Button");
        assert!(driver.element_exists(&five).expect("exists check"));
        driver.invoke(&five).expect("invoke");
        assert_eq!(driver.invoked, vec!["num5Button"]);
        assert_eq!(
            driver
                .read_text(&UiaSelector::automation_id("CalculatorResults"))
                .expect("read"),
            "Display is 8"
        );
    }
}
