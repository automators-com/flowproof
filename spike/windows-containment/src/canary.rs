//! The canary: what actually runs *inside* the boundary.
//!
//! Honesty rule 8 — the mechanism is proven live every run, never inferred from
//! `FwpmFilterAdd0` returning success. So the canary always does all three:
//! a probe to a declared destination that must succeed, a probe to an
//! undeclared one that must fail, and (in the supervisor) the drop turning up
//! in the audit lane.
//!
//! It prints one `CANARY|` line per probe with the raw OS error number, never
//! an interpretation. Honesty rule 1: "it failed" is not evidence, because a
//! usage error fails too. The error *number* is a second layer only a real
//! attempt can produce.

use std::io::Write;
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::time::Duration;

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(4);

/// Result of one probe, in a form that survives being read out of a CI log.
pub struct ProbeResult {
    pub label: String,
    pub target: String,
    pub ok: bool,
    pub os_error: i32,
}

impl ProbeResult {
    pub fn print(&self) {
        println!(
            "CANARY|{}|{}|{}|os_error={}",
            self.label,
            self.target,
            if self.ok { "CONNECTED" } else { "REFUSED" },
            self.os_error
        );
        let _ = std::io::stdout().flush();
    }
}

pub fn tcp_probe(label: &str, target: SocketAddr) -> ProbeResult {
    match TcpStream::connect_timeout(&target, CONNECT_TIMEOUT) {
        Ok(mut s) => {
            // Write a byte so the destination-side oracle sees a real
            // connection, not just a SYN that a half-open scan would produce.
            let _ = s.write_all(b"fp-spike\n");
            ProbeResult {
                label: label.to_string(),
                target: target.to_string(),
                ok: true,
                os_error: 0,
            }
        }
        Err(e) => ProbeResult {
            label: label.to_string(),
            target: target.to_string(),
            ok: false,
            os_error: e.raw_os_error().unwrap_or(-1),
        },
    }
}

/// UDP has no handshake, so "blocked" shows up as `send_to` itself failing.
/// That is the interesting question: ALE_AUTH_CONNECT is documented to fire on
/// the first UDP send, and if it does not, a contained agent could exfiltrate
/// over UDP while every TCP probe looked clean.
pub fn udp_probe(label: &str, target: SocketAddr) -> ProbeResult {
    let bind: SocketAddr = if target.is_ipv4() {
        "0.0.0.0:0".parse().expect("static addr")
    } else {
        "[::]:0".parse().expect("static addr")
    };
    match UdpSocket::bind(bind) {
        Ok(s) => match s.send_to(b"fp-spike", target) {
            Ok(_) => ProbeResult {
                label: label.to_string(),
                target: target.to_string(),
                ok: true,
                os_error: 0,
            },
            Err(e) => ProbeResult {
                label: label.to_string(),
                target: target.to_string(),
                ok: false,
                os_error: e.raw_os_error().unwrap_or(-1),
            },
        },
        Err(e) => ProbeResult {
            label: format!("{label}(bind-failed)"),
            target: target.to_string(),
            ok: false,
            os_error: e.raw_os_error().unwrap_or(-1),
        },
    }
}

/// Everything the canary does, driven by a config handed to it on the command
/// line. Run in the child; also run by the grandchild with `--prefix
/// grandchild`.
pub fn run(args: &[String]) -> i32 {
    let mut declared: Option<SocketAddr> = None;
    let mut undeclared: Option<SocketAddr> = None;
    let mut udp_undeclared: Option<SocketAddr> = None;
    let mut external: Option<SocketAddr> = None;
    let mut spawn_grandchild = false;
    let mut prefix = "child".to_string();

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--declared" => declared = it.next().and_then(|v| v.parse().ok()),
            "--undeclared" => undeclared = it.next().and_then(|v| v.parse().ok()),
            "--udp-undeclared" => udp_undeclared = it.next().and_then(|v| v.parse().ok()),
            "--external" => external = it.next().and_then(|v| v.parse().ok()),
            "--spawn-grandchild" => spawn_grandchild = true,
            "--prefix" => prefix = it.next().cloned().unwrap_or_default(),
            _ => {}
        }
    }

    println!("CANARY|START|prefix={prefix}|pid={}", std::process::id());
    println!("CANARY|WHOAMI|{}|{}", prefix, whoami());

    if let Some(t) = declared {
        tcp_probe(&format!("{prefix}.tcp.declared"), t).print();
    }
    if let Some(t) = undeclared {
        tcp_probe(&format!("{prefix}.tcp.undeclared"), t).print();
    }
    if let Some(t) = udp_undeclared {
        udp_probe(&format!("{prefix}.udp.undeclared"), t).print();
    }
    if let Some(t) = external {
        tcp_probe(&format!("{prefix}.tcp.external"), t).print();
    }

    if spawn_grandchild {
        // Through `cmd.exe`, deliberately. The whole reason a per-run identity
        // beats an app-id filter is that `agent -> python.exe -> curl.exe`
        // escapes app-id scoping; a grandchild behind a shell is the cheapest
        // faithful model of that.
        let exe = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let mut cmd = grandchild_command(&exe);
        cmd.arg("canary").arg("--prefix").arg("grandchild");
        if let Some(t) = undeclared {
            cmd.arg("--undeclared").arg(t.to_string());
        }
        if let Some(t) = declared {
            cmd.arg("--declared").arg(t.to_string());
        }
        match cmd.status() {
            Ok(st) => println!("CANARY|GRANDCHILD-EXIT|{:?}", st.code()),
            Err(e) => println!("CANARY|GRANDCHILD-SPAWN-FAILED|{e}"),
        }
    }

    println!("CANARY|DONE|prefix={prefix}");
    let _ = std::io::stdout().flush();
    0
}

#[cfg(windows)]
fn grandchild_command(exe: &str) -> std::process::Command {
    let mut c = std::process::Command::new("cmd.exe");
    c.arg("/c").arg(exe);
    c
}

#[cfg(not(windows))]
fn grandchild_command(exe: &str) -> std::process::Command {
    let mut c = std::process::Command::new("/bin/sh");
    c.arg("-c").arg(exe);
    c
}

#[cfg(windows)]
fn whoami() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "<unknown>".into())
}

#[cfg(not(windows))]
fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "<unknown>".into())
}
