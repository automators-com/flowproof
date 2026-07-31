//! Getting a child process to actually run as the per-run identity, inside a
//! job object.
//!
//! Two things here are easy to get wrong and expensive to discover on a CI
//! round trip, so both are done up front and reported:
//!
//!   1. `CreateProcessAsUserW` needs the target identity to have access to the
//!      window station and desktop. Without that grant the child either fails
//!      to start or starts and dies at CSRSS connect — and on a console app the
//!      symptom looks like a crash, not a permissions problem.
//!   2. The job object exists so a grandchild cannot outlive the run. It is
//!      *not* what contains the network: WFP has no PID condition, which is the
//!      whole reason the identity exists.

use std::ffi::c_void;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::LUID;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, SET_ACCESS,
    SE_WINDOW_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LogonUserW, LookupPrivilegeValueW, LOGON32_LOGON, LOGON32_LOGON_BATCH,
    LOGON32_LOGON_INTERACTIVE, LOGON32_LOGON_NETWORK_CLEARTEXT, LOGON32_PROVIDER_DEFAULT,
    LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
};
use windows::Win32::Security::{
    ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, GetProcessWindowStation, GetThreadDesktop, OpenDesktopW,
    OpenWindowStationW, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessWithLogonW, GetCurrentProcess, GetCurrentThreadId,
    OpenProcessToken, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    LOGON_WITH_PROFILE, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
};

use super::{wide, WinErr};

pub struct Contained {
    pub process: HANDLE,
    pub thread: HANDLE,
    pub pid: u32,
    pub job: HANDLE,
    pub token: HANDLE,
}

impl Contained {
    pub fn close(&mut self) {
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
            // Closing the job kills anything still inside it, which is the
            // point of KILL_ON_JOB_CLOSE.
            let _ = CloseHandle(self.job);
            let _ = CloseHandle(self.token);
        }
    }
}

/// Log the identity on and hand back its primary token.
///
/// Three logon types are tried in order and the one that worked is reported.
/// `INTERACTIVE` needs `SeInteractiveLogonRight`, which a fresh local user has
/// by default on a member server but not on every policy; `BATCH` is the usual
/// fallback for a non-login service identity. Trying all three in one run costs
/// nothing and saves a CI cycle spent discovering which the runner allows.
pub fn logon(user: &str, password: &str) -> Result<(HANDLE, &'static str), WinErr> {
    let wuser = wide(user);
    let wdom = wide(".");
    let wpass = wide(password);

    let attempts: [(LOGON32_LOGON, &'static str); 3] = [
        (LOGON32_LOGON_INTERACTIVE, "INTERACTIVE"),
        (LOGON32_LOGON_BATCH, "BATCH"),
        (LOGON32_LOGON_NETWORK_CLEARTEXT, "NETWORK_CLEARTEXT"),
    ];
    let mut last = WinErr::new("LogonUserW", 0, "no attempt made".into());
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
            Ok(()) => return Ok((token, label)),
            Err(e) => {
                last = WinErr::new(
                    "LogonUserW",
                    e.code().0 as u32,
                    format!("{user} via {label}"),
                );
            }
        }
    }
    Err(last)
}

/// Enable the privileges `CreateProcessAsUserW` needs, and report what actually
/// got enabled.
///
/// This is the failure most likely to be misread. A privilege the token *holds*
/// is still disabled until it is explicitly enabled, so an administrator can
/// call `CreateProcessAsUserW` and get `ERROR_PRIVILEGE_NOT_HELD` (1314) — an
/// error that reads like "you are not admin" when the real cause is "you did not
/// switch it on". Worse, `AdjustTokenPrivileges` returns success even when it
/// enabled nothing, so the result is checked against `GetLastError` rather than
/// against the return value.
pub fn enable_process_privileges() -> Vec<(String, String)> {
    const WANTED: [&str; 3] = [
        "SeAssignPrimaryTokenPrivilege",
        "SeIncreaseQuotaPrivilege",
        "SeTcbPrivilege",
    ];
    let mut out = Vec::new();
    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        )
        .is_err()
        {
            out.push(("<all>".into(), "OpenProcessToken failed".into()));
            return out;
        }
        for name in WANTED {
            let wname = wide(name);
            let mut luid = LUID::default();
            if LookupPrivilegeValueW(PCWSTR::null(), PCWSTR(wname.as_ptr()), &mut luid).is_err() {
                out.push((name.into(), "LookupPrivilegeValueW failed".into()));
                continue;
            }
            let tp = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES {
                    Luid: luid,
                    Attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let r = AdjustTokenPrivileges(token, false, Some(&tp), 0, None, None);
            // Success plus ERROR_NOT_ALL_ASSIGNED means the token does not hold
            // it; success plus ERROR_SUCCESS means it is now on.
            let le = super::identity::last_error();
            out.push((
                name.into(),
                match (r.is_ok(), le) {
                    (true, 0) => "ENABLED".to_string(),
                    (true, 1300) => "NOT HELD by this token (ERROR_NOT_ALL_ASSIGNED)".to_string(),
                    (true, other) => format!("adjust ok, last_error={other}"),
                    (false, other) => format!("AdjustTokenPrivileges failed, last_error={other}"),
                },
            ));
        }
        let _ = CloseHandle(token);
    }
    out
}

/// Grant the run identity access to this process's window station and the
/// `default` desktop.
///
/// Required for `CreateProcessAsUserW` at all, and required twice over for the
/// day 7–9 question: a process that cannot reach the desktop cannot drive a GUI
/// app even in principle, so failing here would produce a *false* negative on
/// the identity-boundary question rather than a real one.
pub fn grant_desktop_access(sid: PSID) -> Result<(), WinErr> {
    unsafe {
        let winsta_name = wide("winsta0");
        // 0x000F037F is WINSTA_ALL_ACCESS: the run identity needs the whole
        // set, because a partial grant fails later at process creation rather
        // than here, where the error would be legible.
        let winsta = OpenWindowStationW(PCWSTR(winsta_name.as_ptr()), false, 0x000F037F)
            .map_err(|e| WinErr::new("OpenWindowStationW", e.code().0 as u32, String::new()))?;
        let r = add_ace(HANDLE(winsta.0), sid, 0x000F037F);
        let _ = CloseWindowStation(winsta);
        r?;

        let desk_name = wide("default");
        // 0x000F01FF is DESKTOP_ALL_ACCESS.
        let desk = OpenDesktopW(
            PCWSTR(desk_name.as_ptr()),
            DESKTOP_CONTROL_FLAGS(0),
            false,
            0x000F01FF,
        )
        .map_err(|e| WinErr::new("OpenDesktopW", e.code().0 as u32, String::new()))?;
        let r = add_ace(HANDLE(desk.0), sid, 0x000F01FF);
        let _ = CloseDesktop(desk);
        r?;
    }
    Ok(())
}

unsafe fn add_ace(obj: HANDLE, sid: PSID, access: u32) -> Result<(), WinErr> {
    let mut old_dacl: *mut ACL = std::ptr::null_mut();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let rc = GetSecurityInfo(
        obj,
        SE_WINDOW_OBJECT,
        DACL_SECURITY_INFORMATION,
        None,
        None,
        Some(&mut old_dacl),
        None,
        Some(&mut sd),
    );
    if rc.is_err() {
        return Err(WinErr::new("GetSecurityInfo", rc.0, String::new()));
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: SET_ACCESS,
        // 3 == CONTAINER_INHERIT_ACE|OBJECT_INHERIT_ACE. A window station's
        // ACEs must inherit or the desktop objects under it stay unreachable.
        grfInheritance: windows::Win32::Security::ACE_FLAGS(3),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: windows::Win32::Security::Authorization::NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: PWSTR(sid.0 as *mut u16),
        },
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl), &mut new_dacl);
    if rc.is_err() {
        return Err(WinErr::new("SetEntriesInAclW", rc.0, String::new()));
    }
    let rc = SetSecurityInfo(
        obj,
        SE_WINDOW_OBJECT,
        DACL_SECURITY_INFORMATION,
        None,
        None,
        Some(new_dacl),
        None,
    );
    if !new_dacl.is_null() {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            new_dacl as *mut c_void,
        )));
    }
    if !sd.is_invalid() {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(sd.0)));
    }
    if rc.is_err() {
        return Err(WinErr::new("SetSecurityInfo", rc.0, String::new()));
    }
    Ok(())
}

/// Start `command_line` as the identity behind `token`, suspended, inside a
/// fresh kill-on-close job, then resume it.
///
/// `log` is an **inheritable** file handle that becomes the child's stdout and
/// stderr. `CREATE_NEW_CONSOLE` would be simpler and would throw the canary's
/// output away, and the canary's output is the entire evidence base.
pub fn spawn_contained_with_output(
    token: HANDLE,
    user: &str,
    password: &str,
    command_line: &str,
    log: HANDLE,
) -> Result<(Contained, &'static str), WinErr> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|e| WinErr::new("CreateJobObjectW", e.code().0 as u32, String::new()))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| WinErr::new("SetInformationJobObject", e.code().0 as u32, String::new()))?;

        let mut desktop = wide("winsta0\\default");
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            dwFlags: STARTF_USESTDHANDLES,
            hStdOutput: log,
            hStdError: log,
            hStdInput: HANDLE::default(),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let mut cmd = wide(command_line);

        let primary = CreateProcessAsUserW(
            Some(token),
            PCWSTR::null(),
            Some(PWSTR(cmd.as_mut_ptr())),
            None,
            None,
            // TRUE: the log handle has to cross into the child, and it was
            // created with an inheritable SECURITY_ATTRIBUTES for exactly this.
            true,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        );

        let path = match primary {
            Ok(()) => "CreateProcessAsUserW",
            Err(e) => {
                // `CreateProcessAsUserW` needs SeAssignPrimaryTokenPrivilege,
                // which Administrators do *not* hold by default —
                // ERROR_PRIVILEGE_NOT_HELD (1314) here means exactly that, not
                // "you are not an administrator". `CreateProcessWithLogonW`
                // goes through the secondary-logon service and needs no
                // privilege, so it is the honest fallback rather than a
                // weakening: the child still runs under the per-run identity,
                // which is the only property containment depends on.
                //
                // It cannot inherit handles, so the child's output is lost on
                // this path and the caller is told which path ran.
                let mut wuser = wide(user);
                let mut wdom = wide(".");
                let mut wpass = wide(password);
                let mut cmd2 = wide(command_line);
                let si2 = STARTUPINFOW {
                    cb: std::mem::size_of::<STARTUPINFOW>() as u32,
                    lpDesktop: PWSTR(desktop.as_mut_ptr()),
                    ..Default::default()
                };
                CreateProcessWithLogonW(
                    PCWSTR(wuser.as_mut_ptr()),
                    PCWSTR(wdom.as_mut_ptr()),
                    PCWSTR(wpass.as_mut_ptr()),
                    LOGON_WITH_PROFILE,
                    PCWSTR::null(),
                    Some(PWSTR(cmd2.as_mut_ptr())),
                    CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
                    None,
                    PCWSTR::null(),
                    &si2,
                    &mut pi,
                )
                .map_err(|e2| {
                    WinErr::new(
                        "CreateProcessAsUserW then CreateProcessWithLogonW",
                        e.code().0 as u32,
                        format!(
                            "primary={} fallback={} cmd={command_line}",
                            e.code().0,
                            e2.code().0
                        ),
                    )
                })?;
                "CreateProcessWithLogonW(no output capture)"
            }
        };

        // Assign before resuming: a process that ran even briefly outside the
        // job could have spawned something the job never sees.
        AssignProcessToJobObject(job, pi.hProcess).map_err(|e| {
            WinErr::new("AssignProcessToJobObject", e.code().0 as u32, String::new())
        })?;
        ResumeThread(pi.hThread);

        Ok((
            Contained {
                process: pi.hProcess,
                thread: pi.hThread,
                pid: pi.dwProcessId,
                job,
                token,
            },
            path,
        ))
    }
}

/// Suppress unused-import warnings for items kept for the day 7–9 stage.
#[allow(dead_code)]
fn _keepalive() {
    let _ = (
        GetProcessWindowStation,
        GetThreadDesktop,
        GetCurrentThreadId,
        AssignProcessToJobObject,
        TOKEN_QUERY,
        SECURITY_ATTRIBUTES::default(),
    );
}
