//! Windows-only machinery for the containment spike.
//!
//! Nothing in here is shipping code. It exists to produce evidence for
//! `spike/windows-containment/LOG.md` and should be deleted with the rest of
//! the spike once the verdict is taken.

pub mod gui;
pub mod harness;
pub mod identity;
pub mod launch;
pub mod netevents;
pub mod wfp;

/// A Win32 failure with enough context to be diagnosed from a CI log alone.
///
/// The `api` name matters as much as the code: `FwpmFilterAdd0` returning
/// `ERROR_ACCESS_DENIED` and `LogonUserW` returning it mean completely
/// different things, and a bare error number in a log cannot tell them apart.
#[derive(Debug, Clone)]
pub struct WinErr {
    pub api: &'static str,
    pub code: u32,
    pub context: String,
}

impl WinErr {
    pub fn new(api: &'static str, code: u32, context: String) -> Self {
        Self { api, code, context }
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

/// UTF-16, NUL-terminated. Every Win32 `W` entry point wants this and getting
/// the terminator wrong is a silent buffer overrun rather than a compile error.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}
