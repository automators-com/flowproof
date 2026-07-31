//! Everything the canary prints, also written to a file it opens itself.
//!
//! The supervisor's first choice is `CreateProcessAsUserW` with an inherited
//! stdout handle. Its fallback, `CreateProcessWithLogonW`, needs no privilege
//! but **cannot inherit handles** — so on that path the child's stdout goes
//! nowhere and the run produces no evidence at all.
//!
//! Writing the same lines to a file the child opens for itself makes the
//! evidence independent of which spawn path worked. The supervisor prefers this
//! file and falls back to the inherited one.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};

fn sink() -> &'static Mutex<Option<File>> {
    static SINK: OnceLock<Mutex<Option<File>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(None))
}

/// Open the side-channel log. A failure is printed and otherwise ignored: the
/// inherited-stdout path may still be working, and losing the side channel is
/// not a reason to lose the run.
pub fn open(path: &str) {
    match OpenOptions::new().create(true).append(true).open(path) {
        Ok(f) => {
            if let Ok(mut g) = sink().lock() {
                *g = Some(f);
            }
        }
        Err(e) => crate::report::emit(&format!("TEE|OPEN-FAILED|{path}|{e}")),
    }
}

/// Emit to stderr (past any test harness's capture) and to the side-channel
/// file.
pub fn line(s: &str) {
    crate::report::emit(s);
    if let Ok(mut g) = sink().lock() {
        if let Some(f) = g.as_mut() {
            let _ = writeln!(f, "{s}");
            let _ = f.flush();
        }
    }
}
