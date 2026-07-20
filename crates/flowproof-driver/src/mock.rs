//! A scriptable in-memory [`AppDriver`] used by unit tests across the
//! workspace (recorder and replayer logic is driver-generic, so the full
//! record→replay pipeline is testable on any platform).

use std::collections::HashMap;
use std::time::Duration;

use crate::app::{AppDriver, KeyMod, PixelRect, UiaSelector};
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
    /// Scene JSON returned by `scene` (None = authoring unavailable).
    pub scene: Option<String>,
    /// Element keys that report as disabled via `element_enabled`.
    pub disabled: Vec<String>,
    /// Scripted text sequences: each `read_text` on the key pops the next
    /// entry (falling back to `texts` when drained) — simulates a slow UI
    /// whose text changes over time, for auto-wait tests.
    pub text_sequence: HashMap<String, std::collections::VecDeque<String>>,
    /// Keys pressed via `press_key`, formatted like `Ctrl+V` / `Enter`.
    pub keys_pressed: Vec<String>,
    /// Element keys whose text was cleared, in order.
    pub cleared: Vec<String>,
    /// Text typed into the focused element, in order.
    pub typed_focused: Vec<String>,
    /// Session staged via `stage_session` (resolved values).
    pub staged_session: Option<crate::app::WebSession>,
    /// URLs visited via `navigate`, in order.
    pub navigations: Vec<String>,
    /// Number of `reload` calls.
    pub reloads: usize,
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

    /// Key under which `surface_text` (and its scripted sequence) is stored.
    pub const SURFACE: &'static str = "__surface__";

    pub fn with_surface_text(mut self, text: &str) -> Self {
        self.texts.insert(Self::SURFACE.into(), text.into());
        self
    }

    /// The key a selector matches in the fake UI tree: automation id, css,
    /// or — like the real UIA driver's find-by-name — the accessible name
    /// (used by structural / text-anchor ladder rungs).
    fn id_of(selector: &UiaSelector) -> Result<&str, DriverError> {
        selector
            .automation_id
            .as_deref()
            .or(selector.css.as_deref())
            .or(selector.name.as_deref())
            .ok_or_else(|| {
                DriverError::Uia("mock driver only matches automation ids, css, or names".into())
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
        // Ladder rungs without any matchable key simply don't match in the
        // mock, rather than erroring.
        match selector
            .automation_id
            .as_ref()
            .or(selector.css.as_ref())
            .or(selector.name.as_ref())
        {
            Some(id) => Ok(self.elements.contains(id)),
            None => Ok(false),
        }
    }

    fn element_enabled(&mut self, selector: &UiaSelector) -> Result<bool, DriverError> {
        let key = selector
            .automation_id
            .as_ref()
            .or(selector.css.as_ref())
            .or(selector.name.as_ref())
            .ok_or_else(|| DriverError::Uia("mock: selector has no matchable key".into()))?;
        Ok(!self.disabled.contains(key))
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
        if let Some(queue) = self.text_sequence.get_mut(id) {
            if let Some(next) = queue.pop_front() {
                return Ok(next);
            }
        }
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

    fn surface_text(&mut self) -> Result<String, DriverError> {
        if let Some(queue) = self.text_sequence.get_mut(Self::SURFACE) {
            if let Some(next) = queue.pop_front() {
                return Ok(next);
            }
        }
        self.texts
            .get(Self::SURFACE)
            .cloned()
            .ok_or_else(|| DriverError::Uia("mock has no surface text".into()))
    }

    fn clear_text(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?;
        if !self.elements.iter().any(|e| e == id) {
            return Err(DriverError::Uia(format!("mock element '{id}' not found")));
        }
        self.texts.insert(id.to_string(), String::new());
        self.cleared.push(id.to_string());
        Ok(())
    }

    fn type_focused(&mut self, text: &str) -> Result<(), DriverError> {
        self.typed_focused.push(text.to_string());
        Ok(())
    }

    fn press_key(&mut self, key: &str, modifiers: &[KeyMod]) -> Result<(), DriverError> {
        let mut chord: Vec<String> = modifiers.iter().map(ToString::to_string).collect();
        chord.push(key.to_string());
        self.keys_pressed.push(chord.join("+"));
        Ok(())
    }

    fn stage_session(&mut self, session: crate::app::WebSession) -> Result<(), DriverError> {
        self.staged_session = Some(session);
        Ok(())
    }

    fn navigate(&mut self, url: &str) -> Result<(), DriverError> {
        self.navigations.push(url.to_string());
        Ok(())
    }

    fn reload(&mut self) -> Result<(), DriverError> {
        self.reloads += 1;
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

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        Ok(self.scene.clone())
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
