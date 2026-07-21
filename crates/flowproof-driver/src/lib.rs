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
#[cfg(windows)]
pub mod window;

pub use app::{
    absolute_url, numeric_value, resolve_app, url_origin, AppDriver, AppTarget, DebugBundle,
    KeyMod, NoOpDriver, PixelRect, UiaAppDriver, UiaSelector, WebBrowserConfig, WebMock,
    WebSession, WebViewport,
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
