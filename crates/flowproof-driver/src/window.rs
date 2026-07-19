//! Top-level window discovery for pixels-only driving: find a window by
//! title substring, bring it to the foreground, and read its screen
//! rectangle — the whole "attach" story for a Citrix/RDP/vision target.

#![cfg(windows)]

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowRect, GetWindowTextW, IsIconic, IsWindowVisible, SetForegroundWindow,
    ShowWindow, SW_RESTORE,
};

use crate::app::PixelRect;
use crate::DriverError;

/// A visible top-level window matched by title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowInfo {
    pub title: String,
    /// Screen rectangle `(x, y, width, height)` in physical pixels.
    pub rect: PixelRect,
    hwnd: isize,
}

struct EnumState {
    needle: String,
    found: Option<(isize, String)>,
}

extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
    let state = unsafe { &mut *(lparam.0 as *mut EnumState) };
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return true.into();
    }
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        return true.into();
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);
    if title.to_lowercase().contains(&state.needle) {
        state.found = Some((hwnd.0 as isize, title));
        return false.into(); // stop enumerating
    }
    true.into()
}

/// First visible top-level window whose title contains `title_contains`
/// (case-insensitive). `Ok(None)` = no such window right now.
pub fn find_window(title_contains: &str) -> Result<Option<WindowInfo>, DriverError> {
    let mut state = EnumState {
        needle: title_contains.to_lowercase(),
        found: None,
    };
    // EnumWindows returns FALSE when the callback stops early — that is a
    // match, not an error.
    let _ = unsafe {
        EnumWindows(
            Some(enum_proc),
            LPARAM(&mut state as *mut EnumState as isize),
        )
    };
    let Some((hwnd, title)) = state.found else {
        return Ok(None);
    };
    let mut rect = RECT::default();
    unsafe { GetWindowRect(HWND(hwnd as *mut _), &mut rect) }
        .map_err(|e| DriverError::Uia(format!("reading bounds of '{title}': {e}")))?;
    Ok(Some(WindowInfo {
        title,
        rect: (
            rect.left,
            rect.top,
            (rect.right - rect.left).unsigned_abs(),
            (rect.bottom - rect.top).unsigned_abs(),
        ),
        hwnd,
    }))
}

impl WindowInfo {
    /// Bring the window to the foreground (restoring it if minimized) so
    /// capture and injected input reach it.
    pub fn focus(&self) -> Result<(), DriverError> {
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            // Foregrounding can be refused (focus-steal prevention); the
            // caller re-checks by capturing, so a refusal is not fatal.
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(())
    }

    /// Re-read the window's current screen rectangle.
    pub fn refresh_rect(&mut self) -> Result<PixelRect, DriverError> {
        let mut rect = RECT::default();
        unsafe { GetWindowRect(HWND(self.hwnd as *mut _), &mut rect) }
            .map_err(|e| DriverError::Uia(format!("reading bounds of '{}': {e}", self.title)))?;
        self.rect = (
            rect.left,
            rect.top,
            (rect.right - rect.left).unsigned_abs(),
            (rect.bottom - rect.top).unsigned_abs(),
        );
        Ok(self.rect)
    }
}
