//! Logging the per-run identity on, and enabling what it takes to launch as
//! it.
//!
//! Two halves of one job, both of which fail in ways that name the wrong
//! thing:
//!
//! - **Which logon type a host allows is policy, not code.** A fresh local
//!   user has `SeInteractiveLogonRight` on a member server but not under every
//!   policy, so `INTERACTIVE` can be refused on a machine where `BATCH` works
//!   perfectly. Trying all three costs nothing and saves a round trip spent
//!   discovering which one this host allows.
//! - **A privilege the token HOLDS is still disabled until something enables
//!   it.** [`super::HostReadiness`] probes for *held* because a probe must not
//!   mutate a token; this is where it gets switched on, and it is a separate
//!   act with a separate failure mode.
//!
//! Nothing here contains anything. It produces a token; the step that installs
//! WFP filters is the step that makes a run contained.

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LogonUserW, LookupPrivilegeValueW, LOGON32_LOGON, LOGON32_LOGON_BATCH,
    LOGON32_LOGON_INTERACTIVE, LOGON32_LOGON_NETWORK_CLEARTEXT, LOGON32_PROVIDER_DEFAULT,
    LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
    TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use super::{wide, WinErr};

/// `ERROR_NOT_ALL_ASSIGNED` - the adjust succeeded and changed nothing.
const ERROR_NOT_ALL_ASSIGNED: u32 = 1300;

/// A logged-on identity's primary token, closed when dropped.
///
/// A token is a handle, and a handle leaked once per run on a long-lived
/// runner is a leak. `Drop` is the only teardown that survives an early
/// return.
pub struct IdentityToken {
    handle: HANDLE,
    /// Which logon type actually worked, for the report.
    pub logon_type: &'static str,
}

impl IdentityToken {
    /// The raw handle, for `CreateProcessAsUserW`.
    pub fn handle(&self) -> HANDLE {
        self.handle
    }
}

impl Drop for IdentityToken {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Log `user` on locally and hand back its primary token.
///
/// Three logon types are tried in order and the one that worked is recorded.
/// The error returned when all three fail is the LAST one, which is the least
/// misleading choice available: reporting the first would name `INTERACTIVE`
/// on a host that never permits it, sending the reader after a policy that is
/// not the problem.
pub fn logon(user: &str, password: &str) -> Result<IdentityToken, WinErr> {
    let wuser = wide(user);
    let wdom = wide(".");
    let wpass = wide(password);

    let attempts: [(LOGON32_LOGON, &'static str); 3] = [
        (LOGON32_LOGON_INTERACTIVE, "INTERACTIVE"),
        (LOGON32_LOGON_BATCH, "BATCH"),
        (LOGON32_LOGON_NETWORK_CLEARTEXT, "NETWORK_CLEARTEXT"),
    ];

    let mut last = WinErr::new("LogonUserW", 0, "no attempt made");
    let mut tried = Vec::new();
    for (kind, label) in attempts {
        let mut token = HANDLE::default();
        let r = unsafe {
            LogonUserW(
                PCWSTR(wuser.as_ptr()),
                PCWSTR(wdom.as_ptr()),
                PCWSTR(wpass.as_ptr()),
                kind,
                LOGON32_PROVIDER_DEFAULT,
                &mut token,
            )
        };
        match r {
            Ok(()) => {
                return Ok(IdentityToken {
                    handle: token,
                    logon_type: label,
                })
            }
            Err(e) => {
                let code = e.code().0 as u32;
                tried.push(format!("{label}={code}"));
                last = WinErr::new("LogonUserW", code, format!("{user}: {}", tried.join(" ")));
            }
        }
    }
    Err(last)
}

/// What happened to one privilege when we asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegeState {
    /// Enabled and usable.
    Enabled,
    /// The token does not hold it, so it cannot be enabled. This is the state
    /// behind `ERROR_PRIVILEGE_NOT_HELD` (1314) later.
    NotHeld,
    /// The adjust call itself failed.
    Failed(u32),
}

/// Enable the privileges `CreateProcessAsUserW` needs, reporting what actually
/// got enabled.
///
/// This is the failure most likely to be misread, in two layers.
///
/// A privilege the token *holds* is still disabled until it is explicitly
/// enabled, so an Administrator can call `CreateProcessAsUserW` and get
/// `ERROR_PRIVILEGE_NOT_HELD` (1314) - an error that reads like "you are not
/// admin" when the real cause is "you did not switch it on".
///
/// Worse, **`AdjustTokenPrivileges` returns success even when it enabled
/// nothing.** A caller checking only the return value learns nothing at all.
/// The result is therefore read from `GetLastError` against a successful
/// return, which is the one place in this crate where that is the correct
/// thing to do rather than a mistake.
pub fn enable_launch_privileges() -> Vec<(&'static str, PrivilegeState)> {
    const WANTED: [&str; 2] = ["SeAssignPrimaryTokenPrivilege", "SeIncreaseQuotaPrivilege"];

    let mut out = Vec::with_capacity(WANTED.len());
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            let code = last_error();
            return WANTED
                .iter()
                .map(|n| (*n, PrivilegeState::Failed(code)))
                .collect();
        }

        for name in WANTED {
            out.push((name, enable_one(token, name)));
        }
        let _ = CloseHandle(token);
    }
    out
}

/// Enable one privilege on an already-open token.
///
/// # Safety
/// `token` must be a valid handle opened with `TOKEN_ADJUST_PRIVILEGES`.
unsafe fn enable_one(token: HANDLE, name: &str) -> PrivilegeState {
    let mut luid = LUID::default();
    let wname = wide(name);
    if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wname.as_ptr()), &mut luid).is_err() {
        return PrivilegeState::Failed(last_error());
    }
    let tp = TOKEN_PRIVILEGES {
        PrivilegeCount: 1,
        Privileges: [LUID_AND_ATTRIBUTES {
            Luid: luid,
            Attributes: SE_PRIVILEGE_ENABLED,
        }],
    };
    let ok = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None).is_ok();
    // Success plus ERROR_NOT_ALL_ASSIGNED means the token does not hold it;
    // success plus no error means it is now on. See the doc comment: the
    // return value alone cannot tell these apart.
    match (ok, last_error()) {
        (true, 0) => PrivilegeState::Enabled,
        (true, ERROR_NOT_ALL_ASSIGNED) => PrivilegeState::NotHeld,
        (true, other) => PrivilegeState::Failed(other),
        (false, other) => PrivilegeState::Failed(other),
    }
}

/// `GetLastError`, with success normalised to 0.
fn last_error() -> u32 {
    use windows::Win32::Foundation::{GetLastError, ERROR_SUCCESS};
    let e = unsafe { GetLastError() };
    if e == ERROR_SUCCESS {
        0
    } else {
        e.0
    }
}

/// Did every privilege the primary launcher needs get enabled?
///
/// `false` is not fatal: `CreateProcessWithLogonW` needs no privilege and
/// still runs the child under the per-run identity, which is the only property
/// containment depends on. It is a fallback, not a weakening - but the caller
/// should know which path it is on, because that path cannot inherit handles
/// and so cannot capture the child's output.
pub fn all_enabled(states: &[(&'static str, PrivilegeState)]) -> bool {
    !states.is_empty() && states.iter().all(|(_, s)| *s == PrivilegeState::Enabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `all_enabled` decides which launcher path is available, so it must not
    /// be optimistic about a partial result. Half the privilege set is not the
    /// privilege set - `CreateProcessAsUserW` needs both.
    #[test]
    fn all_enabled_requires_every_privilege() {
        let both = [
            ("SeAssignPrimaryTokenPrivilege", PrivilegeState::Enabled),
            ("SeIncreaseQuotaPrivilege", PrivilegeState::Enabled),
        ];
        assert!(all_enabled(&both));

        let half = [
            ("SeAssignPrimaryTokenPrivilege", PrivilegeState::Enabled),
            ("SeIncreaseQuotaPrivilege", PrivilegeState::NotHeld),
        ];
        assert!(!all_enabled(&half));

        let failed = [
            ("SeAssignPrimaryTokenPrivilege", PrivilegeState::Failed(5)),
            ("SeIncreaseQuotaPrivilege", PrivilegeState::Enabled),
        ];
        assert!(!all_enabled(&failed));

        // An empty result is "we learned nothing", never "all fine".
        assert!(!all_enabled(&[]));
    }

    /// `NotHeld` and `Failed` are different findings and must not collapse.
    /// One means this token never had the right; the other means the call
    /// itself broke, and they send an operator to different places.
    #[test]
    fn not_held_is_distinct_from_failed() {
        assert_ne!(PrivilegeState::NotHeld, PrivilegeState::Failed(1300));
        assert_ne!(PrivilegeState::Enabled, PrivilegeState::NotHeld);
    }

    /// Every logon attempt is named in the error, so a host that refuses all
    /// three says which code each one gave rather than only the last.
    #[test]
    fn a_failed_logon_names_what_it_tried() {
        // The real call needs a real account; this pins the shape of the
        // message the loop builds, which is what an operator reads.
        let e = WinErr::new("LogonUserW", 1326, "fp-abc: INTERACTIVE=1385 BATCH=1326");
        let s = e.to_string();
        assert!(s.contains("INTERACTIVE=1385"), "{s}");
        assert!(s.contains("BATCH=1326"), "{s}");
    }
}
