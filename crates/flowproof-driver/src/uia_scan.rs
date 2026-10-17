//! The controls a window's accessibility tree reports, with where each one
//! is drawn: name, control type and screen rectangle.
//!
//! Two readers. The vision adapter takes these as a second source of
//! elements beside OCR when the window it drives is local and answers UIA -
//! an icon-only button has no text for OCR to read, but it usually has an
//! accessible name, and that name is what an author writes (`Click
//! "Back"`). And the vision grounding benchmark takes them as ground truth:
//! a vision click on `Back` is right when it lands inside the rectangle UIA
//! reports for Back.
//!
//! A Citrix or RDP client reports one opaque pane for the whole remote
//! desktop, so over a remote session this finds nothing useful and vision
//! stays pixels-only - the case the adapter exists for.

#![cfg(windows)]

use uiautomation::types::{ControlType, Handle, TreeScope};
use uiautomation::UIAutomation;

use crate::app::PixelRect;
use crate::DriverError;

/// One control as the accessibility tree reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiaControl {
    /// Accessible name - empty when the app gave the control none.
    pub name: String,
    /// The control type's UIA name: `Button`, `Edit`, `MenuItem`, ...
    pub control_type: String,
    /// Screen rectangle, physical pixels.
    pub rect: PixelRect,
    pub enabled: bool,
}

impl UiaControl {
    /// A control a click or a keystroke is aimed at - what a step names -
    /// rather than structure (panes, groups) or decoration (images).
    pub fn is_interactive(&self) -> bool {
        matches!(
            self.control_type.as_str(),
            "Button"
                | "Edit"
                | "ComboBox"
                | "CheckBox"
                | "RadioButton"
                | "ListItem"
                | "TabItem"
                | "MenuItem"
                | "Hyperlink"
                | "SplitButton"
                | "TreeItem"
                | "DataItem"
                | "Slider"
        )
    }
}

/// Every control under the window `hwnd` that is on screen and has a
/// non-empty rectangle, in the order UIA walks them.
pub fn window_controls(hwnd: isize) -> Result<Vec<UiaControl>, DriverError> {
    let err = |context: &str, e: uiautomation::Error| DriverError::Uia(format!("{context}: {e}"));
    let automation = UIAutomation::new().map_err(|e| err("initializing UIA", e))?;
    let root = automation
        .element_from_handle(Handle::from(hwnd))
        .map_err(|e| err("UIA element for window", e))?;
    let all = automation
        .create_true_condition()
        .map_err(|e| err("UIA condition", e))?;
    let elements = root
        .find_all(TreeScope::Descendants, &all)
        .map_err(|e| err("walking the window's UIA tree", e))?;
    let mut controls = Vec::new();
    for element in elements {
        if element.is_offscreen().unwrap_or(true) {
            continue;
        }
        let Ok(rect) = element.get_bounding_rectangle() else {
            continue;
        };
        let (w, h) = (
            rect.get_right() - rect.get_left(),
            rect.get_bottom() - rect.get_top(),
        );
        if w <= 0 || h <= 0 {
            continue;
        }
        let control_type = element
            .get_control_type()
            .map(|t| match t {
                ControlType::SplitButton => "SplitButton".to_string(),
                other => format!("{other:?}"),
            })
            .unwrap_or_default();
        controls.push(UiaControl {
            name: element.get_name().unwrap_or_default().trim().to_string(),
            control_type,
            rect: (rect.get_left(), rect.get_top(), w as u32, h as u32),
            enabled: element.is_enabled().unwrap_or(true),
        });
    }
    Ok(controls)
}
