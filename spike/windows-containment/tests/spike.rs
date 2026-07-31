//! The spike's driver.
//!
//! It runs under the Windows CI job's `cargo test --workspace --all-features`
//! step, which is the only route onto a `windows-latest` runner that does not
//! touch `.github/workflows/` — a constitution-protected path.
//!
//! **This test does not assert.** That is deliberate, not laziness. The output
//! of a feasibility spike is a verdict with evidence, and the evidence is the
//! `SPIKE|` block in the CI log; a red job would stop the run at the first
//! interesting finding and hide everything after it. Negative results carry
//! equal weight here, so they must not abort the run that produces them.
//!
//! Read the result with:
//!
//! ```text
//! grep '^SPIKE|' <ci log>
//! ```

#[cfg(windows)]
#[test]
fn windows_egress_containment_spike() {
    use wfp_spike::report::Report;
    use wfp_spike::win::{harness, identity, launch};

    let mut report = Report::new();
    wfp_spike::report::emit("SPIKE|BEGIN|windows egress containment feasibility spike");

    // Reported, never inferred. WFP filter add needs Administrator (or Network
    // Configuration Operators), and that limitation has to be stated in the
    // same breath as the claim — Linux gets its containment unprivileged and
    // Windows does not.
    let elevated = identity::is_elevated();
    report.note("preflight.elevated", elevated);
    report.note(
        "preflight.os",
        std::env::var("OS").unwrap_or_else(|_| "<unset>".into()),
    );
    if !elevated {
        report.not_run(
            "all",
            "the whole spike",
            "not elevated; WFP filter add requires Administrator",
        );
        report.summary();
        return;
    }

    // A privilege the token holds is still disabled until it is switched on,
    // and `CreateProcessAsUserW` then fails with ERROR_PRIVILEGE_NOT_HELD —
    // an error that reads like "not an administrator". Enable them first and
    // report exactly which ones the runner's token actually has, so the next
    // reader does not have to guess.
    for (name, state) in launch::enable_process_privileges() {
        report.note(&format!("preflight.privilege.{name}"), state);
    }

    wfp_spike::report::emit("SPIKE|STAGE|core (days 1-3) + audit (day 4) - enforcement ON");
    harness::stage_core(&mut report, true, "core");

    wfp_spike::report::emit(
        "SPIKE|STAGE|negative control (day 5) - block filter DELIBERATELY omitted",
    );
    harness::stage_core(&mut report, false, "neg");

    wfp_spike::report::emit("SPIKE|STAGE|teardown after an abruptly killed supervisor (day 6)");
    harness::stage_abrupt_kill(
        &mut report,
        std::path::Path::new(env!("CARGO_BIN_EXE_wfp-spike")),
    );

    wfp_spike::report::emit("SPIKE|STAGE|identity boundary (days 7-9) - THIS is the spike");
    harness::stage_gui(&mut report);

    report.summary();
    wfp_spike::report::emit("SPIKE|END");
}

#[cfg(not(windows))]
#[test]
fn windows_egress_containment_spike_is_windows_only() {
    // The workspace must always build and test cleanly on Linux and macOS
    // (`CHARTER.md` §2 invariant 3). Nothing this spike measures exists here,
    // and saying so out loud beats a silently absent test.
    wfp_spike::report::emit("SPIKE|SKIP|not Windows; nothing to measure");
}
