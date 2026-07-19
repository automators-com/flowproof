//! Platform backend selection. The Windows backend hosts the real
//! SendInput implementation (DXGI capture and a standalone UIA snapshot
//! remain future work); the portable stub keeps non-Windows builds green.

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
        // GDI keyframe capture; DXGI desktop duplication is future work.
        let image = crate::gdi::capture_screen()?;
        Ok(Frame {
            width: image.width(),
            height: image.height(),
            data: image.into_raw(),
        })
    }
}

#[cfg(windows)]
impl Input for PlatformBackend {
    fn inject(&mut self, event: &InputEvent) -> Result<(), DriverError> {
        win_input::inject(event)
    }
}

#[cfg(windows)]
impl UiaTree for PlatformBackend {
    fn snapshot(&mut self) -> Result<String, DriverError> {
        // The scene contract lives on AppDriver::scene today.
        Err(DriverError::Uia("UIA snapshot not implemented yet".into()))
    }
}

/// OS-level input injection via `SendInput` — the pixels-only provenance's
/// way of acting: no accessibility API, exactly the events a user's mouse
/// and keyboard would produce.
#[cfg(windows)]
mod win_input {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

    use crate::{DriverError, InputEvent, MouseButton};

    fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32) -> INPUT {
        INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx,
                    dy,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        }
    }

    fn key(vk: u16, scan: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(vk),
                    wScan: scan,
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        }
    }

    fn send(inputs: &[INPUT]) -> Result<(), DriverError> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize == inputs.len() {
            Ok(())
        } else {
            Err(DriverError::Input(format!(
                "SendInput injected {sent} of {} events (input may be blocked)",
                inputs.len()
            )))
        }
    }

    pub(super) fn inject(event: &InputEvent) -> Result<(), DriverError> {
        match event {
            InputEvent::MouseMove { x, y } => {
                // Absolute coordinates are normalized to a 0..=65535 grid
                // over the primary screen.
                let (w, h) = unsafe {
                    (
                        GetSystemMetrics(SM_CXSCREEN).max(2),
                        GetSystemMetrics(SM_CYSCREEN).max(2),
                    )
                };
                let dx = ((*x as i64 * 65535) / (w as i64 - 1)) as i32;
                let dy = ((*y as i64 * 65535) / (h as i64 - 1)) as i32;
                send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE, dx, dy)])
            }
            InputEvent::MouseDown { button } => send(&[mouse(
                match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
                    MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
                },
                0,
                0,
            )]),
            InputEvent::MouseUp { button } => send(&[mouse(
                match button {
                    MouseButton::Left => MOUSEEVENTF_LEFTUP,
                    MouseButton::Right => MOUSEEVENTF_RIGHTUP,
                    MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
                },
                0,
                0,
            )]),
            InputEvent::KeyDown { virtual_key } => {
                send(&[key(*virtual_key, 0, KEYBD_EVENT_FLAGS(0))])
            }
            InputEvent::KeyUp { virtual_key } => send(&[key(*virtual_key, 0, KEYEVENTF_KEYUP)]),
            InputEvent::Text { text } => {
                // KEYEVENTF_UNICODE types text independent of keyboard
                // layout; each UTF-16 unit gets a down+up pair.
                let mut inputs = Vec::new();
                for unit in text.encode_utf16() {
                    inputs.push(key(0, unit, KEYEVENTF_UNICODE));
                    inputs.push(key(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
                }
                if inputs.is_empty() {
                    return Ok(());
                }
                send(&inputs)
            }
        }
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
