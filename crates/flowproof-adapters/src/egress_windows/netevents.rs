//! Turning on the kernel's net-event collection - the audit lane's power
//! switch.
//!
//! # Why this is a separate handle
//!
//! `FwpmEngineSetOption0` **refuses a dynamic session** with
//! `FWP_E_DYNAMIC_SESSION_IN_PROGRESS` (0x8032000B). A dynamic session buys
//! automatic filter teardown; it costs the ability to configure the engine,
//! and those are separate handles for a reason.
//!
//! Getting this wrong is not a loud failure. It is the exact shape of a false
//! green: the option never takes, the kernel records nothing,
//! `FwpmNetEventEnum` returns **zero rows**, and zero rows reads as "nothing
//! was blocked" - which is indistinguishable from a clean run. The spike lost
//! its whole audit lane to this on one run, in which four filters demonstrably
//! dropped traffic and the enumeration still came back empty
//! (`spike/windows-containment/LOG.md`, negative finding 4.2).
//!
//! So a failure to enable collection must reach the caller as an **error**,
//! and the caller must turn it into a fault in the sense of
//! [`crate::egress::EgressLog::faults`] - never a quietly unaudited run that
//! passes. On Windows this is the whole basis of `assert_no_egress`: UDP
//! `send_to` returns SUCCESS on a datagram the kernel drops, so the client
//! side cannot be asked, and the audit lane is the only witness.
//!
//! # Why it is restored
//!
//! Engine options are **machine-wide**, not session-scoped. A test tool that
//! silently leaves a host collecting net events forever has changed the
//! machine, so the previous value is read first and put back on the way out -
//! and only if we were the ones who changed it. If collection was already on,
//! someone else may depend on it and it is left alone.

use windows::core::PCWSTR;
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FwpmEngineClose0, FwpmEngineGetOption0, FwpmEngineOpen0, FwpmEngineSetOption0,
    FWPM_ENGINE_COLLECT_NET_EVENTS, FWPM_ENGINE_NET_EVENT_MATCH_ANY_KEYWORDS,
    FWPM_NET_EVENT_KEYWORD_CLASSIFY_ALLOW, FWP_UINT32, FWP_VALUE0, FWP_VALUE0_0,
};
use windows::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

use super::wfp::explain_fwp;
use super::WinErr;

/// Net-event collection, turned on for the length of a run and put back after.
///
/// Holding this is what makes the audit lane real. Dropping it restores the
/// engine option and closes the handle.
pub struct NetEventCollection {
    handle: HANDLE,
    /// The value collection had before we touched it. `None` means we did not
    /// change it, so there is nothing to restore.
    previous: Option<u32>,
    /// The positive-evidence keyword could not be set.
    ///
    /// Not fatal, and deliberately not an error: drops are collected whenever
    /// collection is on, keyword or not. What is lost is only the explicit
    /// "the permit matched" record - useful as positive evidence that a
    /// declared destination was allowed rather than merely not denied, but not
    /// something `assert_no_egress` rests on.
    pub keyword_warning: Option<String>,
    closed: bool,
}

impl NetEventCollection {
    /// Open a NON-dynamic handle and turn collection on.
    ///
    /// The handle is separate from [`super::wfp::Engine`]'s on purpose - see
    /// the module docs. An `Err` here means the run cannot be audited, which
    /// on Windows means it cannot be certified either.
    pub fn enable() -> Result<Self, WinErr> {
        let mut handle = HANDLE::default();
        // No FWPM_SESSION0: an ordinary session, because a dynamic one cannot
        // set engine options at all.
        let rc = unsafe {
            FwpmEngineOpen0(
                PCWSTR::null(),
                RPC_C_AUTHN_DEFAULT as u32,
                None,
                None,
                &mut handle,
            )
        };
        if rc != 0 {
            return Err(WinErr::new(
                "FwpmEngineOpen0(non-dynamic, for engine options)",
                rc,
                explain_fwp(rc),
            ));
        }

        let previous = get_u32_option(handle, FWPM_ENGINE_COLLECT_NET_EVENTS);
        let already_on = previous == Some(1);

        if !already_on {
            if let Err(e) = set_u32_option(handle, FWPM_ENGINE_COLLECT_NET_EVENTS, 1) {
                unsafe { FwpmEngineClose0(handle) };
                return Err(e);
            }
        }

        // Best effort. A failure loses positive evidence, not the drops.
        let keyword_warning = set_u32_option(
            handle,
            FWPM_ENGINE_NET_EVENT_MATCH_ANY_KEYWORDS,
            FWPM_NET_EVENT_KEYWORD_CLASSIFY_ALLOW,
        )
        .err()
        .map(|e| e.to_string());

        Ok(Self {
            handle,
            // Only remember a previous value if we actually changed it.
            previous: if already_on { None } else { previous },
            keyword_warning,
            closed: false,
        })
    }

    /// Restore the previous option value and close the handle. Idempotent.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        if let Some(prev) = self.previous {
            // Best effort: there is nowhere useful to report a failure from a
            // teardown, and leaving collection on is the safe direction.
            let _ = set_u32_option(self.handle, FWPM_ENGINE_COLLECT_NET_EVENTS, prev);
        }
        unsafe { FwpmEngineClose0(self.handle) };
    }
}

impl Drop for NetEventCollection {
    fn drop(&mut self) {
        self.close();
    }
}

fn get_u32_option(
    engine: HANDLE,
    option: windows::Win32::NetworkManagement::WindowsFilteringPlatform::FWPM_ENGINE_OPTION,
) -> Option<u32> {
    let mut value = std::ptr::null_mut::<FWP_VALUE0>();
    let rc = unsafe { FwpmEngineGetOption0(engine, option, &mut value) };
    if rc != 0 || value.is_null() {
        return None;
    }
    let v = unsafe { &*value };
    let out = if v.r#type == FWP_UINT32 {
        Some(unsafe { v.Anonymous.uint32 })
    } else {
        None
    };
    unsafe {
        windows::Win32::NetworkManagement::WindowsFilteringPlatform::FwpmFreeMemory0(
            &mut (value as *mut core::ffi::c_void),
        )
    };
    out
}

fn set_u32_option(
    engine: HANDLE,
    option: windows::Win32::NetworkManagement::WindowsFilteringPlatform::FWPM_ENGINE_OPTION,
    value: u32,
) -> Result<(), WinErr> {
    let v = FWP_VALUE0 {
        r#type: FWP_UINT32,
        Anonymous: FWP_VALUE0_0 { uint32: value },
    };
    let rc = unsafe { FwpmEngineSetOption0(engine, option, &v) };
    if rc != 0 {
        return Err(WinErr::new(
            "FwpmEngineSetOption0",
            rc,
            format!("option={option:?} value={value} -> {}", explain_fwp(rc)),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state machine that decides whether we put anything back.
    ///
    /// Restoring a value we never set would turn OFF collection somebody else
    /// switched on, which is the one way a test tool can break an unrelated
    /// thing on the host. So `previous` is `Some` only when we changed it.
    #[test]
    fn a_collection_we_did_not_enable_is_not_restored() {
        // Already on: nothing to restore, and nothing to turn off.
        let mut already_on = NetEventCollection {
            handle: HANDLE::default(),
            previous: None,
            keyword_warning: None,
            closed: true,
        };
        assert!(already_on.previous.is_none());
        already_on.close();

        // We turned it on from off: the previous value travels so `close` can
        // put it back.
        let mut we_enabled = NetEventCollection {
            handle: HANDLE::default(),
            previous: Some(0),
            keyword_warning: None,
            closed: true,
        };
        assert_eq!(we_enabled.previous, Some(0));
        we_enabled.close();
    }

    /// The keyword failure is a warning, not an error.
    ///
    /// Drops are collected whenever collection is on. Treating a missing
    /// keyword as fatal would refuse to audit a run that is perfectly
    /// auditable - and since a failure to audit becomes a capability error on
    /// Windows, that would turn a working host into an uncertifiable one.
    #[test]
    fn a_missing_keyword_is_a_warning_not_a_failure() {
        let c = NetEventCollection {
            handle: HANDLE::default(),
            previous: None,
            keyword_warning: Some("FwpmEngineSetOption0 failed: code=5".into()),
            closed: true,
        };
        // It carries the code, so the operator can tell a permissions problem
        // from an unsupported build.
        let warning = c.keyword_warning.as_deref().expect("a warning is present");
        assert!(warning.contains("code=5"), "{warning}");
    }

    /// The code that cost the spike its audit lane is explained where the
    /// caller will read it.
    #[test]
    fn the_dynamic_session_refusal_is_named() {
        assert!(explain_fwp(0x8032_000B).contains("dynamic session"));
    }
}
