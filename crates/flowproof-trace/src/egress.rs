//! The `allow_egress` grammar and the on-trace egress event shape, shared by
//! spec validation (`flowproof-agent`) and the runtime supervisor
//! (`flowproof-adapters`) so the two never drift on what an entry means.
//!
//! An entry names a destination the contained agent is allowed to reach:
//!
//! ```text
//! api.example.com:443     # host:port
//! api.example.com         # bare host, any port
//! 198.51.100.9:443        # ip:port
//! 198.51.100.9            # bare ip, any port
//! 10.0.0.0/8:443          # cidr:port
//! [2001:db8::1]:443       # bracketed ipv6 with port
//! ${SERVICE_HOST}:443     # ${VAR} ref, resolved at execution, never stored
//! ```
//!
//! Parsing runs on RESOLVED text; validation (which may still carry a
//! `${VAR}`) defers the deep check to execution, exactly like every other
//! secret-bearing field in a spec.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// One denied egress attempt, recorded into the trace's egress lane. `at_ms`
/// is monotonic milliseconds since agent spawn - NEVER wall clock, so a
/// re-record does not churn the lane on timing alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressEvent {
    /// The destination the child tried to reach, `ip:port`.
    pub destination: String,
    /// The transport, `tcp` or `udp`.
    pub protocol: String,
    /// Monotonic milliseconds since the agent was spawned.
    pub at_ms: u64,
}

/// How an entry's host part matches: a DNS name (resolved to an IP set once
/// at run start), a single literal IP, or a CIDR network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMatch {
    Host(String),
    Ip(IpAddr),
    /// Base address and prefix length in bits.
    Cidr(IpAddr, u8),
}

/// A parsed `allow_egress` entry: a host matcher and an optional port
/// (absent = any port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowEntry {
    pub host: HostMatch,
    pub port: Option<u16>,
}

impl AllowEntry {
    /// Does this entry's port constraint admit `port`? An entry with no port
    /// admits every port.
    pub fn port_ok(&self, port: u16) -> bool {
        self.port.map_or(true, |p| p == port)
    }
}

/// Validate one `allow_egress` entry at spec time. An entry carrying a
/// `${VAR}` is accepted here and deep-checked at execution once resolved -
/// the same deferral every secret-bearing spec field uses. A concrete entry
/// is parsed in full, and a malformed one is rejected NAMING the entry.
pub fn validate_allow_entry(entry: &str) -> Result<(), String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err("an `allow_egress` entry is blank".to_string());
    }
    if trimmed.contains("${") {
        // A `${VAR}` ref: its resolved value is unknown until execution, so
        // the structural check waits for `parse_allow_entry` on the resolved
        // text. Storing it raw is deliberate - a resolved allow-list would
        // leak the destination into the trace.
        return Ok(());
    }
    parse_allow_entry(trimmed).map(|_| ())
}

/// Parse one RESOLVED `allow_egress` entry. Never call this with an
/// unresolved `${VAR}` in it - resolve first.
pub fn parse_allow_entry(entry: &str) -> Result<AllowEntry, String> {
    let trimmed = entry.trim();
    let bad = |why: &str| format!("`{trimmed}` is not a valid allow_egress entry: {why}");

    if trimmed.is_empty() {
        return Err(bad("it is empty"));
    }

    // CIDR: the only form carrying a `/`. Shape is `addr/prefix` or
    // `addr/prefix:port` (an IPv4 cidr with a port). An IPv6 cidr keeps its
    // port in brackets, handled below when there is no `/`.
    if let Some((addr_str, rest)) = trimmed.split_once('/') {
        let (prefix_str, port) = match rest.split_once(':') {
            Some((prefix, port)) => (prefix, Some(parse_port(port).map_err(|e| bad(&e))?)),
            None => (rest, None),
        };
        let base: IpAddr = addr_str
            .parse()
            .map_err(|_| bad(&format!("`{addr_str}` is not an IP address")))?;
        let prefix: u8 = prefix_str
            .parse()
            .map_err(|_| bad(&format!("`{prefix_str}` is not a prefix length")))?;
        let max = if base.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(bad(&format!(
                "a /{prefix} prefix is out of range (max /{max})"
            )));
        }
        return Ok(AllowEntry {
            host: HostMatch::Cidr(base, prefix),
            port,
        });
    }

    // Bracketed IPv6, with or without a port: `[::1]` or `[::1]:443`.
    if let Some(after_bracket) = trimmed.strip_prefix('[') {
        let (inside, tail) = after_bracket
            .split_once(']')
            .ok_or_else(|| bad("an opening `[` with no closing `]`"))?;
        let ip: IpAddr = inside
            .parse()
            .map_err(|_| bad(&format!("`{inside}` is not an IPv6 address")))?;
        let port = match tail {
            "" => None,
            _ => {
                let port = tail
                    .strip_prefix(':')
                    .ok_or_else(|| bad("expected `:port` after `]`"))?;
                Some(parse_port(port).map_err(|e| bad(&e))?)
            }
        };
        return Ok(AllowEntry {
            host: HostMatch::Ip(ip),
            port,
        });
    }

    // A bare IP (v4 or v6) parses whole - a bare IPv6's colons are its own,
    // not a port separator.
    if let Ok(ip) = trimmed.parse::<IpAddr>() {
        return Ok(AllowEntry {
            host: HostMatch::Ip(ip),
            port: None,
        });
    }

    // What remains is `host:port`, `ip:port`, or a bare host. A single `:`
    // splits the port; a host has none.
    match trimmed.rsplit_once(':') {
        Some((host, port)) => {
            let port = parse_port(port).map_err(|e| bad(&e))?;
            let host = parse_host(host).map_err(|e| bad(&e))?;
            Ok(AllowEntry {
                host,
                port: Some(port),
            })
        }
        None => Ok(AllowEntry {
            host: parse_host(trimmed).map_err(|e| bad(&e))?,
            port: None,
        }),
    }
}

/// Parse the host part of an entry: a literal IP, or a DNS name.
fn parse_host(host: &str) -> Result<HostMatch, String> {
    if host.is_empty() {
        return Err("the host is empty".to_string());
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(HostMatch::Ip(ip));
    }
    if is_valid_hostname(host) {
        Ok(HostMatch::Host(host.to_string()))
    } else {
        Err(format!("`{host}` is not a host name or IP"))
    }
}

fn parse_port(port: &str) -> Result<u16, String> {
    let n: u32 = port
        .parse()
        .map_err(|_| format!("`{port}` is not a port number"))?;
    if n == 0 || n > 65535 {
        return Err(format!("`{port}` is not a port in 1..=65535"));
    }
    Ok(n as u16)
}

/// A permissive host-name check: labels of letters, digits, hyphen and
/// underscore, joined by dots. Not a full RFC 1123 validator - just enough
/// to reject an obviously malformed entry rather than accept anything.
fn is_valid_hostname(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host.split('.').all(|label| {
            !label.is_empty()
                && label
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
}

/// Does the CIDR network `base`/`prefix` contain `ip`? A network and an
/// address of different families never match.
pub fn cidr_contains(base: IpAddr, prefix: u8, ip: IpAddr) -> bool {
    match (base, ip) {
        (IpAddr::V4(base), IpAddr::V4(ip)) => {
            if prefix > 32 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u32::MAX << (32 - prefix)
            };
            (u32::from(base) & mask) == (u32::from(ip) & mask)
        }
        (IpAddr::V6(base), IpAddr::V6(ip)) => {
            if prefix > 128 {
                return false;
            }
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            (u128::from(base) & mask) == (u128::from(ip) & mask)
        }
        _ => false,
    }
}

/// Is `ip` a loopback address? Loopback is exempt from containment wholesale
/// (127/8 and ::1), including a v4-mapped-v6 loopback.
pub fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6
                    .to_ipv4_mapped()
                    .map(|m| m.is_loopback())
                    .unwrap_or(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn host_port_forms_parse() {
        assert_eq!(
            parse_allow_entry("api.example.com:443").expect("parses"),
            AllowEntry {
                host: HostMatch::Host("api.example.com".into()),
                port: Some(443),
            }
        );
        assert_eq!(
            parse_allow_entry("api.example.com").expect("parses"),
            AllowEntry {
                host: HostMatch::Host("api.example.com".into()),
                port: None,
            }
        );
    }

    #[test]
    fn ip_forms_parse() {
        assert_eq!(
            parse_allow_entry("198.51.100.9:443").expect("parses"),
            AllowEntry {
                host: HostMatch::Ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9))),
                port: Some(443),
            }
        );
        assert_eq!(
            parse_allow_entry("198.51.100.9").expect("parses").host,
            HostMatch::Ip(IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9)))
        );
        // A bare IPv6 keeps all its colons.
        assert_eq!(
            parse_allow_entry("2001:db8::1").expect("parses").host,
            HostMatch::Ip(IpAddr::V6("2001:db8::1".parse::<Ipv6Addr>().unwrap()))
        );
        // Bracketed IPv6 with a port.
        assert_eq!(
            parse_allow_entry("[2001:db8::1]:443").expect("parses"),
            AllowEntry {
                host: HostMatch::Ip(IpAddr::V6("2001:db8::1".parse().unwrap())),
                port: Some(443),
            }
        );
    }

    #[test]
    fn cidr_forms_parse() {
        assert_eq!(
            parse_allow_entry("10.0.0.0/8:443").expect("parses"),
            AllowEntry {
                host: HostMatch::Cidr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)), 8),
                port: Some(443),
            }
        );
        assert_eq!(
            parse_allow_entry("10.0.0.0/8").expect("parses").host,
            HostMatch::Cidr(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0)), 8)
        );
    }

    #[test]
    fn malformed_entries_are_rejected_naming_the_entry() {
        for bad in [
            "",
            "host:0",
            "host:70000",
            "host:notaport",
            "10.0.0.0/40",
            "[2001:db8::1",
            "bad host name:443",
        ] {
            let err = parse_allow_entry(bad).expect_err(bad);
            if !bad.is_empty() {
                assert!(err.contains(bad.trim()), "`{bad}` -> {err}");
            }
        }
    }

    #[test]
    fn a_var_ref_defers_validation() {
        assert_eq!(validate_allow_entry("${SERVICE_HOST}:443"), Ok(()));
        assert_eq!(validate_allow_entry("host.example.com:443"), Ok(()));
        assert!(validate_allow_entry("10.0.0.0/40").is_err());
        assert!(validate_allow_entry("  ").is_err());
    }

    #[test]
    fn cidr_containment_is_family_aware_and_masked() {
        let base = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 0));
        assert!(cidr_contains(
            base,
            8,
            IpAddr::V4(Ipv4Addr::new(10, 5, 6, 7))
        ));
        assert!(!cidr_contains(
            base,
            8,
            IpAddr::V4(Ipv4Addr::new(11, 0, 0, 1))
        ));
        // A /0 admits everything of the same family, nothing of the other.
        assert!(cidr_contains(
            base,
            0,
            IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4))
        ));
        assert!(!cidr_contains(base, 0, IpAddr::V6(Ipv6Addr::LOCALHOST)));
    }

    #[test]
    fn loopback_covers_v4_v8_and_mapped_v6() {
        assert!(is_loopback(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_loopback(IpAddr::V4(Ipv4Addr::new(127, 5, 6, 7))));
        assert!(is_loopback(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_loopback(IpAddr::V6("::ffff:127.0.0.1".parse().unwrap())));
        assert!(!is_loopback(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
    }
}
