//! Platform backend selection. The Windows backend will host the real DXGI /
//! SendInput / UIA implementations; the portable stub keeps non-Windows
//! builds green.

use crate::{Capture, DriverError, Frame, Input, InputEvent, UiaTree};

/// The concrete backend for the current platform.
#[derive(Debug, Default)]
pub struct PlatformBackend {
    _private: (),
}

impl PlatformBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(windows)]
impl Capture for PlatformBackend {
    fn capture_frame(&mut self) -> Result<Frame, DriverError> {
        // TODO: DXGI desktop duplication.
        Err(DriverError::Capture(
            "DXGI capture not implemented yet".into(),
        ))
    }
}

#[cfg(windows)]
impl Input for PlatformBackend {
    fn inject(&mut self, _event: &InputEvent) -> Result<(), DriverError> {
        // TODO: SendInput.
        Err(DriverError::Input("SendInput not implemented yet".into()))
    }
}

#[cfg(windows)]
impl UiaTree for PlatformBackend {
    fn snapshot(&mut self) -> Result<String, DriverError> {
        // TODO: UIA client (IUIAutomation).
        Err(DriverError::Uia("UIA snapshot not implemented yet".into()))
    }
}

#[cfg(not(windows))]
impl Capture for PlatformBackend {
    fn capture_frame(&mut self) -> Result<Frame, DriverError> {
        Err(DriverError::UnsupportedPlatform)
    }
}

#[cfg(not(windows))]
impl Input for PlatformBackend {
    fn inject(&mut self, _event: &InputEvent) -> Result<(), DriverError> {
        Err(DriverError::UnsupportedPlatform)
    }
}

#[cfg(not(windows))]
impl UiaTree for PlatformBackend {
    fn snapshot(&mut self) -> Result<String, DriverError> {
        Err(DriverError::UnsupportedPlatform)
    }
}
