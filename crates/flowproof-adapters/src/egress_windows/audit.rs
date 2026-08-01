//! Reading the audit lane back: which destinations the kernel actually
//! refused.
//!
//! On Windows this is the ONLY witness. UDP `send_to` returns SUCCESS on a
//! datagram the kernel drops, so `assert_no_egress` cannot be implemented
//! against client-side errors the way a naive port would (spike negative
//! finding 4.2). What the kernel recorded is the evidence.
//!
//! # Attribution is by FILTER ID, not by SID
//!
//! `FwpmNetEventEnum` returns the whole host's event buffer - every user, every
//! process. The spike filtered by the run identity's SID, which is workable in
//! a controlled probe and wrong here for two reasons. A drop record whose
//! `USER_ID_SET` flag is absent cannot be attributed at all, and there is no
//! safe answer: counting it fails runs over someone else's traffic, ignoring it
//! risks passing over our own.
//!
//! Our own filter ids have neither problem. A drop carrying one is ours by
//! construction - no inference, no unattributable middle case. The SID is kept
//! as corroboration, never as the test.
//!
//! A timestamp lower bound is applied as well, because filter ids are allocated
//! by the kernel and can be reused once a session closes. Two runs on one host
//! is the CI case, not an exotic one.

use std::ffi::c_void;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::PSID;
use windows::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

use super::wfp::explain_fwp;
use super::WinErr;
use flowproof_trace::egress::EgressEvent;

/// One drop the kernel attributed to one of our filters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Drop_ {
    pub destination: String,
    pub protocol: String,
    pub at_ms: u64,
    /// The SID the kernel attributed it to, when it set one. Corroboration
    /// only - attribution is by filter id.
    pub user_sid: Option<String>,
}

impl From<Drop_> for EgressEvent {
    fn from(d: Drop_) -> Self {
        EgressEvent {
            destination: d.destination,
            protocol: d.protocol,
            at_ms: d.at_ms,
        }
    }
}

/// Read every drop attributable to `our_filters` and recorded at or after
/// `since_filetime`.
///
/// `since_filetime` is a Windows FILETIME (100ns ticks since 1601), taken when
/// the filters were installed; `GetSystemTimeAsFileTime` is the source.
pub fn drops_for(our_filters: &[u64], since_filetime: u64) -> Result<Vec<Drop_>, WinErr> {
    if our_filters.is_empty() {
        return Ok(Vec::new());
    }
    let mut engine = HANDLE::default();
    let rc = unsafe {
        FwpmEngineOpen0(
            PCWSTR::null(),
            RPC_C_AUTHN_DEFAULT as u32,
            None,
            None,
            &mut engine,
        )
    };
    if rc != 0 {
        return Err(WinErr::new(
            "FwpmEngineOpen0(audit readback)",
            rc,
            explain_fwp(rc),
        ));
    }
    let mut enum_handle = HANDLE::default();
    let rc = unsafe { FwpmNetEventCreateEnumHandle0(engine, None, &mut enum_handle) };
    if rc != 0 {
        unsafe { FwpmEngineClose0(engine) };
        return Err(WinErr::new(
            "FwpmNetEventCreateEnumHandle0",
            rc,
            explain_fwp(rc),
        ));
    }

    let mut out = Vec::new();
    loop {
        let mut entries: *mut *mut FWPM_NET_EVENT5 = std::ptr::null_mut();
        let mut num = 0u32;
        let rc = unsafe { FwpmNetEventEnum5(engine, enum_handle, 128, &mut entries, &mut num) };
        if rc != 0 || num == 0 || entries.is_null() {
            break;
        }
        for i in 0..num as usize {
            let ev = unsafe { &**entries.add(i) };
            if let Some(d) = unsafe { decode_drop(ev, our_filters, since_filetime) } {
                out.push(d);
            }
        }
        unsafe { FwpmFreeMemory0(&mut (entries as *mut c_void)) };
        if num < 128 {
            break;
        }
    }

    unsafe {
        FwpmNetEventDestroyEnumHandle0(engine, enum_handle);
        FwpmEngineClose0(engine);
    }
    Ok(out)
}

/// Decode one event, keeping it only if OUR filter dropped it after the run
/// began.
///
/// # Safety
/// `e` must be a live event from the enumeration.
unsafe fn decode_drop(e: &FWPM_NET_EVENT5, our_filters: &[u64], since: u64) -> Option<Drop_> {
    if e.r#type != FWPM_NET_EVENT_TYPE_CLASSIFY_DROP {
        return None;
    }
    let detail = unsafe { e.Anonymous.classifyDrop };
    if detail.is_null() {
        return None;
    }
    let detail = unsafe { &*detail };
    if !our_filters.contains(&detail.filterId) {
        return None;
    }

    let h = &e.header;
    let stamp = filetime(h.timeStamp);
    if stamp < since {
        return None;
    }

    let flags = h.flags;
    let addr = if flags & FWPM_NET_EVENT_FLAG_REMOTE_ADDR_SET == 0 {
        None
    } else if h.ipVersion == FWP_IP_VERSION_V4 {
        Some(IpAddr::V4(Ipv4Addr::from(unsafe {
            h.Anonymous2.remoteAddrV4
        })))
    } else {
        Some(IpAddr::V6(Ipv6Addr::from(unsafe {
            h.Anonymous2.remoteAddrV6.byteArray16
        })))
    };
    let port = if flags & FWPM_NET_EVENT_FLAG_REMOTE_PORT_SET != 0 {
        Some(h.remotePort)
    } else {
        None
    };
    let user_sid = if flags & FWPM_NET_EVENT_FLAG_USER_ID_SET != 0 && !h.userId.is_null() {
        sid_to_string(PSID(h.userId as *mut c_void))
    } else {
        None
    };

    Some(Drop_ {
        destination: destination_of(addr, port),
        protocol: protocol_of(flags, h.ipProtocol),
        at_ms: stamp.saturating_sub(since) / 10_000,
        user_sid,
    })
}

/// Render a destination, saying what is missing rather than inventing it.
///
/// A record without an address is still evidence that something was refused,
/// so it is reported as unknown rather than discarded - discarding it would
/// turn a real denial into silence.
pub fn destination_of(addr: Option<IpAddr>, port: Option<u16>) -> String {
    match (addr, port) {
        (Some(a), Some(p)) => format!("{a}:{p}"),
        (Some(a), None) => format!("{a}:?"),
        (None, Some(p)) => format!("?:{p}"),
        (None, None) => "?".to_string(),
    }
}

/// The protocol name, or `?` when the kernel did not set the field. Never
/// guessed: "we do not know" and "it was TCP" are different findings.
pub fn protocol_of(flags: u32, proto: u8) -> String {
    if flags & FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET == 0 {
        return "?".to_string();
    }
    match proto {
        6 => "tcp".to_string(),
        17 => "udp".to_string(),
        other => format!("ip-proto-{other}"),
    }
}

fn filetime(ft: windows::Win32::Foundation::FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64
}

/// The current time as a FILETIME, for the lower bound handed to
/// [`drops_for`].
pub fn now_filetime() -> u64 {
    filetime(unsafe { windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime() })
}

fn sid_to_string(sid: PSID) -> Option<String> {
    let mut s = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut s) }.ok()?;
    let out = unsafe { s.to_string() }.ok();
    unsafe {
        windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
            s.0 as *mut c_void,
        )))
    };
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With no filters of our own there is nothing attributable, so the lane
    /// is empty WITHOUT enumerating - and the caller must treat an empty
    /// filter list as "we installed nothing", not "nothing was blocked".
    #[test]
    fn no_filters_of_ours_means_nothing_to_attribute() {
        assert_eq!(drops_for(&[], 0).expect("no enumeration"), Vec::new());
    }

    /// A missing field is reported as unknown, never guessed. "We do not know
    /// the protocol" and "it was TCP" are different findings, and a lane that
    /// invented the second would be evidence of something that never happened.
    #[test]
    fn missing_fields_are_unknown_rather_than_invented() {
        assert_eq!(protocol_of(0, 6), "?", "flag unset means unknown");
        assert_eq!(protocol_of(FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET, 6), "tcp");
        assert_eq!(protocol_of(FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET, 17), "udp");
        // An unmodelled protocol keeps its number rather than becoming "?":
        // we DO know it, we just have no name.
        assert_eq!(
            protocol_of(FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET, 1),
            "ip-proto-1"
        );
    }

    /// A record with no address is still a denial. Discarding it would turn a
    /// real refusal into silence, which is the direction that produces a false
    /// green.
    #[test]
    fn a_destination_says_which_half_is_missing() {
        let v4 = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 9));
        assert_eq!(destination_of(Some(v4), Some(443)), "198.51.100.9:443");
        assert_eq!(destination_of(Some(v4), None), "198.51.100.9:?");
        assert_eq!(destination_of(None, Some(443)), "?:443");
        assert_eq!(destination_of(None, None), "?");
    }

    /// The lane folds into the same shape the Linux supervisor produces, so
    /// `assert_no_egress` sees one kind of evidence on both platforms.
    #[test]
    fn a_drop_converts_to_the_cross_platform_event() {
        let d = Drop_ {
            destination: "198.51.100.9:443".into(),
            protocol: "tcp".into(),
            at_ms: 42,
            user_sid: Some("S-1-5-21-1-2-3-1006".into()),
        };
        let e: EgressEvent = d.into();
        assert_eq!(e.destination, "198.51.100.9:443");
        assert_eq!(e.protocol, "tcp");
        assert_eq!(e.at_ms, 42);
    }
}
