//! Windows egress containment, end to end against a real kernel.
//!
//! This is the test the whole `egress_windows` series was built toward: a real
//! per-run identity, real WFP filters, a real child process, and a real
//! destination that either sees the traffic or does not.
//!
//! # It runs where it can run, and it fails where it should
//!
//! The spike this is ported from (`spike/windows-containment/tests/spike.rs`)
//! deliberately never failed: its output was a verdict with evidence, and a red
//! job truncates the run at the first interesting finding, hiding everything
//! after it. Negative results carried equal weight there.
//!
//! **That polarity is inverted here.** A shipping containment test has one job,
//! which is to go red when containment stops holding. There is nothing left to
//! discover; there is only a claim to keep true.
//!
//! # Three independent witnesses, because one is not evidence
//!
//! A client-side connect error is consistent with containment AND with a wrong
//! port, a dead listener, or a typo in this file. So each run is judged by:
//!
//!   1. the **destination**, which either accepted a connection or did not;
//!   2. the **audit lane**, which names the filter id that dropped it;
//!   3. the **positive control** - a declared destination that must still be
//!      reachable, so a child that never ran at all cannot pass.
//!
//! # The negative control is the point
//!
//! `an_undeclared_destination_is_refused` would pass just as happily if the
//! probe never executed: nothing connected to the undeclared oracle, therefore
//! zero sightings, therefore green. That is a vacuous test, and a vacuous
//! containment test is worse than none.
//!
//! So the same probe is run a second time with the undeclared port ADDED to
//! the allow list. It must connect. That inversion is what demonstrates this
//! test can tell contained from uncontained, and it is the falsifiability
//! fixture `CHARTER.md` Milestone 2 criterion 6 asks for.

#![cfg(all(windows, any(feature = "agent", feature = "sap-com")))]

use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flowproof_adapters::egress_windows::{run, HostReadiness};
use flowproof_trace::egress::{AllowEntry, HostMatch};

/// Generous, and deliberately so. The child is a FRESH local account, so its
/// first process pays for profile setup and PowerShell's cold start on top of
/// the work itself. A tight deadline here would flake as a containment
/// failure, which is the most misleading thing this test could do.
const RUN_TIMEOUT: Duration = Duration::from_secs(180);

/// A destination that records how many connections actually arrived.
///
/// Bound in the TEST process, not the child's - that is what makes it an
/// independent witness rather than a restatement of the child's own opinion.
struct Oracle {
    port: u16,
    seen: Arc<AtomicUsize>,
}

impl Oracle {
    /// Bind on loopback and accept in the background until dropped.
    ///
    /// Loopback is a deliberate choice and was checked on a real runner: the
    /// spike found WFP's ALE layer classifies and drops loopback connects,
    /// with `loopback=true` on the drop record. Had it not, an agent could
    /// reach a local service the flow never declared, which would be a hole
    /// rather than a convenience.
    fn bind() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // Read the byte the probe writes, so a bare SYN cannot be
                // mistaken for a completed connection.
                let mut buf = [0u8; 8];
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let _ = stream.read(&mut buf);
                counter.fetch_add(1, Ordering::SeqCst);
            }
        });
        Ok(Self { port, seen })
    }

    fn sightings(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }

    fn allow_entry(&self) -> AllowEntry {
        AllowEntry {
            host: HostMatch::Ip("127.0.0.1".parse().expect("static addr")),
            port: Some(self.port),
        }
    }
}

/// The probe the contained child runs: connect to each port, write a byte,
/// and say what happened.
///
/// `powershell.exe` rather than a helper binary flowproof would have to ship.
/// It lives in System32, where `Users` already holds read+execute, so the
/// per-run identity can start it without this test also having to get a
/// directory ACL right - a second mechanism whose failure would look exactly
/// like a containment failure.
///
/// Only the outer quotes are double quotes. The command line crosses
/// `CreateProcessW` as ONE string, and nesting doubles inside it is how this
/// kind of probe usually breaks.
fn probe_command(ports: &[u16]) -> String {
    let list = ports
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "powershell.exe -NoProfile -NonInteractive -Command \
         \"foreach($p in @({list})){{try{{$c=New-Object Net.Sockets.TcpClient;\
         $c.Connect('127.0.0.1',$p);$s=$c.GetStream();\
         $s.Write([Text.Encoding]::ASCII.GetBytes('fp'),0,2);$s.Flush();$c.Close();\
         Write-Output ('PROBE|'+$p+'|CONNECTED')}}catch{{\
         Write-Output ('PROBE|'+$p+'|REFUSED')}}}}\""
    )
}

/// What one contained run produced, with both oracles' verdicts beside it.
struct Observed {
    outcome: run::Outcome,
    declared_sightings: usize,
    undeclared_sightings: usize,
}

/// Run the probe against both oracles, declaring `allow`.
fn probe_run(declared: &Oracle, undeclared: &Oracle, allow: &[AllowEntry]) -> Observed {
    let outcome = run::run_contained(
        &probe_command(&[declared.port, undeclared.port]),
        &Default::default(),
        &[],
        allow,
        RUN_TIMEOUT,
    );
    // The child writes to the oracle and exits; the accept loop is a separate
    // thread, so give it a moment to record what already arrived. Only ever
    // makes the "saw nothing" assertions HARDER to pass.
    std::thread::sleep(Duration::from_millis(500));
    Observed {
        declared_sightings: declared.sightings(),
        undeclared_sightings: undeclared.sightings(),
        outcome,
    }
}

/// Everything a failure needs, in one block, because reproducing this locally
/// is not an option for most people who will read it.
fn evidence(label: &str, o: &Observed) -> String {
    format!(
        "[{label}] contained={} reason={:?} faults={:?} blocked={:?} exit={:?} \
         declared_sightings={} undeclared_sightings={} stdout={:?} stderr={:?}",
        o.outcome.is_contained(),
        o.outcome.not_contained,
        o.outcome.faults,
        o.outcome
            .blocked
            .iter()
            .map(|e| format!("{} ({})", e.destination, e.protocol))
            .collect::<Vec<_>>(),
        o.outcome.exit_code,
        o.declared_sightings,
        o.undeclared_sightings,
        o.outcome.stdout.trim(),
        o.outcome.stderr.trim(),
    )
}

/// Whether this host can enforce at all.
///
/// A host that cannot is a statement about the HOST, not about the code, so it
/// is not a failure - except on CI, where the runner is elevated by definition
/// and an unready host means something regressed. Without that clause a silent
/// loss of elevation would turn this whole file green while testing nothing,
/// which is the false green `CHARTER.md` §5 ranks first.
fn host_can_enforce() -> bool {
    let blockers = HostReadiness::probe().blockers();
    if blockers.is_empty() {
        return true;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "CI runs elevated, so this host must be able to enforce; it reported: {}",
        blockers.join("; ")
    );
    eprintln!(
        "SKIP windows containment E2E: this host cannot enforce ({}). \
         Run elevated to exercise it.",
        blockers.join("; ")
    );
    false
}

/// The claim, with all three witnesses agreeing.
#[test]
fn an_undeclared_destination_is_refused_and_a_declared_one_is_not() {
    if !host_can_enforce() {
        return;
    }
    let declared = Oracle::bind().expect("bind the declared oracle");
    let undeclared = Oracle::bind().expect("bind the undeclared oracle");

    let o = probe_run(&declared, &undeclared, &[declared.allow_entry()]);
    let ev = evidence("enforced", &o);

    // The run has to have BEEN contained before anything it reports means
    // anything. A run that never installed its filters and then saw no traffic
    // is not evidence of containment.
    assert!(o.outcome.is_contained(), "the run was not contained. {ev}");
    assert!(
        o.outcome.faults.is_empty(),
        "a fault is the absence of evidence, so nothing below can be certified over it. {ev}"
    );

    // Witness 3 first: without it, everything after is consistent with a child
    // that never ran.
    assert!(
        o.declared_sightings >= 1,
        "the DECLARED destination was never reached, so this run proves nothing about \
         the undeclared one - the probe may simply not have executed. {ev}"
    );

    // Witness 1: the destination itself.
    assert_eq!(
        o.undeclared_sightings, 0,
        "the undeclared destination accepted a connection; containment did not hold. {ev}"
    );

    // Witness 2: the audit lane names it, so the report can too. An agent that
    // was stopped but cannot be SHOWN to have been stopped is a weaker claim
    // than flowproof makes.
    let suffix = format!(":{}", undeclared.port);
    assert!(
        o.outcome
            .blocked
            .iter()
            .any(|e| e.destination.ends_with(&suffix)),
        "nothing in the audit lane names the undeclared destination, so the drop \
         cannot be evidenced even though the destination never saw it. {ev}"
    );
}

/// The negative control: with the same probe and the undeclared port DECLARED,
/// the connection must go through.
///
/// This is what makes the test above non-vacuous. If this one ever fails, the
/// test above stops meaning anything - not because containment broke, but
/// because the probe stopped reaching the network at all, and a test that
/// cannot connect when it is allowed to cannot prove anything by failing to
/// connect when it is not.
#[test]
fn the_same_probe_connects_when_the_destination_is_declared() {
    if !host_can_enforce() {
        return;
    }
    let declared = Oracle::bind().expect("bind the declared oracle");
    let undeclared = Oracle::bind().expect("bind the second oracle");

    let o = probe_run(
        &declared,
        &undeclared,
        &[declared.allow_entry(), undeclared.allow_entry()],
    );
    let ev = evidence("negative-control", &o);

    assert!(o.outcome.is_contained(), "the run was not contained. {ev}");
    assert!(
        o.declared_sightings >= 1 && o.undeclared_sightings >= 1,
        "both destinations were declared, so both must have been reached; a probe that \
         cannot connect when allowed makes the refusal test vacuous. {ev}"
    );
    assert!(
        o.outcome.blocked.is_empty(),
        "nothing was undeclared, so nothing should have been dropped. {ev}"
    );
}
