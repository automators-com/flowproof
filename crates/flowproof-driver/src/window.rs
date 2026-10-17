//! Top-level window discovery for pixels-only driving: find a window by
//! title substring, bring it to the foreground, and read its screen
//! rectangle — the whole "attach" story for a Citrix/RDP/vision target.

#![cfg(windows)]

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, RECT};
use windows::Win32::Graphics::Dwm::{DwmGetWindowAttribute, DWMWA_EXTENDED_FRAME_BOUNDS};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    keybd_event, KEYBD_EVENT_FLAGS, KEYEVENTF_KEYUP, VK_MENU,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, EnumWindows, GetAncestor, GetClassNameW, GetForegroundWindow, GetWindowRect,
    GetWindowTextW, GetWindowThreadProcessId, IsIconic, IsWindowVisible, SetForegroundWindow,
    SetWindowPos, ShowWindow, GA_ROOT, SWP_NOACTIVATE, SWP_NOZORDER, SW_RESTORE,
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

fn window_process(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// The title of whatever window holds the foreground now.
pub fn foreground_title() -> String {
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(GetForegroundWindow(), &mut buf) };
    if len <= 0 {
        return "an untitled window".into();
    }
    String::from_utf16_lossy(&buf[..len as usize])
}

/// The window's rectangle as it is drawn. `GetWindowRect` includes the
/// invisible resize border Windows 10 and 11 put around a window - 7px or
/// so each side - so a capture of it took in a strip of whatever stood
/// behind, and OCR read stray letters there: a `C` on Calculator's left
/// edge that was not its `C` key. DWM knows the visible frame; a window
/// it does not compose falls back to the plain rectangle.
fn visible_rect(hwnd: HWND, title: &str) -> Result<RECT, DriverError> {
    let mut rect = RECT::default();
    let framed = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut rect as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        )
    };
    if framed.is_ok() && rect.right > rect.left && rect.bottom > rect.top {
        return Ok(rect);
    }
    unsafe { GetWindowRect(hwnd, &mut rect) }
        .map_err(|e| DriverError::Uia(format!("reading bounds of '{title}': {e}")))?;
    Ok(rect)
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
    let rect = visible_rect(HWND(hwnd as *mut _), &title)?;
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
    /// The native window handle, for APIs that address a window by it (UIA).
    pub fn hwnd(&self) -> isize {
        self.hwnd
    }

    /// Bring the window to the foreground (restoring it if minimized) so
    /// capture and injected input reach it, and confirm it got there.
    ///
    /// Windows refuses `SetForegroundWindow` to a process the user is not
    /// working in - which is flowproof whenever it is started from a
    /// terminal or an IDE. The refusal used to be ignored: a vision run
    /// against Calculator, sitting behind the window flowproof was started
    /// from, OCR'd that window's pixels and clicked into it, and reported
    /// "5" as not on screen. A synthetic Alt tap lifts the foreground lock
    /// (the system lets a pressed Alt switch windows), which is how
    /// automation tools get past it; a window still not in front after
    /// that is an error naming what covers it.
    pub fn focus(&self) -> Result<(), DriverError> {
        let hwnd = HWND(self.hwnd as *mut _);
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
        }
        if self.holds_foreground() {
            return Ok(());
        }
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        if self.await_foreground(Duration::from_millis(300)) {
            return Ok(());
        }
        unsafe {
            keybd_event(VK_MENU.0 as u8, 0, KEYBD_EVENT_FLAGS(0), 0);
            keybd_event(VK_MENU.0 as u8, 0, KEYEVENTF_KEYUP, 0);
            let _ = SetForegroundWindow(hwnd);
            let _ = BringWindowToTop(hwnd);
        }
        if self.await_foreground(Duration::from_secs(1)) {
            return Ok(());
        }
        Err(DriverError::Uia(format!(
            "could not bring '{}' to the front: '{}' holds the foreground. Capture and \
             input would reach that window instead - click '{}' once, or close what \
             covers it, and run again",
            self.title,
            foreground_title(),
            self.title
        )))
    }

    /// Whether the foreground window is this one, inside it, or belongs to
    /// its process. A process, not just a handle: an app's own dialogs,
    /// popups and context menus are separate top-level windows, and taking
    /// the foreground back from one of them would close a menu a step had
    /// just opened. Inside it too: a UWP app's frame belongs to
    /// ApplicationFrameHost while the content that takes focus on a click
    /// belongs to the app's own process.
    pub fn holds_foreground(&self) -> bool {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.is_invalid() {
            return false;
        }
        let own = HWND(self.hwnd as *mut _);
        let root = unsafe { GetAncestor(foreground, GA_ROOT) };
        foreground == own || root == own || window_process(foreground) == window_process(own)
    }

    fn await_foreground(&self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            if self.holds_foreground() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Re-read the window's current screen rectangle, as it is drawn.
    pub fn refresh_rect(&mut self) -> Result<PixelRect, DriverError> {
        let rect = visible_rect(HWND(self.hwnd as *mut _), &self.title)?;
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
