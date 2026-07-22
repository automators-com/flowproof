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
    /// How many elements an id stands for, when more than one. A real
    /// screen has three rows matching "Row"; without this the mock could
    /// only ever model one of anything, and no count assertion could be
    /// tested off a real adapter.
    pub occurrences: HashMap<String, usize>,
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
    /// Failure-time diagnostics returned by `debug_bundle` (None = the
    /// trait default: driver has nothing to add).
    pub debug: Option<crate::DebugBundle>,
    /// Element keys whose center a click would NOT reach (toast/overlay on
    /// top) — `element_receives_events` reports Some(false) for these.
    pub obscured: Vec<String>,
    /// Element keys still animating: each `element_rect` call while the
    /// remaining count is > 0 returns a shifted rect (and decrements), so
    /// stability gates see movement that later settles.
    pub moving: std::collections::HashMap<String, u32>,
    /// Network mocks captured by `stage_mocks` — the mock stands in for
    /// the web driver here, so tests can assert the staging happened.
    pub staged_mocks: Vec<crate::WebMock>,
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
    /// `(element key, file path)` pairs from `set_files`, in order.
    pub uploads: Vec<(String, String)>,
    /// Element keys right-clicked via `context_click`, in order.
    pub context_clicked: Vec<String>,
    /// Browser config captured by `stage_browser` — the mock stands in
    /// for the web driver here, so tests can assert the staging happened.
    pub staged_browser: Option<crate::WebBrowserConfig>,
    /// Geometry applied via `set_window_geometry`, for tests.
    pub geometry: Option<(u32, u32, i32, i32)>,
    /// When set, `set_window_geometry` fails - exercises the rule that a
    /// window which cannot be shaped ERRORS rather than minting a baseline.
    pub fail_geometry: bool,
    /// Checkbox state by element key. Absent = not a checkbox, which the
    /// driver reports as `None` so tests can cover that third answer.
    pub checked: HashMap<String, bool>,
    /// URL reported by `current_url`, for `page url is|contains` tests.
    pub url: Option<String>,
    /// Leading `surface_text` calls that fail with a TRANSPORT fault before
    /// any real answer — a dead CDP socket during an auto-wait poll.
    /// `u32::MAX` never recovers, which is how the "budget expired without
    /// a single reading" path is exercised.
    pub surface_faults: u32,
}

impl MockAppDriver {
    pub fn new(elements: &[&str]) -> Self {
        Self {
            elements: elements.iter().map(|s| s.to_string()).collect(),
            screen: (1920, 1080),
            ..Self::default()
        }
    }

    /// Model `count` elements matching this id, rather than one.
    pub fn with_occurrences(mut self, automation_id: &str, count: usize) -> Self {
        if !self.elements.iter().any(|e| e == automation_id) {
            self.elements.push(automation_id.into());
        }
        self.occurrences.insert(automation_id.into(), count);
        self
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

    pub fn with_checkbox(mut self, key: &str, checked: bool) -> Self {
        self.checked.insert(key.into(), checked);
        self
    }

    pub fn with_url(mut self, url: &str) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Fail the next `count` surface reads with a transport fault. Models
    /// the field case behind GAP-A: the socket dies mid-flow while the app
    /// itself is fine, so a poll learns nothing and must not fail the step.
    pub fn with_surface_faults(mut self, count: u32) -> Self {
        self.surface_faults = count;
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
            Some(id) => {
                if !self.elements.contains(id) {
                    return Ok(false);
                }
                // The Nth exists only while N is within the id's
                // multiplicity, which is what every real adapter does with
                // an ordinal and what makes counting mean the same thing
                // here as it does against a browser.
                let available = self.occurrences.get(id).copied().unwrap_or(1);
                Ok(selector.nth.unwrap_or(1).max(1) as usize <= available)
            }
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

    fn set_window_geometry(
        &mut self,
        width: u32,
        height: u32,
        position: Option<(i32, i32)>,
    ) -> Result<(u32, u32, i32, i32), DriverError> {
        if self.fail_geometry {
            return Err(DriverError::Uia("mock refuses to resize".into()));
        }
        // No position asked for: report where it "landed", which is what a
        // real window manager decides and what the trace then pins.
        let (x, y) = position.unwrap_or((40, 60));
        self.geometry = Some((width, height, x, y));
        Ok((width, height, x, y))
    }

    fn element_checked(&mut self, selector: &UiaSelector) -> Result<Option<bool>, DriverError> {
        let id = Self::id_of(selector)?;
        Ok(self.checked.get(id).copied())
    }

    fn set_checked(&mut self, selector: &UiaSelector, checked: bool) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?.to_string();
        if !self.checked.contains_key(&id) {
            return Err(DriverError::Uia(format!("{id} is not a checkbox")));
        }
        self.checked.insert(id, checked);
        Ok(())
    }

    fn current_url(&mut self) -> Result<String, DriverError> {
        self.url
            .clone()
            .ok_or_else(|| DriverError::Uia("mock has no url".into()))
    }

    fn surface_text(&mut self) -> Result<String, DriverError> {
        if self.surface_faults > 0 {
            // u32::MAX means "never recovers": leave it saturated.
            if self.surface_faults != u32::MAX {
                self.surface_faults -= 1;
            }
            return Err(DriverError::Transport(
                "Unable to make method calls because underlying connection is closed".into(),
            ));
        }
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
        // Scripted animation: shift the rect while polls remain.
        if let Some(remaining) = self.moving.get_mut(key) {
            if *remaining > 0 {
                *remaining -= 1;
                let offset = *remaining as i32 + 1;
                let (x, y, w, h) = self.rects.get(key).copied().unwrap_or((0, 0, 10, 10));
                return Ok(Some((x + offset, y, w, h)));
            }
        }
        Ok(self.rects.get(key).copied())
    }

    fn password_rects(&mut self) -> Result<Vec<PixelRect>, DriverError> {
        Ok(self.password_fields.clone())
    }

    fn scene(&mut self) -> Result<Option<String>, DriverError> {
        Ok(self.scene.clone())
    }

    fn debug_bundle(&mut self) -> Result<Option<crate::DebugBundle>, DriverError> {
        Ok(self.debug.clone())
    }

    fn element_receives_events(
        &mut self,
        selector: &UiaSelector,
    ) -> Result<Option<bool>, DriverError> {
        let key = selector
            .automation_id
            .as_deref()
            .or(selector.css.as_deref())
            .or(selector.name.as_deref())
            .unwrap_or_default();
        Ok(Some(!self.obscured.iter().any(|k| k == key)))
    }

    fn stage_mocks(&mut self, rules: Vec<crate::WebMock>) -> Result<(), DriverError> {
        self.staged_mocks = rules;
        Ok(())
    }

    fn set_files(&mut self, selector: &UiaSelector, paths: &[String]) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?;
        if !self.elements.iter().any(|e| e == id) {
            return Err(DriverError::Uia(format!("mock element '{id}' not found")));
        }
        for path in paths {
            self.uploads.push((id.to_string(), path.clone()));
        }
        Ok(())
    }

    fn context_click(&mut self, selector: &UiaSelector) -> Result<(), DriverError> {
        let id = Self::id_of(selector)?;
        if !self.elements.iter().any(|e| e == id) {
            return Err(DriverError::Uia(format!("mock element '{id}' not found")));
        }
        self.context_clicked.push(id.to_string());
        Ok(())
    }

    fn stage_browser(&mut self, config: crate::WebBrowserConfig) -> Result<(), DriverError> {
        self.staged_browser = Some(config);
        Ok(())
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
