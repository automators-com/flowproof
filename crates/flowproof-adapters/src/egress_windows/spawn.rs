//! Starting the agent as the per-run identity, inside a job it cannot escape.
//!
//! Two things here are easy to get wrong and expensive to discover on a CI
//! round trip.
//!
//! **The desktop grant is not optional.** `CreateProcessAsUserW` needs the
//! target identity to have access to the window station and the `default`
//! desktop. Without it the child either fails to start or starts and dies at
//! CSRSS connect - and on a console process the symptom looks like a crash,
//! not a permissions problem. It is needed for a headless agent, not only for
//! driving a GUI.
//!
//! **The job object is not what contains the network.** WFP has no PID
//! condition, which is the whole reason the identity exists. The job exists so
//! a grandchild cannot outlive the run, and so teardown is total: closing the
//! job handle kills whatever is still inside it.
//!
//! Nothing here contains egress either. It starts a process under an identity
//! a filter can later be scoped to; the step that installs the WFP filters is
//! the step that makes a run contained.

use std::ffi::c_void;

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::Authorization::{
    GetSecurityInfo, SetEntriesInAclW, SetSecurityInfo, EXPLICIT_ACCESS_W, NO_MULTIPLE_TRUSTEE,
    SET_ACCESS, SE_WINDOW_OBJECT, TRUSTEE_IS_SID, TRUSTEE_IS_USER, TRUSTEE_W,
};
use windows::Win32::Security::{
    ACE_FLAGS, ACL, DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, CloseWindowStation, OpenDesktopW, OpenWindowStationW, DESKTOP_CONTROL_FLAGS,
};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, CreateProcessWithLogonW, ResumeThread, CREATE_NO_WINDOW,
    CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, LOGON_WITH_PROFILE, PROCESS_INFORMATION,
    STARTF_USESTDHANDLES, STARTUPINFOW,
};

use super::{wide, WinErr};

/// `WINSTA_ALL_ACCESS`. The identity needs the whole set: a partial grant
/// fails later at process creation, where the error says nothing useful,
/// rather than here where it would be legible.
const WINSTA_ALL_ACCESS: u32 = 0x000F_037F;
/// `DESKTOP_ALL_ACCESS`.
const DESKTOP_ALL_ACCESS: u32 = 0x000F_01FF;
/// `CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE`. A window station's ACEs must
/// inherit or the desktop objects under it stay unreachable.
const INHERIT_ACE: u32 = 3;

/// A running agent, its job, and how it was started.
///
/// Dropping this closes the job, and the job is `KILL_ON_JOB_CLOSE` - so the
/// agent and everything it spawned die with it. That is deliberate: an agent
/// process outliving the run that started it is worse than an abrupt kill,
/// and `Drop` is the only teardown that survives a panic.
pub struct Contained {
    process: HANDLE,
    thread: HANDLE,
    job: HANDLE,
    pub pid: u32,
    /// Which launcher actually ran, for the report.
    pub launcher: &'static str,
    /// Whether the child's stdout/stderr could be captured.
    ///
    /// False on the `CreateProcessWithLogonW` fallback, which cannot inherit
    /// handles. The caller has to know: an agent whose output vanished looks
    /// exactly like an agent that printed nothing, and #188 exists because
    /// that confusion is expensive.
    pub captures_output: bool,
}

impl Contained {
    /// The process handle, for waiting on it.
    pub fn process(&self) -> HANDLE {
        self.process
    }
}

impl Drop for Contained {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.thread);
            let _ = CloseHandle(self.process);
            // Last, and the one that matters: closing the job kills anything
            // still inside it.
            let _ = CloseHandle(self.job);
        }
    }
}

/// Grant `sid` access to this process's window station and `default` desktop.
///
/// Must be called before [`spawn`]. See the module docs: without it a headless
/// child dies at CSRSS connect and the symptom looks like a crash.
pub fn grant_desktop_access(sid: PSID) -> Result<(), WinErr> {
    unsafe {
        let winsta_name = wide("winsta0");
        let winsta = OpenWindowStationW(PCWSTR(winsta_name.as_ptr()), false, WINSTA_ALL_ACCESS)
            .map_err(|e| WinErr::new("OpenWindowStationW", e.code().0 as u32, "winsta0"))?;
        let r = add_ace(HANDLE(winsta.0), sid, WINSTA_ALL_ACCESS);
        let _ = CloseWindowStation(winsta);
        r?;

        let desk_name = wide("default");
        let desk = OpenDesktopW(
            PCWSTR(desk_name.as_ptr()),
            DESKTOP_CONTROL_FLAGS(0),
            false,
            DESKTOP_ALL_ACCESS,
        )
        .map_err(|e| WinErr::new("OpenDesktopW", e.code().0 as u32, "default"))?;
        let r = add_ace(HANDLE(desk.0), sid, DESKTOP_ALL_ACCESS);
        let _ = CloseDesktop(desk);
        r?;
    }
    Ok(())
}

/// Add one allow-ACE for `sid` to a window-object's DACL.
///
/// # Safety
/// `obj` must be a valid window station or desktop handle.
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
        return Err(WinErr::new("GetSecurityInfo", rc.0, "window object"));
    }

    let ea = EXPLICIT_ACCESS_W {
        grfAccessPermissions: access,
        grfAccessMode: SET_ACCESS,
        grfInheritance: ACE_FLAGS(INHERIT_ACE),
        Trustee: TRUSTEE_W {
            pMultipleTrustee: std::ptr::null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: PWSTR(sid.0 as *mut u16),
        },
    };

    let mut new_dacl: *mut ACL = std::ptr::null_mut();
    let rc = SetEntriesInAclW(Some(&[ea]), Some(old_dacl), &mut new_dacl);
    if rc.is_err() {
        return Err(WinErr::new("SetEntriesInAclW", rc.0, "window object"));
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
    free_local(new_dacl as *mut c_void);
    if !sd.is_invalid() {
        free_local(sd.0);
    }
    if rc.is_err() {
        return Err(WinErr::new("SetSecurityInfo", rc.0, "window object"));
    }
    Ok(())
}

unsafe fn free_local(p: *mut c_void) {
    if !p.is_null() {
        use windows::Win32::Foundation::{LocalFree, HLOCAL};
        LocalFree(Some(HLOCAL(p)));
    }
}

/// Start `command_line` as the identity behind `token`, inside a fresh
/// kill-on-close job.
///
/// `output` is an **inheritable** file handle that becomes the child's stdout
/// and stderr. `CREATE_NEW_CONSOLE` would be simpler and would throw the
/// agent's output away, and for an agent under test that output is the
/// evidence.
pub fn spawn(
    token: HANDLE,
    user: &str,
    password: &str,
    command_line: &str,
    output: Option<HANDLE>,
) -> Result<Contained, WinErr> {
    unsafe {
        let job = CreateJobObjectW(None, PCWSTR::null())
            .map_err(|e| WinErr::new("CreateJobObjectW", e.code().0 as u32, ""))?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .map_err(|e| WinErr::new("SetInformationJobObject", e.code().0 as u32, ""))?;

        let mut desktop = wide("winsta0\\default");
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            dwFlags: if output.is_some() {
                STARTF_USESTDHANDLES
            } else {
                Default::default()
            },
            hStdOutput: output.unwrap_or_default(),
            hStdError: output.unwrap_or_default(),
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
            // The output handle has to cross into the child, and it was
            // created inheritable for exactly this.
            output.is_some(),
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
            None,
            PCWSTR::null(),
            &si,
            &mut pi,
        );

        let used_fallback = match primary {
            Ok(()) => false,
            Err(e) => {
                // ERROR_PRIVILEGE_NOT_HELD (1314) here means
                // SeAssignPrimaryTokenPrivilege, not "you are not an
                // administrator". CreateProcessWithLogonW goes through the
                // secondary-logon service and needs no privilege, so it is an
                // honest fallback rather than a weakening: the child still
                // runs under the per-run identity, which is the only property
                // containment depends on.
                //
                // It cannot inherit handles, so the child's output is lost on
                // this path - reported rather than silently accepted.
                spawn_with_logon(user, password, command_line, &mut desktop, &mut pi, e)?;
                true
            }
        };
        let (launcher, captures_output) = launcher_report(used_fallback, output.is_some());

        // Assign BEFORE resuming: a process that ran even briefly outside the
        // job could have spawned something the job never sees.
        AssignProcessToJobObject(job, pi.hProcess)
            .map_err(|e| WinErr::new("AssignProcessToJobObject", e.code().0 as u32, ""))?;
        ResumeThread(pi.hThread);

        Ok(Contained {
            process: pi.hProcess,
            thread: pi.hThread,
            job,
            pid: pi.dwProcessId,
            launcher,
            captures_output,
        })
    }
}

/// Which launcher ran, and whether it could capture the child's output.
///
/// The rule worth isolating: **the fallback never captures, whatever the
/// caller asked for.** `CreateProcessWithLogonW` cannot inherit handles, so a
/// caller that passed an output handle still gets nothing back - and an agent
/// whose stderr vanished looks exactly like an agent that printed nothing.
/// That confusion is what #188 was filed about, so the answer travels with the
/// result rather than being inferred by whoever reads it.
fn launcher_report(used_fallback: bool, wanted_output: bool) -> (&'static str, bool) {
    if used_fallback {
        ("CreateProcessWithLogonW", false)
    } else {
        ("CreateProcessAsUserW", wanted_output)
    }
}

/// The no-privilege fallback. Both error codes are reported, because the
/// primary's code is the one that says WHY the fallback was needed.
///
/// # Safety
/// Called only from [`spawn`], with its buffers still alive.
unsafe fn spawn_with_logon(
    user: &str,
    password: &str,
    command_line: &str,
    desktop: &mut [u16],
    pi: &mut PROCESS_INFORMATION,
    primary: windows::core::Error,
) -> Result<(), WinErr> {
    let mut wuser = wide(user);
    let mut wdom = wide(".");
    let mut wpass = wide(password);
    let mut cmd = wide(command_line);
    let si = STARTUPINFOW {
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
        Some(PWSTR(cmd.as_mut_ptr())),
        CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT,
        None,
        PCWSTR::null(),
        &si,
        pi,
    )
    .map_err(|fallback| {
        WinErr::new(
            "CreateProcessAsUserW then CreateProcessWithLogonW",
            primary.code().0 as u32,
            format!(
                "primary={} fallback={} cmd={command_line}",
                primary.code().0,
                fallback.code().0
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The access masks are the documented constants, spelled out because a
    /// partial grant does not fail here - it fails at process creation, with
    /// an error that says nothing about window stations.
    #[test]
    fn the_access_masks_are_the_all_access_constants() {
        assert_eq!(WINSTA_ALL_ACCESS, 0x000F_037F);
        assert_eq!(DESKTOP_ALL_ACCESS, 0x000F_01FF);
        // CONTAINER_INHERIT_ACE | OBJECT_INHERIT_ACE.
        assert_eq!(INHERIT_ACE, 3);
    }

    /// The fallback never captures output, whatever the caller asked for.
    ///
    /// This is the one decision in this module that is pure logic rather than
    /// a Win32 call, and it is the one that misleads if it is wrong: an agent
    /// whose stderr vanished is indistinguishable from an agent that printed
    /// nothing.
    #[test]
    fn the_fallback_never_reports_captured_output() {
        // Primary path: captures exactly what the caller asked for.
        assert_eq!(launcher_report(false, true), ("CreateProcessAsUserW", true));
        assert_eq!(
            launcher_report(false, false),
            ("CreateProcessAsUserW", false)
        );

        // Fallback: never captures, even when output WAS requested.
        assert_eq!(
            launcher_report(true, true),
            ("CreateProcessWithLogonW", false),
            "CreateProcessWithLogonW cannot inherit handles"
        );
        assert_eq!(
            launcher_report(true, false),
            ("CreateProcessWithLogonW", false)
        );
    }
}
