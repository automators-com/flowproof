//! Capturing a contained agent's stdout and stderr.
//!
//! The child runs as a DIFFERENT user, so a pipe created the ordinary way is
//! not reachable and `std::process::Command`'s plumbing does not apply. What
//! does work is an inheritable file handle: the child never opens the path, it
//! inherits the already-open handle, so no ACL on the file has to name the
//! run identity.
//!
//! Two files, not one. flowproof's `AgentRun` keeps stdout and stderr apart,
//! and #188 is specifically about quoting the tail of an agent's STDERR when
//! it dies before its first model call. Interleaving them into one file would
//! make that quote wrong exactly when it matters most.

use std::path::PathBuf;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Security::SECURITY_ATTRIBUTES;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetTempFileNameW, GetTempPathW, CREATE_ALWAYS, FILE_ATTRIBUTE_TEMPORARY,
    FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use super::{wide, WinErr};

/// One inheritable sink for a child's output, deleted when dropped.
pub struct Capture {
    handle: HANDLE,
    path: PathBuf,
    closed: bool,
}

impl Capture {
    /// Create a temp file and open it with an INHERITABLE handle.
    ///
    /// `bInheritHandle` is the whole point: without it `CreateProcessAsUserW`
    /// with `bInheritHandles: TRUE` still hands the child nothing, and the
    /// output vanishes with no error anywhere.
    pub fn create(tag: &str) -> Result<Self, WinErr> {
        let mut dir = [0u16; 261];
        let n = unsafe { GetTempPathW(Some(&mut dir)) };
        if n == 0 {
            return Err(WinErr::new("GetTempPathW", last_error(), tag.to_string()));
        }
        let prefix = wide("fp");
        // MAX_PATH exactly: GetTempFileNameW writes at most that and the
        // binding pins the array size.
        let mut name = [0u16; 260];
        let rc = unsafe {
            GetTempFileNameW(PCWSTR(dir.as_ptr()), PCWSTR(prefix.as_ptr()), 0, &mut name)
        };
        if rc == 0 {
            return Err(WinErr::new(
                "GetTempFileNameW",
                last_error(),
                tag.to_string(),
            ));
        }
        let path = PathBuf::from(String::from_utf16_lossy(
            &name[..name.iter().position(|&c| c == 0).unwrap_or(name.len())],
        ));

        let sa = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: std::ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let handle = unsafe {
            CreateFileW(
                PCWSTR(name.as_ptr()),
                FILE_GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                Some(&sa),
                CREATE_ALWAYS,
                FILE_ATTRIBUTE_TEMPORARY,
                None,
            )
        }
        .map_err(|e| {
            WinErr::new(
                "CreateFileW(inheritable capture)",
                e.code().0 as u32,
                format!("{tag} at {}", path.display()),
            )
        })?;

        Ok(Self {
            handle,
            path,
            closed: false,
        })
    }

    /// The handle to hand to `STARTUPINFOW`.
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// Close the write handle and read what the child wrote.
    ///
    /// Closing FIRST is not tidiness: the child's data is not guaranteed
    /// flushed to the file until every writing handle is closed, so reading
    /// while ours is open can return a short read that looks like an agent
    /// which stopped early.
    pub fn take(&mut self) -> String {
        self.close();
        std::fs::read_to_string(&self.path).unwrap_or_default()
    }

    fn close(&mut self) {
        if !self.closed {
            self.closed = true;
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.close();
        // Best effort. A temp file left behind is untidy; failing a run over
        // one would be worse.
        let _ = std::fs::remove_file(&self.path);
    }
}

fn last_error() -> u32 {
    use windows::Win32::Foundation::{GetLastError, ERROR_SUCCESS};
    let e = unsafe { GetLastError() };
    if e == ERROR_SUCCESS {
        0
    } else {
        e.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A capture round-trips what was written to it, and the handle is real.
    #[test]
    fn a_capture_reads_back_what_was_written() {
        let mut c = Capture::create("test").expect("temp file");
        std::fs::write(&c.path, "hello from the agent").expect("write");
        assert_eq!(c.take(), "hello from the agent");
    }

    /// Dropping removes the file. A test tool that leaves one temp file per
    /// run on a long-lived runner is a slow leak.
    #[test]
    fn dropping_removes_the_file() {
        let path = {
            let c = Capture::create("test").expect("temp file");
            let p = c.path.clone();
            assert!(p.exists(), "created");
            p
        };
        assert!(!path.exists(), "removed on drop");
    }

    /// `take` after a drop-less double call is harmless: closing is
    /// idempotent, so a caller that reads twice gets the same content rather
    /// than closing an already-closed handle.
    #[test]
    fn taking_twice_is_harmless() {
        let mut c = Capture::create("test").expect("temp file");
        std::fs::write(&c.path, "once").expect("write");
        assert_eq!(c.take(), "once");
        assert_eq!(c.take(), "once");
    }
}
