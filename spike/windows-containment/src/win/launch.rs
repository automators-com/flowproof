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
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, SET_ACCESS,
    SE_WINDOW_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows::Win32::Security::{LogonUserW, LOGON32_LOGON_INTERACTIVE, LOGON32_PROVIDER_DEFAULT};
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
    CreateProcessAsUserW, GetCurrentThreadId, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTF_USESTDHANDLES, STARTUPINFOW,
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
pub fn logon(user: &str, password: &str) -> Result<HANDLE, WinErr> {
    let wuser = wide(user);
    let wdom = wide(".");
    let wpass = wide(password);
    let mut token = HANDLE::default();
    unsafe {
        LogonUserW(
            PCWSTR(wuser.as_ptr()),
            PCWSTR(wdom.as_ptr()),
            PCWSTR(wpass.as_ptr()),
            LOGON32_LOGON_INTERACTIVE,
            LOGON32_PROVIDER_DEFAULT,
            &mut token,
        )
    }
    .map_err(|e| WinErr::new("LogonUserW", e.code().0 as u32, user.to_string()))?;
    Ok(token)
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
    command_line: &str,
    log: HANDLE,
) -> Result<Contained, WinErr> {
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

        CreateProcessAsUserW(
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
        )
        .map_err(|e| {
            WinErr::new(
                "CreateProcessAsUserW",
                e.code().0 as u32,
                command_line.to_string(),
            )
        })?;

        // Assign before resuming: a process that ran even briefly outside the
        // job could have spawned something the job never sees.
        AssignProcessToJobObject(job, pi.hProcess).map_err(|e| {
            WinErr::new("AssignProcessToJobObject", e.code().0 as u32, String::new())
        })?;
        ResumeThread(pi.hThread);

        Ok(Contained {
            process: pi.hProcess,
            thread: pi.hThread,
            pid: pi.dwProcessId,
            job,
            token,
        })
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
