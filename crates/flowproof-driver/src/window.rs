//! Top-level window discovery for pixels-only driving: find a window by
//! title substring, bring it to the foreground, and read its screen
//! rectangle — the whole "attach" story for a Citrix/RDP/vision target.

#![cfg(windows)]

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetAncestor, GetClassNameW, GetWindowRect, GetWindowTextW, IsIconic,
    IsWindowVisible, SetForegroundWindow, SetWindowPos, ShowWindow, GA_ROOT, SWP_NOACTIVATE,
    SWP_NOZORDER, SW_RESTORE,
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

impl WindowInfo {
    /// Size and position this window, returning what was ACTUALLY applied.
    ///
    /// UWP is the trap the field report named: a packaged app's visible
    /// window is a `Windows.UI.Core.CoreWindow` hosted inside an
    /// `ApplicationFrameWindow`, and resizing the CoreWindow moves nothing
    /// a user can see. Resize the frame ancestor instead when we are
    /// looking at one.
    pub fn set_geometry(
        &mut self,
        width: u32,
        height: u32,
        position: Option<(i32, i32)>,
    ) -> Result<(u32, u32, i32, i32), DriverError> {
        let target = self.frame_host().unwrap_or(self.hwnd);
        let hwnd = HWND(target as *mut _);
        // Keep the current position when the spec asked only for a size:
        // the caller records whatever it lands on, so replay pins it.
        let mut rect = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut rect) }
            .map_err(|e| DriverError::Uia(format!("reading bounds of '{}': {e}", self.title)))?;
        let (x, y) = position.unwrap_or((rect.left, rect.top));
        unsafe {
            SetWindowPos(
                hwnd,
                None,
                x,
                y,
                width as i32,
                height as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        }
        .map_err(|e| {
            DriverError::Uia(format!(
                "resizing '{}' to {width}x{height} at ({x},{y}): {e}",
                self.title
            ))
        })?;
        // Report what the window manager actually did, not what we asked
        // for: a DPI-scaled or minimum-size-constrained window lands
        // somewhere else, and the trace must record the truth.
        let mut applied = RECT::default();
        unsafe { GetWindowRect(hwnd, &mut applied) }
            .map_err(|e| DriverError::Uia(format!("re-reading bounds of '{}': {e}", self.title)))?;
        Ok((
            (applied.right - applied.left).unsigned_abs(),
            (applied.bottom - applied.top).unsigned_abs(),
            applied.left,
            applied.top,
        ))
    }

    /// The `ApplicationFrameWindow` hosting this window, when it is a UWP
    /// `CoreWindow`. `None` for an ordinary Win32 window, which is its own
    /// frame.
    fn frame_host(&self) -> Option<isize> {
        let hwnd = HWND(self.hwnd as *mut _);
        let mut class = [0u16; 128];
        let len = unsafe { GetClassNameW(hwnd, &mut class) };
        if len <= 0 {
            return None;
        }
        if String::from_utf16_lossy(&class[..len as usize]) != "Windows.UI.Core.CoreWindow" {
            return None;
        }
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        (!root.is_invalid() && root.0 as isize != self.hwnd).then(|| root.0 as isize)
    }
}
