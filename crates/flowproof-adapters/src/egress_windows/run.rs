//! One contained run, start to finish.
//!
//! # The tier is derived from the RUN, not from a probe
//!
//! On Linux `Containment::command_flow()` decides the tier before anything
//! starts, and that is sound there: the seccomp filter installs in the child's
//! `pre_exec`, so a probe-pass implies an install-success and a failure aborts
//! the spawn outright.
//!
//! Windows has seven places to fail AFTER the probe says yes - the account,
//! the logon, the privileges, the desktop grant, the engine, the sublayer, the
//! filters - plus collection, without which there is no audit lane at all. A
//! host can be perfectly ready and the run still end up uncontained.
//!
//! So this returns what the run ACHIEVED. Reporting the probe's answer as the
//! tier would mean a run whose filters failed to install still said
//! "enforced", which is the false green of #300 and #301 with a longer fuse.
//! `Enforced` is returned from exactly one place: the end of the happy path,
//! after every step above has succeeded.
//!
//! # Enable-failure is a tier; readback-failure is a fault
//!
//! The split mirrors Linux. Anything decided BEFORE the agent runs makes the
//! run not-contained, because there is still time to say so honestly. Anything
//! that breaks DURING or after - here, being unable to read the audit lane
//! back - is a fault in the sense of [`crate::egress::EgressLog::faults`]:
//! the absence of evidence, which must never be read as evidence of absence.

use std::collections::BTreeMap;

use flowproof_trace::egress::{AllowEntry, EgressEvent};

use super::{audit, filters, identity, logon, netevents, spawn, wfp, HostReadiness};

/// What a contained run produced.
///
/// Deliberately plain data rather than `EgressLog`/`Containment`: those live
/// behind the `agent` feature, and depending on them here would put this
/// module behind it too - which would make `cargo check --target
/// x86_64-pc-windows-msvc` impossible and leave this code with no typecheck at
/// all. The caller converts.
#[derive(Debug, Default)]
pub struct Outcome {
    /// Undeclared destinations the kernel refused, in order.
    pub blocked: Vec<EgressEvent>,
    /// Anything that stopped us observing what we were supposed to observe.
    pub faults: Vec<String>,
    /// `None` means contained. `Some(reason)` is the honest tier line.
    pub not_contained: Option<String>,
    /// The agent's exit code, if it exited.
    pub exit_code: Option<i32>,
    /// Whether the launcher could capture the agent's output at all.
    pub captured_output: bool,
}

impl Outcome {
    /// The run was contained: every step succeeded and nothing prevented us
    /// observing it.
    pub fn is_contained(&self) -> bool {
        self.not_contained.is_none()
    }

    /// A run that never got contained, with the reason. The only constructor
    /// besides the happy path in [`run_contained`].
    pub fn uncontained(reason: impl Into<String>) -> Self {
        Self {
            not_contained: Some(reason.into()),
            ..Default::default()
        }
    }
}

/// Run `command_line` under a fresh per-run identity, behind WFP filters
/// scoped to it, and report what the kernel refused.
///
/// Every early return is an `Outcome` carrying a reason rather than an `Err`,
/// because "the run was not contained" is a RESULT, not a failure to produce
/// one - the agent may still have run, and the report has to say both things.
pub fn run_contained(
    command_line: &str,
    env: &BTreeMap<String, String>,
    entries: &[AllowEntry],
    timeout: std::time::Duration,
) -> Outcome {
    let host = HostReadiness::probe();
    let blockers = host.blockers();
    if !blockers.is_empty() {
        return Outcome::uncontained(format!(
            "this host cannot enforce it: {}",
            blockers.join("; ")
        ));
    }

    // Collection first: without an audit lane there is no evidence, and on
    // Windows the audit lane is the only witness. Deciding this BEFORE the
    // agent starts is what makes it a tier rather than a fault.
    let collection = match netevents::NetEventCollection::enable() {
        Ok(c) => c,
        Err(e) => {
            return Outcome::uncontained(format!(
                "net-event collection could not be enabled ({e}), so the run could not \
                 be audited"
            ))
        }
    };

    let mut ident = match identity::RunIdentity::create() {
        Ok(i) => i,
        Err(e) => return Outcome::uncontained(format!("the per-run identity: {e}")),
    };

    // Enabling is separate from probing (see `logon`), and its result decides
    // which launcher path `spawn` can take - not whether we proceed.
    let privileges = logon::enable_launch_privileges();
    let token = match logon::logon(&ident.name, &ident.password) {
        Ok(t) => t,
        Err(e) => return Outcome::uncontained(format!("logging the identity on: {e}")),
    };

    if let Err(e) = spawn::grant_desktop_access(ident.psid()) {
        return Outcome::uncontained(format!("granting window-station access: {e}"));
    }

    let mut engine = match wfp::Engine::open_dynamic() {
        Ok(e) => e,
        Err(e) => return Outcome::uncontained(format!("opening the WFP session: {e}")),
    };
    if let Err(e) = engine.add_sublayer() {
        return Outcome::uncontained(format!("adding the private sublayer: {e}"));
    }
    let user_condition = match filters::UserCondition::for_sid(&ident.sid_string) {
        Ok(u) => u,
        Err(e) => return Outcome::uncontained(format!("building the identity condition: {e}")),
    };
    let block_ids = match filters::install(&mut engine, &user_condition, entries) {
        Ok(ids) => ids,
        Err(e) => return Outcome::uncontained(format!("installing the filters: {e}")),
    };

    // Taken AFTER the filters exist, so the readback cannot pick up a record
    // from before this run was being contained.
    let since = audit::now_filetime();

    let contained = match spawn::spawn(
        token.handle(),
        &ident.name,
        &ident.password,
        command_line,
        env,
        None,
    ) {
        Ok(c) => c,
        Err(e) => return Outcome::uncontained(format!("starting the agent: {e}")),
    };

    let exit_code = wait_for(&contained, timeout);

    let mut out = Outcome {
        exit_code,
        captured_output: contained.captures_output,
        ..Default::default()
    };
    if !out.captured_output {
        out.faults.push(
            "the agent was started through CreateProcessWithLogonW, which cannot inherit \
             handles, so its stdout and stderr were not captured"
                .to_string(),
        );
    }
    if let Some(w) = &collection.keyword_warning {
        // Not a fault: drops are still collected. Recorded so a missing
        // classify-allow record is explicable rather than mysterious.
        out.faults
            .push(format!("positive-evidence keyword unavailable: {w}"));
    }
    if !logon::all_enabled(&privileges) {
        out.faults.push(format!(
            "launch privileges were not all enabled: {privileges:?}"
        ));
    }

    // Read back BEFORE teardown drops the session: closing the engine removes
    // the filters, and the ids are what attributes the records.
    match audit::drops_for(&block_ids, since) {
        Ok(drops) => out.blocked = drops.into_iter().map(Into::into).collect(),
        Err(e) => out.faults.push(format!(
            "the audit lane could not be read back ({e}); nothing can be certified over \
             a run whose evidence was not readable"
        )),
    }

    let _ = ident.delete();
    out
}

/// Wait for the contained process, or give up at `timeout`.
fn wait_for(contained: &spawn::Contained, timeout: std::time::Duration) -> Option<i32> {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};

    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let waited = unsafe { WaitForSingleObject(contained.process(), ms) };
    if waited != WAIT_OBJECT_0 {
        return None;
    }
    let mut code = 0u32;
    if unsafe { GetExitCodeProcess(contained.process(), &mut code) }.is_err() {
        return None;
    }
    Some(code as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `Enforced` has exactly one source: the end of the happy path. Every
    /// early return carries a reason, so a run that failed at step five cannot
    /// be mistaken for one that succeeded.
    #[test]
    fn every_early_return_is_uncontained_and_says_why() {
        let o = Outcome::uncontained("the per-run identity: NetUserAdd failed");
        assert!(!o.is_contained());
        let why = o
            .not_contained
            .as_deref()
            .expect("a reason travels with it");
        assert!(why.contains("NetUserAdd"), "{why}");
        // And it claims nothing about a run that never happened.
        assert!(o.blocked.is_empty());
        assert_eq!(o.exit_code, None);
    }

    /// A default `Outcome` is contained, which is only correct because the
    /// happy path is the ONLY place one is built without a reason.
    #[test]
    fn the_default_outcome_is_the_happy_path() {
        assert!(Outcome::default().is_contained());
    }

    /// Losing the agent's output is a fault, not a silent gap: an agent whose
    /// stderr vanished looks exactly like one that printed nothing. The
    /// default is `false`, so a path that forgets to set it reports the
    /// pessimistic answer rather than claiming a capture it never made.
    #[test]
    fn output_capture_defaults_to_not_captured() {
        assert!(!Outcome::default().captured_output);
    }
}
