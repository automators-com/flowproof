//! Timestamped keypress capture for keys that change the screen but
//! leave no visible trace of their own — Enter, Tab, Escape, function
//! keys. A screen recording alone can never reveal *that* one of these
//! was pressed, only its downstream effect; this gives a video-authoring
//! pipeline the actual event as a logged fact instead of something to
//! guess at from pixels.
//!
//! A global low-level keyboard hook (`WH_KEYBOARD_LL`), not a
//! `GetAsyncKeyState` poll: polling was tried first (simpler, no message
//! pump needed) and failed to see keystrokes at all when the guest OS is
//! a VM — whatever the hypervisor's input path is, it did not update the
//! table `GetAsyncKeyState` reads, even for a real physical keypress.
//! `WH_KEYBOARD_LL` sits earlier in the input pipeline and is documented
//! to see every keystroke regardless of source.
//!
//! CORRECTNESS AND SAFETY: this hook sits in the path of every keystroke
//! on the machine for as long as it is installed. It must never swallow
//! one — every code path calls `CallNextHookEx` exactly once, including
//! the negative-`code` case the Win32 contract requires passing through
//! untouched, and the hook callback can never panic (no unwrap, no
//! indexing, nothing that unwinds across an `extern "system"` boundary).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::DriverError;

/// One logged keypress, timestamped from the start of capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyEvent {
    pub offset_ms: u64,
    pub key: String,
}

/// Keys worth logging: the ones a click never lands on. Matches SAP's
/// own scripting VKey table (Enter, F1-F12) for a familiar vocabulary.
const MONITORED_KEYS: &[(i32, &str)] = &[
    (0x0D, "Enter"),
    (0x09, "Tab"),
    (0x1B, "Escape"),
    (0x70, "F1"),
    (0x71, "F2"),
    (0x72, "F3"),
    (0x73, "F4"),
    (0x74, "F5"),
    (0x75, "F6"),
    (0x76, "F7"),
    (0x77, "F8"),
    (0x78, "F9"),
    (0x79, "F10"),
    (0x7A, "F11"),
    (0x7B, "F12"),
];

/// Install the hook, pump messages for `duration` so it actually
/// receives events, then uninstall it — returning every monitored
/// keydown in chronological order.
#[cfg(windows)]
pub fn capture_for(duration: Duration) -> Result<Vec<KeyEvent>, DriverError> {
    win_hook::capture_for(duration)
}

#[cfg(not(windows))]
pub fn capture_for(_duration: Duration) -> Result<Vec<KeyEvent>, DriverError> {
    Err(DriverError::UnsupportedPlatform)
}

#[cfg(windows)]
mod win_hook {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, PeekMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, PM_REMOVE, WH_KEYBOARD_LL, WM_KEYDOWN,
        WM_SYSKEYDOWN,
    };

    use super::{KeyEvent, MONITORED_KEYS};
    use crate::DriverError;

    // Not Send/Sync, and only ever touched from the thread that installed
    // the hook — a low-level hook's callback always runs on that same
    // thread, so a thread_local is the right (and only simple) way to
    // get data out of a plain `extern "system"` function pointer.
    thread_local! {
        static STATE: RefCell<Option<(Instant, Vec<KeyEvent>)>> = const { RefCell::new(None) };
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // The Win32 contract for every hook procedure: a negative code
        // means "not yours to look at", pass it on immediately.
        if code >= 0 && (wparam.0 as u32 == WM_KEYDOWN || wparam.0 as u32 == WM_SYSKEYDOWN) {
            // Safe per the hook contract: for HC_ACTION, lparam always
            // points to a valid KBDLLHOOKSTRUCT for the call's duration.
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let vk = info.vkCode as i32;
            if let Some((_, name)) = MONITORED_KEYS.iter().find(|(k, _)| *k == vk) {
                let _ = STATE.try_with(|state| {
                    if let Ok(mut state) = state.try_borrow_mut() {
                        if let Some((start, events)) = state.as_mut() {
                            events.push(KeyEvent {
                                offset_ms: start.elapsed().as_millis() as u64,
                                key: (*name).to_string(),
                            });
                        }
                    }
                });
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    pub fn capture_for(duration: Duration) -> Result<Vec<KeyEvent>, DriverError> {
        STATE.with(|state| *state.borrow_mut() = Some((Instant::now(), Vec::new())));

        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
            .map_err(|e| DriverError::Input(format!("installing keyboard hook: {e}")))?;

        // A low-level hook only receives events while its installing
        // thread pumps messages, so this loop IS the capture window.
        let deadline = Instant::now() + duration;
        let mut msg = MSG::default();
        while Instant::now() < deadline {
            while unsafe { PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        // Best-effort: an unhook failure here leaks the hook until
        // process exit, which still tears it down — worth logging, not
        // worth failing an otherwise-successful capture over.
        if let Err(e) = unsafe { UnhookWindowsHookEx(hook) } {
            eprintln!("input_log: failed to remove keyboard hook cleanly: {e}");
        }

        Ok(STATE.with(|state| {
            state
                .borrow_mut()
                .take()
                .map(|(_, events)| events)
                .unwrap_or_default()
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_event_round_trips_through_json() {
        let event = KeyEvent {
            offset_ms: 1234,
            key: "Enter".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serializes");
        let back: KeyEvent = serde_json::from_str(&json).expect("parses");
        assert_eq!(event, back);
    }

    #[test]
    #[cfg(not(windows))]
    fn non_windows_reports_unsupported_rather_than_capturing_nothing_silently() {
        let err = capture_for(Duration::from_millis(1)).expect_err("stub must refuse");
        assert!(matches!(err, DriverError::UnsupportedPlatform));
    }
}
