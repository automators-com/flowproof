//! The mechanism under test: WFP filters in a private sublayer, over a dynamic
//! session, scoped to the run identity's SID.
//!
//! Shape:
//!   - a **dynamic** engine session, so every object added through it is torn
//!     down by the kernel when the handle closes — including when the process
//!     is killed and never gets to run cleanup;
//!   - one private sublayer, so arbitration is ours and not shared with the
//!     Windows Firewall's;
//!   - per declared destination, a PERMIT filter at high weight;
//!   - one BLOCK filter at weight 0 matching only the run identity.
//!
//! WFP's cross-sublayer arbitration is block-wins, so the block holds even
//! though the Windows Firewall permits in its own sublayer.

use std::ffi::c_void;

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::HANDLE;
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::PSECURITY_DESCRIPTOR;
use windows::Win32::System::Rpc::RPC_C_AUTHN_DEFAULT;

use super::{wide, WinErr};

/// Decode the `FWP_E_*` codes this spike has actually hit.
///
/// Written down because two of them cost a CI cycle each:
/// `0x8032000B` is returned for a perfectly correct call made from the wrong
/// kind of session, and `0x80320027` for a condition whose *value type* is
/// wrong rather than whose field is.
pub fn explain_fwp(code: u32) -> &'static str {
    match code {
        0x8032_0009 => "FWP_E_ALREADY_EXISTS",
        0x8032_000B => "FWP_E_DYNAMIC_SESSION_IN_PROGRESS - not settable from a dynamic session",
        0x8032_0014 => "FWP_E_INCOMPATIBLE_LAYER - this condition is not valid at this layer",
        0x8032_0026 => "FWP_E_MATCH_TYPE_MISMATCH",
        0x8032_0027 => "FWP_E_TYPE_MISMATCH - the condition value's data type is wrong",
        0x8032_0028 => "FWP_E_OUT_OF_BOUNDS",
        _ => "see the FWP_E_* table",
    }
}

/// A declared destination — the Windows analogue of `allow_egress`.
#[derive(Clone, Copy, Debug)]
pub struct Declared {
    pub addr: std::net::Ipv4Addr,
    pub port: u16,
    pub protocol: u8,
}

pub const IPPROTO_TCP_U8: u8 = 6;
pub const IPPROTO_UDP_U8: u8 = 17;

/// Owns the security descriptor blob backing an `ALE_USER_ID` condition.
///
/// The condition value is a pointer into this allocation, so it has to outlive
/// every `FwpmFilterAdd0` that references it. Keeping it in a struct makes that
/// lifetime explicit instead of relying on where a local happens to be dropped.
pub struct UserCondition {
    psd: PSECURITY_DESCRIPTOR,
    blob: FWP_BYTE_BLOB,
}

impl UserCondition {
    /// Build the SD that `FWPM_CONDITION_ALE_USER_ID` access-checks the
    /// connecting token against.
    ///
    /// `CC` is `SDDL_CREATE_CHILD` = 0x1, which at this layer *is*
    /// `FWP_ACTRL_MATCH_FILTER`. The mapping is not obvious and is the single
    /// most common way to get an ALE_USER_ID filter that silently matches
    /// nothing.
    pub fn for_sid(sid_string: &str) -> Result<Self, WinErr> {
        let sddl = format!("O:SYG:SYD:(A;;CC;;;{sid_string})");
        let wsddl = wide(&sddl);
        let mut psd = PSECURITY_DESCRIPTOR::default();
        let mut len: u32 = 0;
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                PCWSTR(wsddl.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                Some(&mut len),
            )
        }
        .map_err(|e| {
            WinErr::new(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                e.code().0 as u32,
                sddl.clone(),
            )
        })?;
        Ok(Self {
            psd,
            blob: FWP_BYTE_BLOB {
                size: len,
                data: psd.0 as *mut u8,
            },
        })
    }

    fn condition(&self) -> FWPM_FILTER_CONDITION0 {
        FWPM_FILTER_CONDITION0 {
            fieldKey: FWPM_CONDITION_ALE_USER_ID,
            matchType: FWP_MATCH_EQUAL,
            conditionValue: FWP_CONDITION_VALUE0 {
                r#type: FWP_SECURITY_DESCRIPTOR_TYPE,
                Anonymous: FWP_CONDITION_VALUE0_0 {
                    sd: &self.blob as *const FWP_BYTE_BLOB as *mut FWP_BYTE_BLOB,
                },
            },
        }
    }
}

impl Drop for UserCondition {
    fn drop(&mut self) {
        if !self.psd.is_invalid() {
            unsafe {
                windows::Win32::Foundation::LocalFree(Some(windows::Win32::Foundation::HLOCAL(
                    self.psd.0,
                )))
            };
        }
    }
}

pub struct Engine {
    pub handle: HANDLE,
    pub sublayer_key: GUID,
    pub added_filter_ids: Vec<u64>,
    closed: bool,
}

impl Engine {
    /// Open a **dynamic** session. This is the whole cleanup story: objects
    /// added over a dynamic session are removed by the kernel when the last
    /// handle to it goes away, which covers the supervisor being killed with
    /// no chance to run a destructor.
    pub fn open_dynamic() -> Result<Self, WinErr> {
        let mut name = wide("flowproof spike session");
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
            return Err(WinErr::new("FwpmEngineOpen0", rc, "dynamic session".into()));
        }
        Ok(Self {
            handle,
            sublayer_key: GUID::zeroed(),
            added_filter_ids: Vec::new(),
            closed: false,
        })
    }

    /// A private sublayer, so our arbitration is not entangled with the
    /// Windows Firewall's. Not marked persistent: in a dynamic session it dies
    /// with the handle.
    pub fn add_sublayer(&mut self) -> Result<GUID, WinErr> {
        let key = new_guid();
        let mut name = wide("flowproof spike sublayer");
        let mut desc = wide("per-run egress containment");
        let sl = FWPM_SUBLAYER0 {
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
            weight: 0x8000,
        };
        let rc = unsafe { FwpmSubLayerAdd0(self.handle, &sl, None) };
        if rc != 0 {
            return Err(WinErr::new("FwpmSubLayerAdd0", rc, format!("{key:?}")));
        }
        self.sublayer_key = key;
        Ok(key)
    }

    /// PERMIT for one declared destination, at weight 15.
    pub fn add_permit(
        &mut self,
        user: &UserCondition,
        d: Declared,
        v6_layer: bool,
    ) -> Result<u64, WinErr> {
        let addr_mask = FWP_V4_ADDR_AND_MASK {
            // WFP takes v4 addresses in host byte order, not network order.
            addr: u32::from(d.addr),
            mask: 0xFFFF_FFFF,
        };
        let mut conds = vec![user.condition()];
        if !v6_layer {
            conds.push(FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_IP_REMOTE_ADDRESS,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_V4_ADDR_MASK,
                    Anonymous: FWP_CONDITION_VALUE0_0 {
                        v4AddrMask: &addr_mask as *const _ as *mut _,
                    },
                },
            });
        }
        conds.push(FWPM_FILTER_CONDITION0 {
            fieldKey: FWPM_CONDITION_IP_PROTOCOL,
            matchType: FWP_MATCH_EQUAL,
            conditionValue: FWP_CONDITION_VALUE0 {
                r#type: FWP_UINT8,
                Anonymous: FWP_CONDITION_VALUE0_0 { uint8: d.protocol },
            },
        });
        conds.push(FWPM_FILTER_CONDITION0 {
            fieldKey: FWPM_CONDITION_IP_REMOTE_PORT,
            matchType: FWP_MATCH_EQUAL,
            conditionValue: FWP_CONDITION_VALUE0 {
                r#type: FWP_UINT16,
                Anonymous: FWP_CONDITION_VALUE0_0 { uint16: d.port },
            },
        });

        let label = format!("permit {}:{} proto {}", d.addr, d.port, d.protocol);
        self.add_filter(&label, &conds, FWP_ACTION_PERMIT, 15, v6_layer)
    }

    /// The default-deny half. Matches on the run identity and nothing else, at
    /// weight 0 so any permit outranks it.
    pub fn add_block_all(&mut self, user: &UserCondition, v6_layer: bool) -> Result<u64, WinErr> {
        let conds = vec![user.condition()];
        self.add_filter(
            "block all (identity)",
            &conds,
            FWP_ACTION_BLOCK,
            0,
            v6_layer,
        )
    }

    /// UNUSED, and kept as evidence. See LOG.md finding 5.1: this adds cleanly
    /// and denies every socket the identity tries to open, declared or not.
    ///
    /// Refuse raw sockets and promiscuous mode at ALE_RESOURCE_ASSIGNMENT.
    ///
    /// Without this a contained process could build its own packets and skip
    /// the connect path entirely, which would make "it cannot" false.
    ///
    /// The first Windows run added this with an `FWP_UINT8` condition value and
    /// got `FWP_E_TYPE_MISMATCH` (0x80320027) — `FWPM_CONDITION_ALE_PROMISCUOUS_MODE`
    /// wants `FWP_UINT32`. Both are attempted and both outcomes reported, so a
    /// second wrong guess costs a log line rather than a CI cycle.
    #[allow(dead_code)]
    pub fn add_promiscuous_block(&mut self, user: &UserCondition) -> Result<u64, WinErr> {
        let mut first_err = None;
        for (label, ty) in [("uint32", FWP_UINT32), ("uint8", FWP_UINT8)] {
            let conds = vec![
                user.condition(),
                FWPM_FILTER_CONDITION0 {
                    fieldKey: FWPM_CONDITION_ALE_PROMISCUOUS_MODE,
                    matchType: FWP_MATCH_EQUAL,
                    conditionValue: FWP_CONDITION_VALUE0 {
                        r#type: ty,
                        Anonymous: if ty == FWP_UINT32 {
                            FWP_CONDITION_VALUE0_0 { uint32: 1 }
                        } else {
                            FWP_CONDITION_VALUE0_0 { uint8: 1 }
                        },
                    },
                },
            ];
            match self.add_filter_at_layer(
                &format!("block promiscuous ({label})"),
                &conds,
                FWP_ACTION_BLOCK,
                15,
                FWPM_LAYER_ALE_RESOURCE_ASSIGNMENT_V4,
            ) {
                Ok(id) => return Ok(id),
                Err(e) => {
                    crate::report::emit(&format!(
                        "SPIKE|NOTE|wfp.block.promiscuous.attempt.{label}|{e}"
                    ));
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.unwrap_or_else(|| {
            WinErr::new("FwpmFilterAdd0", 0, "no promiscuous attempt made".into())
        }))
    }

    /// Refuse raw sockets outright, by protocol, at the same layer.
    ///
    /// Separate from the promiscuous block because they fail independently and
    /// a single combined result would hide which one holds.
    #[allow(dead_code)]
    pub fn add_raw_socket_block(&mut self, user: &UserCondition) -> Result<u64, WinErr> {
        // 255 is IPPROTO_RAW. A process that can open one can compose its own
        // headers, and everything proven at ALE_AUTH_CONNECT stops applying.
        let conds = vec![
            user.condition(),
            FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_IP_PROTOCOL,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_UINT8,
                    Anonymous: FWP_CONDITION_VALUE0_0 { uint8: 255 },
                },
            },
        ];
        self.add_filter_at_layer(
            "block raw sockets",
            &conds,
            FWP_ACTION_BLOCK,
            15,
            FWPM_LAYER_ALE_RESOURCE_ASSIGNMENT_V4,
        )
    }

    fn add_filter(
        &mut self,
        label: &str,
        conds: &[FWPM_FILTER_CONDITION0],
        action: FWP_ACTION_TYPE,
        weight: u8,
        v6_layer: bool,
    ) -> Result<u64, WinErr> {
        let layer = if v6_layer {
            FWPM_LAYER_ALE_AUTH_CONNECT_V6
        } else {
            FWPM_LAYER_ALE_AUTH_CONNECT_V4
        };
        self.add_filter_weighted(label, conds, action, weight, layer)
    }

    fn add_filter_at_layer(
        &mut self,
        label: &str,
        conds: &[FWPM_FILTER_CONDITION0],
        action: FWP_ACTION_TYPE,
        weight: u8,
        layer: GUID,
    ) -> Result<u64, WinErr> {
        self.add_filter_weighted(label, conds, action, weight, layer)
    }

    fn add_filter_weighted(
        &mut self,
        label: &str,
        conds: &[FWPM_FILTER_CONDITION0],
        action: FWP_ACTION_TYPE,
        weight: u8,
        layer: GUID,
    ) -> Result<u64, WinErr> {
        let mut name = wide(label);
        let mut desc = wide("flowproof spike");
        let filter = FWPM_FILTER0 {
            filterKey: new_guid(),
            displayData: FWPM_DISPLAY_DATA0 {
                name: PWSTR(name.as_mut_ptr()),
                description: PWSTR(desc.as_mut_ptr()),
            },
            flags: FWPM_FILTER_FLAGS(0),
            providerKey: std::ptr::null_mut(),
            providerData: FWP_BYTE_BLOB {
                size: 0,
                data: std::ptr::null_mut(),
            },
            layerKey: layer,
            subLayerKey: self.sublayer_key,
            weight: FWP_VALUE0 {
                r#type: FWP_UINT8,
                Anonymous: FWP_VALUE0_0 { uint8: weight },
            },
            numFilterConditions: conds.len() as u32,
            filterCondition: conds.as_ptr() as *mut FWPM_FILTER_CONDITION0,
            action: FWPM_ACTION0 {
                r#type: action,
                Anonymous: FWPM_ACTION0_0 {
                    filterType: GUID::zeroed(),
                },
            },
            Anonymous: FWPM_FILTER0_0 { rawContext: 0 },
            reserved: std::ptr::null_mut(),
            filterId: 0,
            effectiveWeight: FWP_VALUE0 {
                r#type: FWP_EMPTY,
                Anonymous: FWP_VALUE0_0 { uint8: 0 },
            },
        };
        let mut id: u64 = 0;
        let rc = unsafe { FwpmFilterAdd0(self.handle, &filter, None, Some(&mut id)) };
        if rc != 0 {
            return Err(WinErr::new("FwpmFilterAdd0", rc, label.to_string()));
        }
        self.added_filter_ids.push(id);
        Ok(id)
    }

    /// Turn on net-event collection and ask for the classify-drop keyword.
    ///
    /// Engine-wide, not session-scoped — so it is restored on the way out.
    pub fn enable_net_events(&self) -> Result<(), WinErr> {
        let on = FWP_VALUE0 {
            r#type: FWP_UINT32,
            Anonymous: FWP_VALUE0_0 { uint32: 1 },
        };
        let rc = unsafe { FwpmEngineSetOption0(self.handle, FWPM_ENGINE_COLLECT_NET_EVENTS, &on) };
        if rc != 0 {
            return Err(WinErr::new(
                "FwpmEngineSetOption0(COLLECT_NET_EVENTS)",
                rc,
                String::new(),
            ));
        }
        // There is deliberately no CLASSIFY_DROP keyword here, and its absence
        // is not an oversight: WFP has no such keyword. Classify *drops* are
        // collected whenever COLLECT_NET_EVENTS is on; the keyword set only
        // adds the optional categories. CLASSIFY_ALLOW is requested so the
        // declared connection shows up as an explicit allow — positive
        // evidence that the permit filter matched, rather than the absence of
        // a drop.
        let keywords = FWP_VALUE0 {
            r#type: FWP_UINT32,
            Anonymous: FWP_VALUE0_0 {
                uint32: FWPM_NET_EVENT_KEYWORD_CLASSIFY_ALLOW,
            },
        };
        let rc = unsafe {
            FwpmEngineSetOption0(
                self.handle,
                FWPM_ENGINE_NET_EVENT_MATCH_ANY_KEYWORDS,
                &keywords,
            )
        };
        if rc != 0 {
            return Err(WinErr::new(
                "FwpmEngineSetOption0(MATCH_ANY_KEYWORDS)",
                rc,
                String::new(),
            ));
        }
        Ok(())
    }

    /// How many filters currently live in our sublayer, read back through a
    /// **fresh** engine handle.
    ///
    /// Reading through our own handle would prove nothing about teardown: the
    /// question is whether the filters are gone for everyone, which only an
    /// independent handle can answer.
    pub fn count_filters_in_sublayer(sublayer: GUID) -> Result<usize, WinErr> {
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
            return Err(WinErr::new("FwpmEngineOpen0(readback)", rc, String::new()));
        }
        let mut enum_handle = HANDLE::default();
        let rc = unsafe { FwpmFilterCreateEnumHandle0(engine, None, &mut enum_handle) };
        if rc != 0 {
            unsafe { FwpmEngineClose0(engine) };
            return Err(WinErr::new(
                "FwpmFilterCreateEnumHandle0",
                rc,
                String::new(),
            ));
        }
        let mut found = 0usize;
        loop {
            let mut entries: *mut *mut FWPM_FILTER0 = std::ptr::null_mut();
            let mut num: u32 = 0;
            let rc = unsafe { FwpmFilterEnum0(engine, enum_handle, 256, &mut entries, &mut num) };
            if rc != 0 || num == 0 {
                break;
            }
            for i in 0..num as usize {
                let f = unsafe { *(*entries.add(i)) };
                if f.subLayerKey == sublayer {
                    found += 1;
                }
            }
            unsafe { FwpmFreeMemory0(&mut (entries as *mut c_void)) };
            if num < 256 {
                break;
            }
        }
        unsafe {
            FwpmFilterDestroyEnumHandle0(engine, enum_handle);
            FwpmEngineClose0(engine);
        }
        Ok(found)
    }

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

fn new_guid() -> GUID {
    // `CoCreateGuid` rather than a random crate: no new dependency, and it is
    // the same source WFP's own tooling uses.
    unsafe { windows::Win32::System::Com::CoCreateGuid() }.unwrap_or_else(|_| GUID::zeroed())
}
