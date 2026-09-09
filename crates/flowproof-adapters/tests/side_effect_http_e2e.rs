//! The `http_request` capture against a real kernel: an ALLOWED non-loopback
//! connect must land in the egress log's `observed` list, and observation-only
//! supervision must report its own tier, never `enforced`. The unit tests in
//! `egress_linux` prove the capture point as data; only a live filter proves
//! `handle_connect` reaches it.

#![cfg(all(target_os = "linux", feature = "agent"))]

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::time::Duration;

use flowproof_adapters::agent_runner::run_against_contained;
use flowproof_adapters::egress::{AllowSet, Containment};
use flowproof_adapters::AgentProxy;

/// Points the CHILD half of the re-exec below at the listener; unset on an
/// ordinary test run, so the child test passes empty there.
const CHILD_CONNECT_VAR: &str = "FLOWPROOF_SIDE_EFFECT_E2E_CONNECT";

#[test]
fn the_child_half_connects_where_the_env_points() {
    if let Ok(addr) = std::env::var(CHILD_CONNECT_VAR) {
        let _ = std::net::TcpStream::connect(addr.as_str());
    }
}

/// This host's primary non-loopback IPv4, via the packet-free UDP-connect
/// trick; loopback would not exercise the capture.
fn host_ipv4() -> Option<Ipv4Addr> {
    let sock = UdpSocket::bind("0.0.0.0:0").ok()?;
    sock.connect("8.8.8.8:80").ok()?;
    match sock.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() => Some(ip),
        _ => None,
    }
}

#[test]
fn an_allowed_connect_is_observed_and_the_tier_stays_honest() {
    let Some(ip) = host_ipv4() else {
        eprintln!("skipping: this host has no non-loopback IPv4 interface");
        return;
    };
    let listener = TcpListener::bind((ip, 0)).expect("bind non-loopback listener");
    let addr = listener.local_addr().expect("addr").to_string();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(4).flatten() {
            drop(stream);
        }
    });

    // The full observation-only runner path, this test binary re-exec'd as
    // the agent.
    let exe = std::env::current_exe().expect("test binary path");
    let command = format!(
        "{} the_child_half_connects_where_the_env_points --exact",
        exe.display()
    );
    let env = BTreeMap::from([(CHILD_CONNECT_VAR.to_string(), addr.clone())]);
    let proxy = AgentProxy::start(Default::default(), BTreeMap::new(), 0).expect("proxy");
    let run = run_against_contained(
        &proxy,
        &command,
        &env,
        Duration::from_secs(60),
        &AllowSet::allow_all(),
        /* egress_engaged: */ false,
    )
    .expect("run");

    // The tier pin: the filter watched (`observed`), but a wildcard policy
    // nobody declared must never read as enforcement.
    assert!(run.observed, "the filter watched, so the run was observed");
    assert_eq!(run.containment, Some(Containment::observation_only()));

    let egress = &run.egress;
    assert!(
        egress.blocked.is_empty(),
        "allow-all denies nothing on the policy path: {:?}",
        egress.blocked
    );
    let event = (egress.observed.iter())
        .find(|e| e.destination == addr)
        .unwrap_or_else(|| {
            panic!(
                "the allowed connect must be observed; log: {:?} / faults: {:?}",
                egress.observed, egress.faults
            )
        });
    assert_eq!(event.protocol, "tcp");
}
