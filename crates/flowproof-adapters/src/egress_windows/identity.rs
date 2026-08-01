//! The per-run identity: a throwaway local user the agent runs as.
//!
//! # Why a user, and not a process
//!
//! WFP has no PID condition. A filter can be scoped by app-id, user-id or
//! AppContainer SID, and **app-id is useless for an agent**: an agent that
//! spawns `python.exe` which spawns `curl.exe` walks straight out of an
//! app-id-scoped filter, and that escape is exactly what an agent does all
//! day. So the unit of containment has to be a *user*, created for the run
//! and deleted after it.
//!
//! The spike measured that this closes the escape - a grandchild through
//! `cmd.exe` was refused identically to the child (`core.grandchild.client`,
//! `spike/windows-containment/LOG.md`). It is the finding the whole Windows
//! design rests on.
//!
//! # Nothing here contains anything
//!
//! This module creates and deletes an account. It installs no filter, and an
//! agent run as this user is no more contained than one run as you - it is
//! merely *scopable*. `Containment::command_flow` still reports "not
//! contained", and the step that adds the WFP filters is the step that
//! changes that.

use std::ffi::c_void;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::NetManagement::{
    NetUserAdd, NetUserDel, UF_DONT_EXPIRE_PASSWD, UF_SCRIPT, USER_INFO_1, USER_PRIV_USER,
};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{LookupAccountNameW, PSID, SID_NAME_USE};

use super::{wide, WinErr};

/// `NERR_UserExists`: an earlier run died before its cleanup.
const NERR_USER_EXISTS: u32 = 2224;

/// A local account that exists for the length of one run.
///
/// Dropping it deletes the account. That is the only teardown that survives a
/// panic or an early return, and a leaked local user is a real (if small)
/// mess on a machine that is not a throwaway CI runner.
pub struct RunIdentity {
    pub name: String,
    pub password: String,
    /// Raw SID bytes, kept so the SID stays valid as long as the identity
    /// does. The WFP filter condition and the net-event lane both need it.
    pub sid_bytes: Vec<u8>,
    pub sid_string: String,
    created: bool,
}

impl RunIdentity {
    /// Create a fresh unprivileged local user.
    ///
    /// `USER_PRIV_USER` lands it in `Users` and nothing else - no
    /// Administrators, no Network Configuration Operators. That is not
    /// decoration: the spike found the raw-socket/promiscuous WFP filter
    /// unusable (it denied the contained identity every socket, declared
    /// included), and raw sockets are closed instead by this account being
    /// unprivileged. Sturdier, because it cannot be got wrong.
    pub fn create() -> Result<Self, WinErr> {
        // Local usernames cap at 20 characters. `fp-` plus 8 hex is 11, so
        // there is room, and the randomness is what makes two concurrent runs
        // on one host safe - a pid would collide across containers sharing a
        // host, and a fixed name would collide with itself.
        let name = format!("fp-{}", hex(&random_bytes(4)?));
        let password = random_password()?;

        let mut wname = wide(&name);
        let mut wpass = wide(&password);
        let mut wcomment = wide("flowproof per-run containment identity; safe to delete");

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

        let mut parm_err = 0u32;
        let mut rc = add_user(&info, &mut parm_err);

        // The name is random, so this is near-impossible - but "near" is not
        // "never", and the account is ours by prefix, unprivileged, and about
        // to be recreated identically. Failing a run because a previous run
        // crashed would be a worse outcome than reclaiming the name.
        if rc == NERR_USER_EXISTS {
            let w = wide(&name);
            unsafe { NetUserDel(PCWSTR::null(), PCWSTR(w.as_ptr())) };
            rc = add_user(&info, &mut parm_err);
        }

        if rc != 0 {
            return Err(WinErr::new(
                "NetUserAdd",
                rc,
                format!("parm_err={parm_err} -> {}", explain_neterr(rc)),
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

    /// The SID, as WFP and the net-event lane want it.
    pub fn psid(&self) -> PSID {
        PSID(self.sid_bytes.as_ptr() as *mut c_void)
    }

    /// Delete the account. Idempotent, so an explicit call and the `Drop` do
    /// not fight.
    pub fn delete(&mut self) -> Result<(), WinErr> {
        if !self.created {
            return Ok(());
        }
        let w = wide(&self.name);
        let rc = unsafe { NetUserDel(PCWSTR::null(), PCWSTR(w.as_ptr())) };
        self.created = false;
        if rc != 0 {
            return Err(WinErr::new("NetUserDel", rc, self.name.clone()));
        }
        Ok(())
    }
}

impl Drop for RunIdentity {
    fn drop(&mut self) {
        // Best effort, and deliberately silent: a failure here is reported by
        // the explicit `delete` on the success path, and there is nowhere
        // useful to send an error from a destructor.
        let _ = self.delete();
    }
}

fn add_user(info: &USER_INFO_1, parm_err: &mut u32) -> u32 {
    unsafe {
        NetUserAdd(
            PCWSTR::null(),
            1,
            info as *const USER_INFO_1 as *const u8,
            Some(parm_err),
        )
    }
}

/// Cryptographically random bytes from the system RNG.
///
/// The spike used a hard-coded password and said, in as many words, that
/// anything shipping must not. This is that: an account that exists for one
/// run on a machine an adopter cares about does not get a credential from a
/// source that is in the git history.
fn random_bytes(n: usize) -> Result<Vec<u8>, WinErr> {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    let mut buf = vec![0u8; n];
    let status = unsafe { BCryptGenRandom(None, &mut buf, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if status.is_err() {
        return Err(WinErr::new(
            "BCryptGenRandom",
            status.0 as u32,
            format!("wanted {n} bytes"),
        ));
    }
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A random password that satisfies Windows complexity without tripping the
/// rule that eats the obvious implementations.
///
/// Complexity rejects a password containing any token of the USERNAME three
/// characters or longer, splitting the name on delimiters. The spike lost a
/// full CI cycle to this: `fp-spk-9024-core` with password `Fp!Spk-9024-core`
/// failed as `NERR_PasswordTooShort` (2245) - an error naming length about a
/// sixteen-character password. Deriving the password from the name in ANY way
/// is the trap; this shares nothing with it by construction.
fn random_password() -> Result<String, WinErr> {
    const LOWER: &[u8] = b"abcdefghijkmnopqrstuvwxyz";
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const DIGIT: &[u8] = b"23456789";
    const SYMBOL: &[u8] = b"!@#$%^&*()-_=+[]{}";

    // One from each class first, so complexity is satisfied by construction
    // rather than by luck; the rest from the union.
    let classes: [&[u8]; 4] = [LOWER, UPPER, DIGIT, SYMBOL];
    let all: Vec<u8> = classes.concat();
    let raw = random_bytes(28)?;

    let mut out = Vec::with_capacity(28);
    for (i, class) in classes.iter().enumerate() {
        out.push(class[raw[i] as usize % class.len()]);
    }
    for byte in &raw[classes.len()..] {
        out.push(all[*byte as usize % all.len()]);
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
}

fn lookup_sid(name: &str) -> Result<(Vec<u8>, String), WinErr> {
    let wname = wide(name);
    let mut sid_len = 0u32;
    let mut dom_len = 0u32;
    let mut use_ = SID_NAME_USE(0);

    // The first call sizes the buffers and is EXPECTED to fail with
    // ERROR_INSUFFICIENT_BUFFER, so its error is not checked - only the size
    // it reported back.
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
        let code = if e == ERROR_SUCCESS { 0 } else { e.0 };
        return Err(WinErr::new("LookupAccountNameW(size)", code, name));
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
    .map_err(|e| WinErr::new("LookupAccountNameW", e.code().0 as u32, name))?;

    let mut sid_str = PWSTR::null();
    unsafe { ConvertSidToStringSidW(PSID(sid.as_ptr() as *mut c_void), &mut sid_str) }
        .map_err(|e| WinErr::new("ConvertSidToStringSidW", e.code().0 as u32, name))?;
    let s = unsafe { sid_str.to_string() }.unwrap_or_default();
    unsafe {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            sid_str.0 as *mut c_void,
        )))
    };

    Ok((sid, s))
}

/// The `NetUserAdd` codes this can actually hit, in words.
///
/// Recorded rather than looked up each time, because the two that matter both
/// name the wrong thing: 2245 says "too short" about a long password that
/// merely resembled the username, and 2224 says "exists" about an account a
/// previous run failed to clean up.
fn explain_neterr(rc: u32) -> &'static str {
    match rc {
        5 => "ERROR_ACCESS_DENIED: creating a local user needs Administrator",
        2224 => "NERR_UserExists",
        2245 => "NERR_PasswordTooShort: also fires when the password resembles the username",
        2202 => "NERR_BadUsername",
        _ => "see NetUserAdd return codes",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The username has to fit Windows' 20-character local-account limit, and
    /// two concurrent runs on one host must not collide.
    #[test]
    fn the_generated_name_is_short_and_unique() {
        let a = format!("fp-{}", hex(&random_bytes(4).expect("rng")));
        let b = format!("fp-{}", hex(&random_bytes(4).expect("rng")));
        assert!(a.len() <= 20, "{a} is longer than a local username may be");
        assert_eq!(a.len(), 11);
        assert_ne!(a, b, "two runs on one host must not collide");
    }

    /// Complexity is satisfied by construction, not by luck - and the password
    /// shares nothing with the account name, which is the rule that ate a CI
    /// cycle in the spike.
    #[test]
    fn the_password_meets_complexity_by_construction() {
        for _ in 0..32 {
            let p = random_password().expect("rng");
            assert_eq!(p.len(), 28);
            assert!(p.chars().any(|c| c.is_ascii_lowercase()), "{p}");
            assert!(p.chars().any(|c| c.is_ascii_uppercase()), "{p}");
            assert!(p.chars().any(|c| c.is_ascii_digit()), "{p}");
            assert!(p.chars().any(|c| !c.is_ascii_alphanumeric()), "{p}");
            assert!(!p.contains("fp-"), "must share no token with the name: {p}");
        }
    }

    /// Two passwords are not the same password. A generator that returned a
    /// constant would pass every assertion above.
    #[test]
    fn passwords_are_not_reused() {
        let a = random_password().expect("rng");
        let b = random_password().expect("rng");
        assert_ne!(a, b);
    }
}
