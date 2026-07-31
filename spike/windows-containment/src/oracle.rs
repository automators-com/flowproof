//! The destination-side oracle.
//!
//! Honesty rule 2: a blocked connection has to be confirmed by the destination
//! never seeing it, not only by the client's error. A client-side error code is
//! consistent with containment *and* with a routing failure, a wrong port, or a
//! typo in the harness — so on its own it proves nothing.
//!
//! So the supervisor runs the destinations itself and records every peer that
//! actually arrives. Two of them, deliberately:
//!
//!   - one on `127.0.0.1`, which asks whether WFP's ALE layer classifies
//!     loopback at all;
//!   - one on the runner's primary NIC address, which takes the normal path.
//!
//! If those two disagree that is a finding, not a nuisance: an agent that can
//! reach a loopback service the flow never declared is not contained.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
pub struct Sightings(Arc<Mutex<Vec<String>>>);

impl Sightings {
    pub fn all(&self) -> Vec<String> {
        self.0.lock().map(|g| g.clone()).unwrap_or_default()
    }
    pub fn count(&self) -> usize {
        self.0.lock().map(|g| g.len()).unwrap_or(0)
    }
}

pub struct Listener {
    pub addr: SocketAddr,
    pub label: String,
    pub sightings: Sightings,
    stop: Arc<AtomicBool>,
}

impl Listener {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Bind a TCP destination that records every peer it accepts.
pub fn tcp_destination(label: &str, ip: IpAddr) -> std::io::Result<Listener> {
    let l = TcpListener::bind(SocketAddr::new(ip, 0))?;
    let addr = l.local_addr()?;
    l.set_nonblocking(true)?;
    let sightings = Sightings(Arc::new(Mutex::new(Vec::new())));
    let stop = Arc::new(AtomicBool::new(false));

    let s2 = sightings.clone();
    let stop2 = stop.clone();
    let label2 = label.to_string();
    std::thread::spawn(move || {
        while !stop2.load(Ordering::SeqCst) {
            match l.accept() {
                Ok((mut sock, peer)) => {
                    let mut buf = [0u8; 64];
                    let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
                    let n = sock.read(&mut buf).unwrap_or(0);
                    let body = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                    if let Ok(mut g) = s2.0.lock() {
                        g.push(format!("{label2} accepted {peer} payload={body:?}"));
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    });

    Ok(Listener {
        addr,
        label: label.to_string(),
        sightings,
        stop,
    })
}

/// Bind a UDP destination that records every datagram it receives.
pub fn udp_destination(label: &str, ip: IpAddr) -> std::io::Result<Listener> {
    let s = UdpSocket::bind(SocketAddr::new(ip, 0))?;
    let addr = s.local_addr()?;
    s.set_read_timeout(Some(Duration::from_millis(250)))?;
    let sightings = Sightings(Arc::new(Mutex::new(Vec::new())));
    let stop = Arc::new(AtomicBool::new(false));

    let s2 = sightings.clone();
    let stop2 = stop.clone();
    let label2 = label.to_string();
    std::thread::spawn(move || {
        let mut buf = [0u8; 128];
        while !stop2.load(Ordering::SeqCst) {
            if let Ok((n, peer)) = s.recv_from(&mut buf) {
                let body = String::from_utf8_lossy(&buf[..n]).trim().to_string();
                if let Ok(mut g) = s2.0.lock() {
                    g.push(format!("{label2} received {peer} payload={body:?}"));
                }
            }
        }
    });

    Ok(Listener {
        addr,
        label: label.to_string(),
        sightings,
        stop,
    })
}

/// The runner's primary IPv4 address.
///
/// Found by `connect`ing a UDP socket, which picks a source address from the
/// routing table without putting a packet on the wire. Falls back to loopback
/// if there is no route at all, and the caller reports which it got — a run
/// where both destinations were loopback answers a weaker question than one
/// where they differed, and that difference must not be silent.
pub fn primary_ipv4() -> Ipv4Addr {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("192.0.2.1:9")?;
            s.local_addr()
        })
        .ok()
        .and_then(|a| match a.ip() {
            IpAddr::V4(v4) if !v4.is_loopback() && !v4.is_unspecified() => Some(v4),
            _ => None,
        })
        .unwrap_or(Ipv4Addr::LOCALHOST)
}
