//! Egress containment, the cross-platform half: the containment tier a run
//! achieved, the log of denied attempts, and the resolved allow-set the
//! Linux supervisor enforces. The seccomp mechanism itself lives in
//! `egress_linux` and is compiled only on Linux; everything here is
//! platform-neutral so the "not contained" report path is exercised on every
//! OS.

use std::collections::BTreeSet;
use std::net::IpAddr;

use flowproof_trace::egress::{self, AllowEntry, EgressEvent, HostMatch};

/// The containment tier a single agent run achieved. Printed on EVERY agent
/// report, on every platform - honesty about what was and was not enforced
/// is the whole point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Containment {
    /// A real default-deny seccomp filter was installed in the child (Linux).
    Enforced,
    /// No containment. The reason is honest and always printed.
    NotContained(String),
}

impl Containment {
    /// Did the run enforce containment? `assert_no_egress` can only certify
    /// when this is true.
    pub fn is_enforced(&self) -> bool {
        matches!(self, Containment::Enforced)
    }

    /// The reason a run was not contained, if it was not.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Containment::NotContained(why) => Some(why),
            Containment::Enforced => None,
        }
    }

    /// The tier line printed on the report.
    pub fn report_line(&self) -> String {
        match self {
            Containment::Enforced => "egress containment: enforced (linux seccomp)".to_string(),
            Containment::NotContained(why) => {
                format!("egress containment: not contained ({why})")
            }
        }
    }

    /// The tier for a flow that does not ENGAGE egress: it declares no
    /// `allow_egress` and asserts no egress, so no seccomp filter is installed
    /// and there is nothing to contain. Containment is opt-in; an unengaged
    /// flow claims no tier.
    pub fn not_engaged() -> Self {
        Containment::NotContained(
            "flow does not engage egress (no allow_egress or assert_no_egress)".to_string(),
        )
    }

    /// The tier for a `url:` flow: a service flowproof did not start cannot
    /// be contained.
    pub fn url_flow() -> Self {
        Containment::NotContained(
            "a url: service is not contained; flowproof does not own it".to_string(),
        )
    }

    /// The tier a `command:` flow achieves on THIS platform and kernel: a
    /// real seccomp probe on Linux, a flat "not contained" everywhere else.
    #[cfg(target_os = "linux")]
    pub fn command_flow() -> Self {
        crate::egress_linux::probe_containment()
    }

    #[cfg(not(target_os = "linux"))]
    pub fn command_flow() -> Self {
        Containment::NotContained(
            "egress containment is Linux-only; this platform is not contained".to_string(),
        )
    }
}

/// What the supervisor observed: the denied (undeclared) egress attempts, in
/// order. Surfaces through [`crate::agent_runner::AgentRun`] beside
/// `divergence`, exactly like the proxy log. Empty on every non-enforced
/// run, and empty on an enforced run that attempted nothing undeclared.
#[derive(Debug, Clone, Default)]
pub struct EgressLog {
    /// Every denied attempt, in order - retries included.
    pub blocked: Vec<EgressEvent>,
    /// Every supervisor FAULT, in order. A fault is not a policy denial: it
    /// is a trapped syscall the supervisor could not adjudicate at all,
    /// because the mechanism it needs was refused (`process_vm_readv` or
    /// `pidfd_getfd` denied by a hardened host).
    ///
    /// The distinction is the whole point. A denial is evidence: we saw an
    /// undeclared destination and refused it. A fault is the ABSENCE of
    /// evidence: the syscall was refused too, so nothing reached the network -
    /// but we never learned where it was going, so `blocked` stays empty and
    /// an emptiness test over it means nothing. Kept in its own field so the
    /// two can never be confused for one another.
    pub faults: Vec<String>,
}

impl EgressLog {
    /// The set of undeclared destinations attempted, deduped by destination
    /// so retry-count variance is irrelevant.
    ///
    /// This is HALF the `assert_no_egress` predicate. Emptiness here means
    /// "nothing undeclared was seen", which is only the same as "nothing
    /// undeclared happened" when the supervisor was working - see
    /// [`EgressLog::faults`] for the other half.
    pub fn undeclared_destinations(&self) -> Vec<EgressEvent> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for event in &self.blocked {
            if seen.insert(event.destination.clone()) {
                out.push(event.clone());
            }
        }
        out
    }

    /// The distinct supervisor faults, deduped and in first-seen order. One
    /// broken mechanism produces a fault per trapped syscall, and a hundred
    /// copies of "process_vm_readv: Operation not permitted" is one finding.
    pub fn distinct_faults(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for fault in &self.faults {
            if seen.insert(fault.clone()) {
                out.push(fault.clone());
            }
        }
        out
    }

    /// Nothing undeclared was attempted AND the supervisor adjudicated every
    /// trapped syscall it was handed. Both halves are required: a run whose
    /// supervisor could not read child memory blocked everything and observed
    /// nothing, which is not the same as clean.
    pub fn is_clean(&self) -> bool {
        self.blocked.is_empty() && self.faults.is_empty()
    }
}

/// The resolved egress policy: the allow entries from the CURRENT spec, with
/// hostnames resolved to their IP sets ONCE at build time and pinned.
/// Loopback (127/8, ::1) is exempt wholesale and never appears here. This is
/// POLICY, not authority-in-trace: enforcement always uses the current
/// spec's set.
#[derive(Debug, Clone, Default)]
pub struct AllowSet {
    /// Host entries are pre-resolved into `Ip` entries; only `Ip`/`Cidr`
    /// remain, each keeping its own optional port constraint.
    entries: Vec<AllowEntry>,
}

impl AllowSet {
    /// Build from the spec's `allow_egress`, whose `${VAR}` refs are ALREADY
    /// resolved. A hostname is resolved to its IP set and pinned here (the
    /// agent's own lookups still go to the loopback resolver, which is
    /// exempt). A name that does not resolve contributes no IPs, so its
    /// traffic is denied - the safe default.
    pub fn resolve(entries: &[String]) -> Result<Self, String> {
        let mut out = Vec::new();
        for raw in entries {
            let parsed = egress::parse_allow_entry(raw)?;
            match parsed.host {
                HostMatch::Host(name) => {
                    for ip in resolve_host(&name) {
                        out.push(AllowEntry {
                            host: HostMatch::Ip(ip),
                            port: parsed.port,
                        });
                    }
                }
                HostMatch::Ip(_) | HostMatch::Cidr(_, _) => out.push(parsed),
            }
        }
        Ok(Self { entries: out })
    }

    /// Is `(ip, port)` allowed? Loopback is allowed wholesale, independent of
    /// the list. `ip` is expected already normalized (a v4-mapped-v6
    /// collapsed to v4) by the caller.
    pub fn allows(&self, ip: IpAddr, port: u16) -> bool {
        if egress::is_loopback(ip) {
            return true;
        }
        self.entries
            .iter()
            .any(|entry| entry.port_ok(port) && host_matches(&entry.host, ip))
    }

    /// No declared destinations: a contained run with an empty allow-set
    /// permits only loopback.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Does a resolved host matcher admit `ip`?
fn host_matches(host: &HostMatch, ip: IpAddr) -> bool {
    match host {
        HostMatch::Ip(allowed) => *allowed == ip,
        HostMatch::Cidr(base, prefix) => egress::cidr_contains(*base, *prefix, ip),
        // Resolved away in `AllowSet::resolve`; never matches directly.
        HostMatch::Host(_) => false,
    }
}

/// Resolve a hostname to its IP set, once. A failure yields no IPs (deny).
fn resolve_host(name: &str) -> Vec<IpAddr> {
    use std::net::ToSocketAddrs;
    // ToSocketAddrs needs a port; 0 is fine - only the IPs are kept, the
    // entry's own port constraint is applied at match time.
    match (name, 0u16).to_socket_addrs() {
        Ok(addrs) => addrs.map(|sa| sa.ip()).collect(),
        Err(_) => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn not_contained_report_line_is_honest() {
        let c = Containment::url_flow();
        assert!(!c.is_enforced());
        assert!(c.report_line().contains("not contained"));
        assert!(c.report_line().contains("does not own"));

        let c = Containment::NotContained("kernel too old".into());
        assert_eq!(
            c.report_line(),
            "egress containment: not contained (kernel too old)"
        );
    }

    #[test]
    fn enforced_report_line() {
        let c = Containment::Enforced;
        assert!(c.is_enforced());
        assert_eq!(
            c.report_line(),
            "egress containment: enforced (linux seccomp)"
        );
        assert_eq!(c.reason(), None);
    }

    #[test]
    fn undeclared_destinations_dedupe_by_destination() {
        let log = EgressLog {
            faults: Vec::new(),
            blocked: vec![
                EgressEvent {
                    destination: "198.51.100.9:443".into(),
                    protocol: "tcp".into(),
                    at_ms: 10,
                },
                EgressEvent {
                    destination: "198.51.100.9:443".into(),
                    protocol: "tcp".into(),
                    at_ms: 30,
                },
                EgressEvent {
                    destination: "203.0.113.9:53".into(),
                    protocol: "udp".into(),
                    at_ms: 40,
                },
            ],
        };
        assert!(!log.is_clean());
        let undeclared = log.undeclared_destinations();
        assert_eq!(undeclared.len(), 2, "retries collapse to one destination");
        assert_eq!(undeclared[0].destination, "198.51.100.9:443");
        assert_eq!(undeclared[1].destination, "203.0.113.9:53");
    }

    /// A fault is not a denial, and an empty `blocked` list is not by itself
    /// a clean run. This is the log-level half of #300.
    #[test]
    fn a_faulted_log_is_not_clean_even_with_nothing_blocked() {
        let log = EgressLog {
            blocked: Vec::new(),
            faults: vec!["connect: process_vm_readv: Operation not permitted".into()],
        };
        // The old predicate - "nothing was blocked" - still reads as empty.
        assert!(log.undeclared_destinations().is_empty());
        // The real one does not.
        assert!(!log.is_clean(), "a blind supervisor observed nothing");
    }

    #[test]
    fn repeated_faults_dedupe_but_keep_first_seen_order() {
        let log = EgressLog {
            blocked: Vec::new(),
            faults: vec![
                "connect: process_vm_readv: EPERM".into(),
                "sendto: pidfd_getfd: EPERM".into(),
                "connect: process_vm_readv: EPERM".into(),
            ],
        };
        let distinct = log.distinct_faults();
        assert_eq!(distinct.len(), 2, "one broken mechanism is one finding");
        assert_eq!(distinct[0], "connect: process_vm_readv: EPERM");
        assert_eq!(distinct[1], "sendto: pidfd_getfd: EPERM");
        // The raw list keeps every occurrence, so a count is still available.
        assert_eq!(log.faults.len(), 3);
    }

    #[test]
    fn a_run_with_neither_blocked_nor_faults_is_clean() {
        assert!(EgressLog::default().is_clean());
    }

    #[test]
    fn allow_set_admits_declared_and_denies_the_rest() {
        let set = AllowSet::resolve(&["198.51.100.9:443".to_string(), "10.0.0.0/8".to_string()])
            .expect("resolves");
        assert!(set.allows(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)), 443));
        // Wrong port on a port-constrained entry.
        assert!(!set.allows(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)), 80));
        // Inside the cidr, any port.
        assert!(set.allows(IpAddr::V4(Ipv4Addr::new(10, 1, 2, 3)), 8080));
        // Outside everything.
        assert!(!set.allows(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9)), 443));
        // Loopback is exempt wholesale, independent of the list.
        assert!(set.allows(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 9999));
    }

    #[test]
    fn an_empty_allow_set_permits_only_loopback() {
        let set = AllowSet::resolve(&[]).expect("resolves");
        assert!(set.is_empty());
        assert!(set.allows(IpAddr::V4(Ipv4Addr::LOCALHOST), 1234));
        assert!(!set.allows(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 53));
    }
}
