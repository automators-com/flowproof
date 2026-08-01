//! The WFP session and sublayer: a private arbitration space that tears
//! itself down.
//!
//! This is the container the filters will go in. It adds none of them yet, so
//! a Windows run is still reported "not contained".
//!
//! # Two decisions the whole mechanism rests on
//!
//! **The session is DYNAMIC.** Every object added over a dynamic session is
//! removed by the kernel when the last handle to it goes away - including when
//! flowproof is killed outright and never gets to run a destructor. That is
//! the teardown guarantee, and it is a property of the kernel rather than of
//! our cleanup code being correct. The spike verified it the only way worth
//! trusting: it killed the supervisor, then re-opened a *fresh* engine handle
//! and confirmed the filters were gone, having first confirmed they had been
//! there (`teardown.abrupt.*` in `spike/windows-containment/LOG.md`).
//!
//! `Drop` still calls `FwpmEngineClose0`, but that is the tidy path, not the
//! guarantee. If `Drop` never runs, the kernel does it anyway.
//!
//! **The sublayer is PRIVATE.** Arbitration inside it is ours alone, not
//! entangled with the Windows Firewall's. This matters because WFP's
//! cross-sublayer arbitration is **block-wins**: our block still holds even
//! though the firewall permits in its own sublayer. Sharing a sublayer would
//! make the outcome depend on weights we do not control.
//!
//! It is deliberately not marked persistent. In a dynamic session it dies with
//! the handle, which is exactly what we want - a filter surviving a crashed
//! test run would silently break the next thing the machine does.

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::{
    FwpmEngineClose0, FwpmEngineOpen0, FwpmSubLayerAdd0, FWPM_DISPLAY_DATA0, FWPM_SESSION0,
    FWPM_SESSION_FLAG_DYNAMIC, FWPM_SUBLAYER0, FWP_BYTE_BLOB,
};
use windows::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

use super::{wide, WinErr};

/// Mid-range sublayer weight. Nothing depends on the exact value: arbitration
/// across sublayers is block-wins, so this only orders us against other
/// third-party sublayers, and it is recorded rather than tuned.
const SUBLAYER_WEIGHT: u16 = 0x8000;

/// Decode the `FWP_E_*` codes this work has actually hit.
///
/// Written down because two of them cost the spike a CI cycle each, and both
/// name something other than the real cause. `0x8032000B` is returned for a
/// perfectly correct call made from the wrong KIND of session, and
/// `0x80320027` for a condition whose *value type* is wrong rather than whose
/// field is - so the obvious next move (checking the field) is the wrong one.
pub fn explain_fwp(code: u32) -> &'static str {
    match code {
        0x8032_0009 => "FWP_E_ALREADY_EXISTS",
        0x8032_000B => "FWP_E_DYNAMIC_SESSION_IN_PROGRESS - not settable from a dynamic session",
        0x8032_0014 => "FWP_E_INCOMPATIBLE_LAYER - this condition is not valid at this layer",
        0x8032_0026 => "FWP_E_MATCH_TYPE_MISMATCH",
        0x8032_0027 => "FWP_E_TYPE_MISMATCH - the condition value's data type is wrong",
        0x8032_0028 => "FWP_E_OUT_OF_BOUNDS",
        0x8032_0005 => "FWP_E_NOT_FOUND",
        0x0000_0005 => "ERROR_ACCESS_DENIED - adding a filter requires Administrator",
        _ => "see the FWP_E_* table",
    }
}

/// An open WFP session and the private sublayer inside it.
///
/// Dropping this closes the session, which removes every object added through
/// it. See the module docs for why that is a kernel guarantee rather than a
/// promise about this destructor.
pub struct Engine {
    handle: HANDLE,
    sublayer_key: GUID,
    closed: bool,
}

impl Engine {
    /// Open a dynamic session.
    ///
    /// Requires Administrator. `ERROR_ACCESS_DENIED` (5) here is that and
    /// nothing subtler - see [`super::HostReadiness`], which is what an
    /// adopter should have been told before reaching this point.
    pub fn open_dynamic() -> Result<Self, WinErr> {
        let mut name = wide("flowproof egress containment");
        let mut desc = wide("dynamic session; auto-torn-down on handle close");
        let session = FWPM_SESSION0 {
            sessionKey: GUID::zeroed(),
            displayData: FWPM_DISPLAY_DATA0 {
                name: PWSTR(name.as_mut_ptr()),
                description: PWSTR(desc.as_mut_ptr()),
            },
            flags: FWPM_SESSION_FLAG_DYNAMIC,
            txnWaitTimeoutInMSec: 0,
            processId: 0,
            sid: std::ptr::null_mut(),
            username: PWSTR::null(),
            kernelMode: false.into(),
        };
        let mut handle = HANDLE::default();
        let rc = unsafe {
            FwpmEngineOpen0(
                PCWSTR::null(),
                RPC_C_AUTHN_DEFAULT as u32,
                None,
                Some(&session),
                &mut handle,
            )
        };
        if rc != 0 {
            return Err(WinErr::new(
                "FwpmEngineOpen0",
                rc,
                format!("dynamic session -> {}", explain_fwp(rc)),
            ));
        }
        Ok(Self {
            handle,
            sublayer_key: GUID::zeroed(),
            closed: false,
        })
    }

    /// Add the private sublayer this run's filters live in.
    ///
    /// The key is freshly generated per run, so two flowproof runs on one host
    /// never share an arbitration space or collide on `FWP_E_ALREADY_EXISTS`.
    pub fn add_sublayer(&mut self) -> Result<GUID, WinErr> {
        let key = new_guid();
        let mut name = wide("flowproof per-run sublayer");
        let mut desc = wide("per-run egress containment");
        let sublayer = FWPM_SUBLAYER0 {
            subLayerKey: key,
            displayData: FWPM_DISPLAY_DATA0 {
                name: PWSTR(name.as_mut_ptr()),
                description: PWSTR(desc.as_mut_ptr()),
            },
            flags: 0,
            providerKey: std::ptr::null_mut(),
            providerData: FWP_BYTE_BLOB {
                size: 0,
                data: std::ptr::null_mut(),
            },
            weight: SUBLAYER_WEIGHT,
        };
        let rc = unsafe { FwpmSubLayerAdd0(self.handle, &sublayer, None) };
        if rc != 0 {
            return Err(WinErr::new(
                "FwpmSubLayerAdd0",
                rc,
                format!("{key:?} -> {}", explain_fwp(rc)),
            ));
        }
        self.sublayer_key = key;
        Ok(key)
    }

    /// The engine handle, for the filter-add calls that come next.
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// The private sublayer's key. Zero until [`Engine::add_sublayer`] has
    /// run - a filter added against a zero key would land in the default
    /// sublayer, where our arbitration is not ours.
    pub fn sublayer_key(&self) -> GUID {
        self.sublayer_key
    }

    /// Has the sublayer been created? Callers must not add filters before it
    /// has; see [`Engine::sublayer_key`] for why.
    pub fn has_sublayer(&self) -> bool {
        self.sublayer_key != GUID::zeroed()
    }

    /// Close the session, removing everything added through it. Idempotent.
    pub fn close(&mut self) {
        if !self.closed {
            unsafe { FwpmEngineClose0(self.handle) };
            self.closed = true;
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
impl Engine {
    /// A CLOSED, sublayer-less engine, so a caller's preconditions can be
    /// tested without a Windows kernel or Administrator. Marked closed so
    /// `Drop` never calls `FwpmEngineClose0` on a null handle.
    pub(crate) fn closed_for_test() -> Self {
        Self {
            handle: HANDLE::default(),
            sublayer_key: GUID::zeroed(),
            closed: true,
        }
    }
}

/// A fresh GUID for the per-run sublayer.
///
/// `CoCreateGuid` rather than a random crate: no new dependency, and it is the
/// same source WFP's own tooling uses.
fn new_guid() -> GUID {
    unsafe { windows::Win32::System::Com::CoCreateGuid() }.unwrap_or_else(|_| GUID::zeroed())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A filter must never be added before the sublayer exists.
    ///
    /// A zero sublayer key is not an error to WFP - the filter lands in the
    /// DEFAULT sublayer, where arbitration is shared with the Windows Firewall
    /// and our block no longer holds by construction. It would look like a
    /// working filter and behave like a suggestion, which is the worst
    /// available outcome, so the precondition is a queryable fact rather than
    /// a comment.
    #[test]
    fn a_fresh_engine_has_no_sublayer_yet() {
        // Constructed directly: opening a real session needs Administrator and
        // a Windows kernel, and the state machine under test is ours, not
        // WFP's.
        let engine = Engine {
            handle: HANDLE::default(),
            sublayer_key: GUID::zeroed(),
            closed: true,
        };
        assert!(
            !engine.has_sublayer(),
            "no filter may be added against a zero key"
        );
        assert_eq!(engine.sublayer_key(), GUID::zeroed());
    }

    /// Two runs on one host must not share an arbitration space, or collide
    /// on FWP_E_ALREADY_EXISTS.
    #[test]
    fn each_run_gets_its_own_sublayer_key() {
        let a = new_guid();
        let b = new_guid();
        assert_ne!(a, GUID::zeroed(), "CoCreateGuid should not have failed");
        assert_ne!(a, b, "two runs must not collide");
    }

    /// The two codes that cost a CI cycle each say what they actually mean,
    /// because both name something other than the real cause.
    #[test]
    fn the_expensive_error_codes_are_explained() {
        assert!(explain_fwp(0x8032_000B).contains("dynamic session"));
        assert!(explain_fwp(0x8032_0027).contains("data type"));
        // Access denied is the one an adopter hits first, and it means
        // Administrator rather than anything about filters.
        assert!(explain_fwp(5).contains("Administrator"));
        // An unknown code still says where to look rather than nothing.
        assert!(!explain_fwp(0xDEAD_BEEF).is_empty());
    }

    /// Closing twice must not double-close the handle.
    #[test]
    fn close_is_idempotent() {
        let mut engine = Engine {
            handle: HANDLE::default(),
            sublayer_key: GUID::zeroed(),
            closed: true,
        };
        engine.close();
        engine.close();
        assert!(engine.closed);
    }
}
