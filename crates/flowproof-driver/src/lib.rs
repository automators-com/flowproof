//! Native driver for flowproof: screen capture (DXGI), input injection
//! (SendInput), and UI Automation (UIA) client access.
//!
//! The real backend is Windows-only. On other platforms a stub backend is
//! compiled so the workspace always builds (e.g. on Linux CI); every
//! operation on the stub returns [`DriverError::UnsupportedPlatform`].

pub mod app;
mod backend;
#[cfg(windows)]
pub mod gdi;
pub mod mock;
#[cfg(feature = "oob")]
pub mod oob;
pub mod recording;
pub mod redact;
pub mod visual;
#[cfg(windows)]
pub mod window;

pub use app::{
    absolute_url, capture_matches, count_matching, numeric_value, resolve_app, split_command_line,
    text_contains, text_occurrences, url_matches, url_origin, AppDriver, AppTarget, CellHints,
    CellQuery, DebugBundle, KeyMod, NoOpDriver, PixelRect, UiaAppDriver, UiaSelector,
    WebBrowserConfig, WebClock, WebMock, WebSession, WebViewport, COUNT_DIAGNOSTIC_CAP,
};
pub use backend::PlatformBackend;
pub use recording::{FrameRef, Recording, RunRecorder, StepTiming};
pub use redact::{RedactMode, RedactTarget, RedactionRule};

/// A captured frame of the target screen or window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// RGBA8 pixel data, row-major, `width * height * 4` bytes.
    pub data: Vec<u8>,
}

/// A keyboard/mouse input event to inject.
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    MouseMove { x: i32, y: i32 },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    KeyDown { virtual_key: u16 },
    KeyUp { virtual_key: u16 },
    Text { text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// Canonical key name → Windows virtual-key code. One table for every
/// SendInput-based driver (UIA, vision), so `Press Escape` means the same
/// keystroke on each. Names are the grammar's canonical spellings.
pub fn virtual_key(key: &str) -> Option<u16> {
    let named = match key {
        "Enter" => 0x0D,
        "Escape" => 0x1B,
        "Tab" => 0x09,
        "Backspace" => 0x08,
        "Delete" => 0x2E,
        "Space" => 0x20,
        "ArrowLeft" => 0x25,
        "ArrowUp" => 0x26,
        "ArrowRight" => 0x27,
        "ArrowDown" => 0x28,
        "Home" => 0x24,
        "End" => 0x23,
        "PageUp" => 0x21,
        "PageDown" => 0x22,
        _ => 0,
    };
    if named != 0 {
        return Some(named);
    }
    if let Some(n) = key
        .strip_prefix(['F', 'f'])
        .and_then(|n| n.parse::<u16>().ok())
    {
        if (1..=12).contains(&n) {
            return Some(0x6F + n); // F1 = 0x70
        }
    }
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii_alphanumeric() => {
            Some(c.to_ascii_uppercase() as u16) // VK for A-Z/0-9 equals ASCII
        }
        _ => None,
    }
}

/// Modifier → its virtual-key code (left-hand variants).
pub fn modifier_virtual_key(m: &KeyMod) -> u16 {
    match m {
        KeyMod::Ctrl => 0x11,
        KeyMod::Alt => 0x12,
        KeyMod::Shift => 0x10,
        KeyMod::Meta => 0x5B,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("driver backend not supported on this platform (Windows-only feature)")]
    UnsupportedPlatform,
    #[error("capture failed: {0}")]
    Capture(String),
    #[error("input injection failed: {0}")]
    Input(String),
    #[error("UIA query failed: {0}")]
    Uia(String),
    /// A browser-driver fault. Distinct from [`DriverError::Uia`] so a web
    /// flow never reports the Windows adapter's name (a field report caught
    /// `UIA query failed` on a headless Chrome run).
    #[error("browser driver failed: {0}")]
    Browser(String),
    /// A fault of the DRIVER TRANSPORT itself (a dead CDP websocket, a
    /// dropped event) rather than an observation about the app. Callers
    /// polling inside an auto-wait budget treat this as a MISS and keep
    /// polling: the app was never asked, so nothing was learned about it.
    #[error("driver transport fault: {0}")]
    Transport(String),
}

impl DriverError {
    /// Is this a transport fault - one that says nothing about the app and
    /// may well be gone on the next poll? Assertion loops tolerate these
    /// within their recorded wait budget; every other error propagates.
    pub fn is_transient(&self) -> bool {
        matches!(self, DriverError::Transport(_))
    }
}

/// Screen/window capture source.
pub trait Capture {
    fn capture_frame(&mut self) -> Result<Frame, DriverError>;
}

/// Input injection sink.
pub trait Input {
    fn inject(&mut self, event: &InputEvent) -> Result<(), DriverError>;
}

/// Read access to the UI Automation tree of the target application.
pub trait UiaTree {
    /// Serialized snapshot of the accessibility tree (JSON), used to build
    /// the scene graph.
    fn snapshot(&mut self) -> Result<String, DriverError>;
}

/// Entry point: constructs the platform backend.
pub fn platform_backend() -> PlatformBackend {
    PlatformBackend::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_backend_reports_unsupported_off_windows() {
        let mut backend = platform_backend();
        let result = backend.capture_frame();
        if cfg!(windows) {
            // The Windows backend captures via GDI — succeeds on a real
            // desktop session, errors headless. Either way it must not
            // claim the platform is unsupported.
            assert!(!matches!(result, Err(DriverError::UnsupportedPlatform)));
        } else {
            assert!(matches!(result, Err(DriverError::UnsupportedPlatform)));
        }
    }

    #[test]
    fn input_event_roundtrips_clone() {
        let ev = InputEvent::Text {
            text: "hello".into(),
        };
        assert_eq!(ev.clone(), ev);
    }
}
