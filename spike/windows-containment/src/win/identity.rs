//! The per-run identity.
//!
//! WFP has no PID condition — only app-id, user-id or AppContainer SID. App-id
//! is unusable for an agent, because an agent that spawns `python.exe` which
//! spawns `curl.exe` walks straight out of an app-id scoped filter. So the unit
//! of containment has to be a *user*, created for the run and deleted after it.

use std::ffi::c_void;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_SUCCESS, HANDLE};
use windows::Win32::NetworkManagement::NetManagement::{
    NetUserAdd, NetUserDel, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1, USER_PRIV_USER,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};

use super::{wide, WinErr};

pub struct RunIdentity {
    pub name: String,
    pub password: String,
    /// Raw SID bytes, kept so the SID stays valid for as long as the identity
    /// does. WFP conditions and the net-event lane both need it.
    pub sid_bytes: Vec<u8>,
    pub sid_string: String,
    created: bool,
}

impl RunIdentity {
    /// Create a fresh unprivileged local user. `NetUserAdd` with
    /// `USER_PRIV_USER` lands it in `Users` and nothing else — no
    /// Administrators, no Network Configuration Operators.
    pub fn create(tag: &str) -> Result<Self, WinErr> {
        // Uniqueness without a random source: the run's own pid plus the tick
        // count. A collision would only matter if two spikes ran in the same
        // millisecond in the same process, which cannot happen.
        let name = format!("fp-spk-{}-{}", std::process::id() % 100000, tag);
        // Local password policy on a hosted runner is default (no complexity
        // requirement), but satisfy complexity anyway so this behaves the same
        // on a domain-joined box.
        let password = format!("Fp!Spk-{}-{}", std::process::id(), tag);

        let mut wname = wide(&name);
        let mut wpass = wide(&password);
        let mut wcomment = wide("flowproof Windows containment spike; safe to delete");

        let info = USER_INFO_1 {
            usri1_name: PWSTR(wname.as_mut_ptr()),
            usri1_password: PWSTR(wpass.as_mut_ptr()),
            usri1_password_age: 0,
            usri1_priv: USER_PRIV_USER,
            usri1_home_dir: PWSTR::null(),
            usri1_comment: PWSTR(wcomment.as_mut_ptr()),
            usri1_flags: UF_SCRIPT | UF_DONT_EXPIRE_PASSWD,
            usri1_script_path: PWSTR::null(),
        };

        let mut parm_err: u32 = 0;
        let rc = unsafe {
            NetUserAdd(
                PCWSTR::null(),
                1,
                &info as *const USER_INFO_1 as *const u8,
                Some(&mut parm_err),
            )
        };
        if rc != 0 {
            return Err(WinErr::new(
                "NetUserAdd",
                rc,
                format!("parm_err={parm_err} name={name}"),
            ));
        }

        let (sid_bytes, sid_string) = lookup_sid(&name)?;
        Ok(Self {
            name,
            password,
            sid_bytes,
            sid_string,
            created: true,
        })
    }

    pub fn psid(&self) -> PSID {
        PSID(self.sid_bytes.as_ptr() as *mut c_void)
    }

    pub fn delete(&mut self) -> Result<(), WinErr> {
        if !self.created {
            return Ok(());
        }
        let wname = wide(&self.name);
        let rc = unsafe { NetUserDel(PCWSTR::null(), PCWSTR(wname.as_ptr())) };
        self.created = false;
        if rc != 0 {
            return Err(WinErr::new("NetUserDel", rc, self.name.clone()));
        }
        Ok(())
    }
}

impl Drop for RunIdentity {
    fn drop(&mut self) {
        // Best effort. The harness deletes explicitly and reports the result;
        // this only catches an early return. A leaked local user on a
        // throwaway CI runner is harmless, but leaking one is still worth not
        // doing.
        let _ = self.delete();
    }
}

fn lookup_sid(name: &str) -> Result<(Vec<u8>, String), WinErr> {
    let wname = wide(name);
    let mut sid_len: u32 = 0;
    let mut dom_len: u32 = 0;
    let mut use_: SID_NAME_USE = SID_NAME_USE(0);

    // First call sizes the buffers; it is expected to fail with
    // ERROR_INSUFFICIENT_BUFFER, so its error is not checked.
    let _ = unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(wname.as_ptr()),
            None,
            &mut sid_len,
            None,
            &mut dom_len,
            &mut use_,
        )
    };
    if sid_len == 0 {
        let e = unsafe { GetLastError() };
        return Err(WinErr::new(
            "LookupAccountNameW(size)",
            e.0,
            name.to_string(),
        ));
    }

    let mut sid = vec![0u8; sid_len as usize];
    let mut dom = vec![0u16; dom_len.max(1) as usize];
    unsafe {
        LookupAccountNameW(
            PCWSTR::null(),
            PCWSTR(wname.as_ptr()),
            Some(PSID(sid.as_mut_ptr() as *mut c_void)),
            &mut sid_len,
            Some(PWSTR(dom.as_mut_ptr())),
            &mut dom_len,
            &mut use_,
        )
    }
    .map_err(|e| WinErr::new("LookupAccountNameW", e.code().0 as u32, name.to_string()))?;

    let mut sid_str = PWSTR::null();
    unsafe { ConvertSidToStringSidW(PSID(sid.as_ptr() as *mut c_void), &mut sid_str) }
        .map_err(|e| WinErr::new("ConvertSidToStringSidW", e.code().0 as u32, String::new()))?;
    let s = unsafe { sid_str.to_string() }.unwrap_or_default();
    unsafe {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            sid_str.0 as *mut c_void,
        )))
    };

    Ok((sid, s))
}

/// Is this process running with an elevated administrator token?
///
/// Reported, never assumed. The claim "requires Administrator" has to be said
/// in the same breath as the containment claim, and a run that quietly had no
/// admin would otherwise look like a mechanism failure.
pub fn is_elevated() -> bool {
    use windows::Win32::Foundation::CloseHandle;
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

pub fn last_error() -> u32 {
    let e = unsafe { GetLastError() };
    if e == ERROR_SUCCESS {
        0
    } else {
        e.0
    }
}
