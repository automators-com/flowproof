//! Days 7–9 — the identity boundary. This is the spike.
//!
//! Days 1–6 test a mechanism the prior research pass already put at ~90%. The
//! real obstacle is elsewhere: a process running under a **per-run identity**
//! may simply be unable to drive a GUI application owned by a **different**
//! identity. Cross-identity UI Automation, window handles and `SendInput` are
//! refused across that boundary.
//!
//! That obstacle is identity-generic. Notepad exercises it exactly as well as
//! SAP GUI does, and a hosted runner has neither SAP GUI nor a Citrix client.
//!
//! **Notepad is a fair proxy for the identity boundary and a poor one for SAP.**
//! What it cannot speak to — SAP GUI's licensing, COM registration and profile
//! state under a freshly created local user — stays open, and the log says so
//! rather than folding it into a pass.
//!
//! Three questions, in order:
//!
//!   1. Can a process under the per-run identity drive a GUI app **it launched
//!      itself**? If no, the fused claim is dead and no amount of WFP work
//!      revives it.
//!   2. Can it drive a GUI app started by the **original** user? Expected NO.
//!      The exact failure is the constraint every later design lives with.
//!   3. Is a GUI app launched inside the boundary actually inside the
//!      containment scope?

use std::ffi::c_void;
use std::time::{Duration, Instant};

use uiautomation::UIAutomation;
use windows::core::PWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{GetTokenInformation, TokenUser, PSID, TOKEN_QUERY, TOKEN_USER};
use windows::Win32::System::Threading::{
    OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};

/// Everything the GUI probe prints is prefixed `GUI|` so it survives the trip
/// out through the child's redirected stdout and into the CI log.
fn emit(key: &str, value: impl std::fmt::Display) {
    println!("GUI|{key}|{value}");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Runs **inside** the boundary, as the per-run identity.
pub fn inside(args: &[String]) -> i32 {
    let mut launch_own = false;
    let mut foreign_title: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--launch-and-drive" => launch_own = true,
            "--foreign-title" => foreign_title = it.next().cloned(),
            _ => {}
        }
    }

    emit("PID", std::process::id());
    emit(
        "USERNAME",
        std::env::var("USERNAME").unwrap_or_else(|_| "<unset>".into()),
    );
    emit(
        "TOKEN-SID",
        token_sid_of(std::process::id()).unwrap_or_else(|| "<unknown>".into()),
    );

    let automation = match UIAutomation::new() {
        Ok(a) => {
            emit("UIA-INIT", "ok");
            a
        }
        Err(e) => {
            // A failure here is itself an answer to question 1, and a very
            // clean one: no UIA client, no driving anything.
            emit("UIA-INIT-FAILED", e);
            return 0;
        }
    };

    // The window list as this identity sees it. This is the single most
    // informative line in the whole stage: if the foreign window is simply
    // absent from the tree, that IS the answer to question 2, recorded
    // precisely rather than inferred from a failed click.
    dump_top_level(&automation, "before-launch");

    if launch_own {
        question_one(&automation);
    }

    if let Some(title) = foreign_title {
        question_two(&automation, &title);
    }

    emit("DONE", "gui-inside");
    0
}

/// Question 1 — launch a GUI app inside the boundary and drive it.
fn question_one(automation: &UIAutomation) {
    let child = std::process::Command::new("notepad.exe").spawn();
    let pid = match child {
        Ok(c) => {
            emit("Q1-LAUNCH", format!("notepad.exe pid={}", c.id()));
            c.id()
        }
        Err(e) => {
            emit("Q1-LAUNCH-FAILED", e);
            return;
        }
    };

    // Question 3's supporting evidence: WFP scopes containment by SID, so a
    // GUI app whose token carries the run identity's SID is inside the same
    // boundary by construction. Verified, not assumed.
    match token_sid_of(pid) {
        Some(s) => emit("Q3-GUI-APP-TOKEN-SID", s),
        None => emit("Q3-GUI-APP-TOKEN-SID", "<could not read>"),
    }

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut found = false;
    while Instant::now() < deadline {
        let matcher = automation
            .create_matcher()
            .depth(6)
            .timeout(1000)
            .contains_name("Notepad");
        if let Ok(w) = matcher.find_first() {
            emit(
                "Q1-WINDOW-FOUND",
                format!(
                    "name={:?} class={:?}",
                    w.get_name().unwrap_or_default(),
                    w.get_classname().unwrap_or_default()
                ),
            );
            found = true;

            // The editor is AutomationId 15 on classic Notepad — the same id
            // the shipping driver uses, so this is the app's real control and
            // not a lookalike.
            let edit = automation
                .create_matcher()
                .from_ref(&w)
                .depth(8)
                .timeout(2000)
                .filter_fn(Box::new(|e: &uiautomation::UIElement| {
                    Ok(e.get_automation_id()? == "15")
                }))
                .find_first();
            match edit {
                Ok(e) => {
                    emit("Q1-EDIT-FOUND", e.get_automation_id().unwrap_or_default());
                    let marker = "fp-spike-inside-boundary";
                    match e.send_text(marker, 10) {
                        Ok(()) => {
                            emit("Q1-SEND-TEXT", "ok");
                            // Read it back. Typing that "succeeds" and leaves
                            // an empty document is exactly the false green
                            // this codebase exists to prevent.
                            std::thread::sleep(Duration::from_millis(500));
                            match read_value(&e) {
                                Some(v) => {
                                    let ok = v.contains(marker);
                                    emit(
                                        "Q1-READBACK",
                                        format!("contains_marker={ok} value={v:?}"),
                                    );
                                }
                                None => emit("Q1-READBACK", "<could not read value>"),
                            }
                        }
                        Err(err) => emit("Q1-SEND-TEXT-FAILED", err),
                    }
                }
                Err(err) => emit("Q1-EDIT-NOT-FOUND", err),
            }
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    if !found {
        emit(
            "Q1-WINDOW-NOT-FOUND",
            "no window containing 'Notepad' within 30s",
        );
    }
    dump_top_level(automation, "after-own-launch");

    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output();
}

/// Question 2 — reach across the identity boundary to a window the original
/// user started. Expected to fail; the point is the exact failure.
fn question_two(automation: &UIAutomation, title: &str) {
    emit("Q2-TARGET-TITLE", title);
    let matcher = automation
        .create_matcher()
        .depth(6)
        .timeout(5000)
        .contains_name(title);
    match matcher.find_first() {
        Ok(w) => {
            emit(
                "Q2-WINDOW-FOUND",
                format!(
                    "name={:?} class={:?} - the boundary did NOT hide it",
                    w.get_name().unwrap_or_default(),
                    w.get_classname().unwrap_or_default()
                ),
            );
            let edit = automation
                .create_matcher()
                .from_ref(&w)
                .depth(8)
                .timeout(2000)
                .filter_fn(Box::new(|e: &uiautomation::UIElement| {
                    Ok(e.get_automation_id()? == "15")
                }))
                .find_first();
            match edit {
                Ok(e) => match e.send_text("fp-spike-cross-identity", 10) {
                    Ok(()) => {
                        std::thread::sleep(Duration::from_millis(500));
                        match read_value(&e) {
                            Some(v) => emit(
                                "Q2-SEND-TEXT",
                                format!(
                                    "ok, readback contains_marker={} value={v:?}",
                                    v.contains("fp-spike-cross-identity")
                                ),
                            ),
                            None => emit("Q2-SEND-TEXT", "ok, but value unreadable"),
                        }
                    }
                    Err(err) => emit("Q2-SEND-TEXT-FAILED", err),
                },
                Err(err) => emit("Q2-EDIT-NOT-FOUND", err),
            }
        }
        Err(err) => emit("Q2-WINDOW-NOT-FOUND", err),
    }
}

fn read_value(e: &uiautomation::UIElement) -> Option<String> {
    use uiautomation::patterns::UIValuePattern;
    if let Ok(p) = e.get_pattern::<UIValuePattern>() {
        if let Ok(v) = p.get_value() {
            return Some(v);
        }
    }
    e.get_name().ok()
}

fn dump_top_level(automation: &UIAutomation, when: &str) {
    match automation.get_root_element() {
        Ok(root) => {
            let matcher = automation
                .create_matcher()
                .from_ref(&root)
                .depth(1)
                .timeout(2000);
            match matcher.find_all() {
                Ok(all) => {
                    emit("WINDOW-LIST-COUNT", format!("{when} n={}", all.len()));
                    for e in all.iter().take(40) {
                        emit(
                            "WINDOW",
                            format!(
                                "{when} name={:?} class={:?} pid={:?}",
                                e.get_name().unwrap_or_default(),
                                e.get_classname().unwrap_or_default(),
                                e.get_process_id().unwrap_or(0)
                            ),
                        );
                    }
                }
                Err(e) => emit("WINDOW-LIST-FAILED", format!("{when} {e}")),
            }
        }
        Err(e) => emit("ROOT-ELEMENT-FAILED", format!("{when} {e}")),
    }
}

/// The user SID on a process's primary token.
pub fn token_sid_of(pid: u32) -> Option<String> {
    unsafe {
        let proc = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut token = HANDLE::default();
        if OpenProcessToken(proc, TOKEN_QUERY, &mut token).is_err() {
            let _ = CloseHandle(proc);
            return None;
        }
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        let ok = GetTokenInformation(
            token,
            TokenUser,
            Some(buf.as_mut_ptr() as *mut c_void),
            len,
            &mut len,
        )
        .is_ok();
        let out = if ok {
            let tu = &*(buf.as_ptr() as *const TOKEN_USER);
            let mut s = PWSTR::null();
            if ConvertSidToStringSidW(PSID(tu.User.Sid.0), &mut s).is_ok() {
                let v = s.to_string().ok();
                windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
                    s.0 as *mut c_void,
                )));
                v
            } else {
                None
            }
        } else {
            None
        };
        let _ = CloseHandle(token);
        let _ = CloseHandle(proc);
        out
    }
}
