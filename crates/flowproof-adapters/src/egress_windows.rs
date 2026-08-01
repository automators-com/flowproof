//! Windows egress containment. Still installs no WFP filter and launches
//! nothing; [`identity`] adds the per-run account the filters will be scoped
//! to.
//!
//! **Nothing here can produce a `Containment::Enforced`.** That is structural
//! rather than a convention: this module does not name that type at all. It
//! reports facts; `Containment::command_flow` turns them into a "not
//! contained" reason, and `enforced_is_unreachable_on_windows` in `egress`
//! asserts the tier. Claiming enforcement before a filter exists would let
//! `assert_no_egress` certify a run nothing was containing - the same false
//! green as #300 and #301 by a third route.
//!
//! Probing at all is about the REASON. Windows containment needs three things
//! a host can be missing, each of which fails as something else (see
//! `spike/windows-containment/LOG.md`): **Administrator** on every run, which
//! Linux does not require; **`SeAssignPrimaryTokenPrivilege`**, which an
//! Administrator can still lack, reporting `ERROR_PRIVILEGE_NOT_HELD` (1314)
//! as though it were a rights problem; and the **Secondary Logon service**
//! behind the fallback launcher. "Not contained" alone leaves an adopter
//! guessing which; naming it gives them a checklist.
//!
//! Net-event collection needs a WFP engine handle, so it is probed in the
//! step that opens one.

pub mod filters;
pub mod identity;
pub mod logon;
pub mod netevents;
pub mod spawn;
pub mod wfp;

use std::ffi::c_void;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};

/// UTF-16, NUL-terminated. Every Win32 `W` entry point wants this, and getting
/// the terminator wrong is a silent buffer overrun rather than a compile
/// error.
pub(crate) fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A Win32 failure with enough context to be diagnosed from a CI log alone.
///
/// The `api` name matters as much as the code: `FwpmFilterAdd0` returning
/// `ERROR_ACCESS_DENIED` and `LogonUserW` returning it mean completely
/// different things, and a bare error number cannot tell them apart. On a
/// platform where containment is new and every failure looks like every other
/// failure, that distinction is most of the diagnosis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinErr {
    pub api: &'static str,
    pub code: u32,
    pub context: String,
}

impl WinErr {
    pub fn new(api: &'static str, code: u32, context: impl Into<String>) -> Self {
        Self {
            api,
            code,
            context: context.into(),
        }
    }
}

impl std::fmt::Display for WinErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} failed: code={} (0x{:08x}) {}",
            self.api, self.code, self.code, self.context
        )
    }
}

impl std::error::Error for WinErr {}

/// What this host can and cannot do, measured rather than assumed.
///
/// `elevated` and `in_administrators` are only informative together: with UAC
/// disabled there is no split token, so `TokenIsElevated` reads false on a
/// token holding every administrative right, and a probe gating on the flag
/// alone would refuse the machine it was built for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostReadiness {
    pub elevated: bool,
    pub in_administrators: bool,
    /// HELD, not enabled - enabling mutates the token and is the launcher's
    /// job. Gates `CreateProcessAsUserW`, together with `increase_quota`.
    pub assign_primary_token: bool,
    pub increase_quota: bool,
    /// The service behind the `CreateProcessWithLogonW` fallback exists.
    pub secondary_logon: bool,
}

impl HostReadiness {
    /// Read-only: adjusts no token, opens no engine, creates no account.
    pub fn probe() -> Self {
        Self {
            elevated: is_elevated(),
            in_administrators: is_in_administrators(),
            assign_primary_token: holds_privilege("SeAssignPrimaryTokenPrivilege"),
            increase_quota: holds_privilege("SeIncreaseQuotaPrivilege"),
            secondary_logon: secondary_logon_present(),
        }
    }

    /// Administrator by either reading - see the type's note on why both.
    pub fn is_administrator(&self) -> bool {
        self.elevated || self.in_administrators
    }

    /// Either the privilege path or the Secondary Logon fallback will do.
    pub fn can_launch_as_identity(&self) -> bool {
        (self.assign_primary_token && self.increase_quota) || self.secondary_logon
    }

    /// What would stop this host enforcing containment, in fix order. Empty
    /// means ready.
    pub fn blockers(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !self.is_administrator() {
            out.push(
                "not running as Administrator (adding WFP filters requires it, on every run)"
                    .to_string(),
            );
        }
        if !self.can_launch_as_identity() {
            out.push(
                "no way to launch the agent under a per-run identity: the token holds neither \
                 SeAssignPrimaryTokenPrivilege nor SeIncreaseQuotaPrivilege, and the Secondary \
                 Logon service is not present for the CreateProcessWithLogonW fallback"
                    .to_string(),
            );
        }
        out
    }

    /// Compact rendering for the tier line.
    pub fn summary(&self) -> String {
        format!(
            "administrator={}, SeAssignPrimaryTokenPrivilege={}, SeIncreaseQuotaPrivilege={}, \
             secondary-logon={}",
            self.is_administrator(),
            self.assign_primary_token,
            self.increase_quota,
            self.secondary_logon,
        )
    }
}

/// Elevated administrator token?
fn is_elevated() -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elev as *mut _ as *mut c_void),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut len,
        )
        .is_ok();
        let _ = CloseHandle(token);
        ok && elev.TokenIsElevated != 0
    }
}

/// Member of the local Administrators group?
fn is_in_administrators() -> bool {
    use windows::Win32::Security::{
        AllocateAndInitializeSid, CheckTokenMembership, FreeSid, PSID, SECURITY_NT_AUTHORITY,
        SID_IDENTIFIER_AUTHORITY,
    };

    const SECURITY_BUILTIN_DOMAIN_RID: u32 = 0x20;
    const DOMAIN_ALIAS_RID_ADMINS: u32 = 0x220;

    unsafe {
        let auth: SID_IDENTIFIER_AUTHORITY = SECURITY_NT_AUTHORITY;
        let mut admins = PSID::default();
        if AllocateAndInitializeSid(
            &auth,
            2,
            SECURITY_BUILTIN_DOMAIN_RID,
            DOMAIN_ALIAS_RID_ADMINS,
            0,
            0,
            0,
            0,
            0,
            0,
            &mut admins,
        )
        .is_err()
        {
            return false;
        }
        let mut member = windows::core::BOOL(0);
        // A NULL token means "the calling thread's effective token".
        let ok = CheckTokenMembership(None, admins, &mut member).is_ok();
        FreeSid(admins);
        ok && member.as_bool()
    }
}

/// Does this token HOLD `name`?
///
/// Held, not enabled - the distinction most likely to be misread. A held
/// privilege stays disabled until something enables it, so testing for
/// *enabled* would report "missing" on a capable host; and enabling mutates
/// the token. So: enumerate `TokenPrivileges` and look the LUID up.
fn holds_privilege(name: &str) -> bool {
    use windows::Win32::Security::{
        GetTokenInformation, LookupPrivilegeValueW, TokenPrivileges, TOKEN_PRIVILEGES, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut luid = LUID::default();
        let wname = wide(name);
        if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wname.as_ptr()), &mut luid).is_err() {
            return false;
        }

        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }

        // Sized in two calls: the first fails with the length it wanted.
        let mut needed = 0u32;
        let _ = GetTokenInformation(token, TokenPrivileges, None, 0, &mut needed);
        if needed == 0 {
            let _ = CloseHandle(token);
            return false;
        }
        let mut buf = vec![0u8; needed as usize];
        let got = GetTokenInformation(
            token,
            TokenPrivileges,
            Some(buf.as_mut_ptr() as *mut c_void),
            needed,
            &mut needed,
        )
        .is_ok();
        let _ = CloseHandle(token);
        if !got {
            return false;
        }

        let tp = &*(buf.as_ptr() as *const TOKEN_PRIVILEGES);
        let count = tp.PrivilegeCount as usize;
        // `Privileges` is declared `[LUID_AND_ATTRIBUTES; 1]`; the real array
        // follows it in the buffer, which is why this is read by pointer.
        let first = tp.Privileges.as_ptr();
        (0..count).any(|i| {
            let entry = &*first.add(i);
            entry.Luid.LowPart == luid.LowPart && entry.Luid.HighPart == luid.HighPart
        })
    }
}

/// Does the Secondary Logon service exist? Presence, not state: a stopped
/// service can be started on demand, so only its absence is a finding - and
/// only a blocker when the privilege path is missing too.
fn secondary_logon_present() -> bool {
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_QUERY_STATUS,
    };

    unsafe {
        let Ok(scm) = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) else {
            return false;
        };
        let name = wide("seclogon");
        let found = match OpenServiceW(scm, PCWSTR(name.as_ptr()), SERVICE_QUERY_STATUS) {
            Ok(svc) => {
                let _ = CloseServiceHandle(svc);
                true
            }
            Err(_) => false,
        };
        let _ = CloseServiceHandle(scm);
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ready vs blocked, and the two launch routes. They are ALTERNATIVES -
    /// the spike's finding 3c.2 is a host with one and not the other - and
    /// half the privilege path is no route, since `CreateProcessAsUserW`
    /// needs both.
    #[test]
    fn readiness_separates_ready_from_blocked_and_route_from_half_route() {
        let ready = HostReadiness {
            elevated: true,
            in_administrators: true,
            assign_primary_token: true,
            increase_quota: true,
            secondary_logon: true,
        };
        assert!(ready.blockers().is_empty());

        // No admin is a blocker on its own, and the summary names every fact
        // so a support conversation reads one line instead of four questions.
        let no_admin = HostReadiness {
            elevated: false,
            in_administrators: false,
            ..ready.clone()
        };
        assert_eq!(no_admin.blockers().len(), 1);
        assert!(no_admin.blockers()[0].contains("Administrator"));
        let s = no_admin.summary();
        assert!(s.contains("administrator=false"), "{s}");
        assert!(s.contains("SeAssignPrimaryTokenPrivilege=true"), "{s}");
        assert!(s.contains("secondary-logon=true"), "{s}");

        // Either route alone is enough.
        let privilege_only = HostReadiness {
            secondary_logon: false,
            ..ready.clone()
        };
        assert!(privilege_only.can_launch_as_identity());
        assert!(privilege_only.blockers().is_empty());

        let fallback_only = HostReadiness {
            assign_primary_token: false,
            increase_quota: false,
            ..ready.clone()
        };
        assert!(fallback_only.can_launch_as_identity());
        assert!(fallback_only.blockers().is_empty());

        // Half the privilege path is not a route.
        let half = HostReadiness {
            increase_quota: false,
            secondary_logon: false,
            ..ready.clone()
        };
        assert!(!half.can_launch_as_identity());
        assert!(half.blockers()[0].contains("per-run identity"));
    }
}
