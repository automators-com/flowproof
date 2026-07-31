//! The spike's driver.
//!
//! It runs under the Windows CI job's `cargo test --workspace --all-features`
//! step, which is the only route onto a `windows-latest` runner that does not
//! touch `.github/workflows/` — a constitution-protected path.
//!
//! **The spike runs as a subprocess, and that is load-bearing.** `cargo test`
//! captures the stdout of a passing test and prints it only for a failing one.
//! The CI step has no `--nocapture` and the workflow may not be edited, so an
//! earlier revision that printed from the test thread produced a green Windows
//! job with zero `SPIKE|` lines — a whole run spent measuring nothing.
//!
//! libtest's capture is a thread-local redirect of Rust's `print!` macros; it
//! does not touch file descriptor 1. A child process inherits the real
//! descriptor, so its output reaches the job log even when the test passes.
//!
//! **This test does not assert on containment.** The output of a feasibility
//! spike is a verdict with evidence, and the evidence is the `SPIKE|` block in
//! the CI log; a red job would stop the run at the first interesting finding
//! and hide everything after it. Negative results carry equal weight here, so
//! they must not abort the run that produces them.
//!
//! Read the result with:
//!
//! ```text
//! grep '^SPIKE|' <ci log>
//! ```

#[cfg(windows)]
#[test]
fn windows_egress_containment_spike() {
    use std::process::{Command, Stdio};

    let exe = env!("CARGO_BIN_EXE_wfp-spike");
    println!("(this line is captured by libtest; the subprocess output below is not)");

    let status = Command::new(exe)
        .arg("run-all")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    // The only failure this test reports is "the spike binary did not run at
    // all". That is honesty rule 1: a harness that never started would
    // otherwise be indistinguishable from a harness that found nothing.
    match status {
        Ok(st) => {
            println!("SPIKE-DRIVER|exit={:?}", st.code());
            assert!(
                st.success(),
                "the spike binary exited non-zero ({:?}); see the SPIKE| lines above",
                st.code()
            );
        }
        Err(e) => panic!("could not start the spike binary at {exe}: {e}"),
    }
}

#[cfg(not(windows))]
#[test]
fn windows_egress_containment_spike_is_windows_only() {
    // The workspace must always build and test cleanly on Linux and macOS
    // (`CHARTER.md` §2 invariant 3). Nothing this spike measures exists here,
    // and saying so out loud beats a silently absent test.
    wfp_spike::report::emit("SPIKE|SKIP|not Windows; nothing to measure");
}
