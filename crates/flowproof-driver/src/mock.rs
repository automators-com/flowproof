//! A scriptable in-memory [`AppDriver`] used by unit tests across the
//! workspace (recorder and replayer logic is driver-generic, so the full
//! record→replay pipeline is testable on any platform).

use std::collections::HashMap;
use std::time::Duration;

use crate::app::{AppDriver, UiaSelector};
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
    pub screen: (u32, u32),
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
            .ok_or_else(|| DriverError::Uia("mock driver only matches automation ids".into()))
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
        let id = Self::id_of(selector)?;
        Ok(self.elements.iter().any(|e| e == id))
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

    fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
        Ok(self.screen)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_scripts_a_ui() {
        let mut driver =
            MockAppDriver::new(&["num5Button", "CalculatorResults"]).with_text(
                "CalculatorResults",
                "Display is 8",
            );
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
