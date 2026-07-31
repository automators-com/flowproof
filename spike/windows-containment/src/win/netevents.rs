//! Day 4 — the audit lane.
//!
//! A containment claim that cannot say *what* was refused is worth much less
//! than one that can. This subscribes to WFP's net-event stream and keeps the
//! drop records, then checks the thing that actually decides audit quality:
//! does the record carry the remote address and port, and is it attributable to
//! the run identity?
//!
//! Both paths are exercised in the same run — the live subscription
//! (`FwpmNetEventSubscribe4`) and the enumeration (`FwpmNetEventEnum5`) — for
//! one practical reason: a missing log line costs a full CI cycle, and if the
//! subscription turns out to deliver nothing, the enumeration still tells us
//! whether the kernel recorded the drop at all. Those are very different
//! failures and worth telling apart on the first iteration rather than the
//! second.

use std::ffi::c_void;
use std::net::Ipv4Addr;
use std::sync::{Mutex, OnceLock};

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::PSID;
use windows::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

use super::WinErr;

#[derive(Clone, Debug)]
pub struct NetEventRecord {
    pub kind: String,
    pub remote_addr: Option<Ipv4Addr>,
    pub remote_port: Option<u16>,
    pub ip_protocol: Option<u8>,
    pub user_sid: Option<String>,
    pub filter_id: Option<u64>,
    pub layer_id: Option<u16>,
    pub is_loopback: Option<bool>,
    /// Which fields the kernel actually said it had set. Printed verbatim so a
    /// later reader can tell "the field was zero" from "the field was absent".
    pub flags: u32,
}

fn sink() -> &'static Mutex<Vec<NetEventRecord>> {
    static SINK: OnceLock<Mutex<Vec<NetEventRecord>>> = OnceLock::new();
    SINK.get_or_init(|| Mutex::new(Vec::new()))
}

pub fn drain() -> Vec<NetEventRecord> {
    sink()
        .lock()
        .map(|mut g| std::mem::take(&mut *g))
        .unwrap_or_default()
}

pub fn snapshot() -> Vec<NetEventRecord> {
    sink().lock().map(|g| g.clone()).unwrap_or_default()
}

/// # Safety
/// Called by WFP on its own thread with a pointer valid for the call only.
unsafe extern "system" fn callback(_ctx: *mut c_void, event: *const FWPM_NET_EVENT5) {
    if event.is_null() {
        return;
    }
    let rec = unsafe { decode(&*event) };
    if let Ok(mut g) = sink().lock() {
        g.push(rec);
    }
}

unsafe fn decode(e: &FWPM_NET_EVENT5) -> NetEventRecord {
    let h = &e.header;
    let flags = h.flags;

    let remote_addr =
        if flags & FWPM_NET_EVENT_FLAG_REMOTE_ADDR_SET != 0 && h.ipVersion == FWP_IP_VERSION_V4 {
            Some(Ipv4Addr::from(unsafe { h.Anonymous2.remoteAddrV4 }))
        } else {
            None
        };
    let remote_port = if flags & FWPM_NET_EVENT_FLAG_REMOTE_PORT_SET != 0 {
        Some(h.remotePort)
    } else {
        None
    };
    let ip_protocol = if flags & FWPM_NET_EVENT_FLAG_IP_PROTOCOL_SET != 0 {
        Some(h.ipProtocol)
    } else {
        None
    };
    let user_sid = if flags & FWPM_NET_EVENT_FLAG_USER_ID_SET != 0 && !h.userId.is_null() {
        sid_to_string(PSID(h.userId as *mut c_void))
    } else {
        None
    };

    let (kind, filter_id, layer_id, is_loopback) = match e.r#type {
        FWPM_NET_EVENT_TYPE_CLASSIFY_DROP => {
            let d = unsafe { e.Anonymous.classifyDrop };
            if d.is_null() {
                ("classify-drop(no-detail)".to_string(), None, None, None)
            } else {
                let d = unsafe { &*d };
                (
                    "classify-drop".to_string(),
                    Some(d.filterId),
                    Some(d.layerId),
                    Some(d.isLoopback.as_bool()),
                )
            }
        }
        FWPM_NET_EVENT_TYPE_CLASSIFY_ALLOW => {
            let a = unsafe { e.Anonymous.classifyAllow };
            if a.is_null() {
                ("classify-allow(no-detail)".to_string(), None, None, None)
            } else {
                let a = unsafe { &*a };
                (
                    "classify-allow".to_string(),
                    Some(a.filterId),
                    Some(a.layerId),
                    None,
                )
            }
        }
        other => (format!("other({})", other.0), None, None, None),
    };

    NetEventRecord {
        kind,
        remote_addr,
        remote_port,
        ip_protocol,
        user_sid,
        filter_id,
        layer_id,
        is_loopback,
        flags,
    }
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

pub struct Subscription {
    engine: HANDLE,
    handle: HANDLE,
}

impl Subscription {
    /// Subscribe on the engine handle whose session added the filters, so the
    /// events are attributable to this run rather than to whatever else the
    /// machine is doing.
    pub fn start(engine: HANDLE, session_key: GUID) -> Result<Self, WinErr> {
        let sub = FWPM_NET_EVENT_SUBSCRIPTION0 {
            enumTemplate: std::ptr::null_mut(),
            flags: 0,
            sessionKey: session_key,
        };
        let mut handle = HANDLE::default();
        let rc = unsafe { FwpmNetEventSubscribe4(engine, &sub, Some(callback), None, &mut handle) };
        if rc != 0 {
            return Err(WinErr::new("FwpmNetEventSubscribe4", rc, String::new()));
        }
        Ok(Self { engine, handle })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        unsafe { FwpmNetEventUnsubscribe0(self.engine, self.handle) };
    }
}

/// Read the kernel's stored net-event log directly, independent of the
/// subscription.
pub fn enumerate() -> Result<Vec<NetEventRecord>, WinErr> {
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
        return Err(WinErr::new("FwpmEngineOpen0(netevent)", rc, String::new()));
    }
    let mut enum_handle = HANDLE::default();
    let rc = unsafe { FwpmNetEventCreateEnumHandle0(engine, None, &mut enum_handle) };
    if rc != 0 {
        unsafe { FwpmEngineClose0(engine) };
        return Err(WinErr::new(
            "FwpmNetEventCreateEnumHandle0",
            rc,
            String::new(),
        ));
    }

    let mut out = Vec::new();
    loop {
        let mut entries: *mut *mut FWPM_NET_EVENT5 = std::ptr::null_mut();
        let mut num: u32 = 0;
        let rc = unsafe { FwpmNetEventEnum5(engine, enum_handle, 128, &mut entries, &mut num) };
        if rc != 0 || num == 0 {
            break;
        }
        for i in 0..num as usize {
            let ev = unsafe { &**entries.add(i) };
            out.push(unsafe { decode(ev) });
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

impl NetEventRecord {
    pub fn line(&self) -> String {
        format!(
            "kind={} remote={} port={} proto={} sid={} filter_id={} layer={} loopback={} flags=0x{:x}",
            self.kind,
            self.remote_addr
                .map(|a| a.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.remote_port
                .map(|p| p.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.ip_protocol
                .map(|p| p.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.user_sid.clone().unwrap_or_else(|| "<unset>".into()),
            self.filter_id
                .map(|f| f.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.layer_id
                .map(|l| l.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.is_loopback
                .map(|b| b.to_string())
                .unwrap_or_else(|| "<unset>".into()),
            self.flags
        )
    }
}
