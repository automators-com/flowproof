//! The filters themselves: one permit per declared destination, one block for
//! everything else, both scoped to the run identity.
//!
//! # Matching the Linux semantics exactly
//! The same `allow_egress` list drives seccomp on Linux and WFP here, so both
//! must enforce the SAME set. A spec meaning one thing on Linux and another on
//! Windows would be a silent behavioural fork - worse than an unsupported
//! platform, because nothing reports it. Two consequences, both deliberate:
//!
//! **No protocol condition.** `AllowSet::allows` vets by address and port and
//! never looks at the protocol. The spike's filters carried
//! `FWPM_CONDITION_IP_PROTOCOL` because they were hand-built for one TCP
//! probe; porting that would deny UDP to a host the spec declared.
//!
//! **Loopback is permitted wholesale**, because `allows` short-circuits on it
//! before consulting the list, while a WFP block scoped to the identity blocks
//! loopback too. Not a nicety: **replay reaches flowproof's own model-boundary
//! proxy over loopback**, so without this permit containment would not merely
//! diverge from Linux - it would break every replay on the platform, and the
//! symptom would be an agent that cannot reach a model rather than anything
//! naming a filter.

use std::net::IpAddr;

use windows::core::{GUID, PWSTR};
use windows::Win32::NetworkManagement::WindowsFilteringPlatform::*;
use windows::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::PSECURITY_DESCRIPTOR;

use super::wfp::{explain_fwp, Engine};
use super::{wide, WinErr};
use flowproof_trace::egress::{AllowEntry, HostMatch};

/// A permit outranks the block. Any non-zero weight would do; 15 is the
/// spike's, kept so its evidence still describes this.
const PERMIT_WEIGHT: u8 = 15;
/// The block sits at the bottom, so every permit wins.
const BLOCK_WEIGHT: u8 = 0;

// Compile-time, not a test: inverting these does not produce a failing
// assertion, it produces a run where every declared destination is denied
// while the report says "contained". A build that cannot express that state
// beats a test that catches it.
const _: () = assert!(
    PERMIT_WEIGHT > BLOCK_WEIGHT,
    "a permit must outrank the block, or every declared destination is denied"
);

/// Owns the security-descriptor blob backing an `ALE_USER_ID` condition. The
/// condition value POINTS into this allocation, so it must outlive every
/// `FwpmFilterAdd0` referencing it - explicit here rather than depending on
/// where a local happens to drop.
pub struct UserCondition {
    psd: PSECURITY_DESCRIPTOR,
    blob: FWP_BYTE_BLOB,
}

impl UserCondition {
    /// Build the SD that `FWPM_CONDITION_ALE_USER_ID` access-checks the
    /// connecting token against.
    ///
    /// `CC` is `SDDL_CREATE_CHILD` = 0x1, which at this layer *is*
    /// `FWP_ACTRL_MATCH_FILTER`. Per the spike that mapping is **the single
    /// most common way to get an `ALE_USER_ID` filter that adds successfully
    /// and silently matches nothing** - installed-looking and enforcing
    /// nothing, the failure this design is written against.
    pub fn for_sid(sid_string: &str) -> Result<Self, WinErr> {
        let sddl = format!("O:SYG:SYD:(A;;CC;;;{sid_string})");
        let wsddl = wide(&sddl);
        let mut psd = PSECURITY_DESCRIPTOR::default();
        let mut len = 0u32;
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                windows::core::PCWSTR(wsddl.as_ptr()),
                SDDL_REVISION_1,
                &mut psd,
                Some(&mut len),
            )
        }
        .map_err(|e| {
            WinErr::new(
                "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                e.code().0 as u32,
                sddl,
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

/// The v4 mask for a prefix length: `/0` is everything, `/32` is one address.
pub fn v4_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else if prefix >= 32 {
        u32::MAX
    } else {
        u32::MAX << (32 - prefix)
    }
}

/// Install the whole policy: loopback, one permit per declared entry, then the
/// default-deny block on BOTH address families.
///
/// The block goes on v4 AND v6. On v4 only, IPv6 would be entirely
/// unrestricted while every report said "contained" - the same false-green
/// family as #300 and #301, and easiest to miss because most probes are v4.
/// Takes the RESOLVED entries rather than the `AllowSet`, which keeps this
/// module off the `agent` feature and so cross-checkable from Linux; callers
/// pass `AllowSet::entries()`.
pub fn install(
    engine: &mut Engine,
    user: &UserCondition,
    entries: &[AllowEntry],
) -> Result<Vec<u64>, WinErr> {
    if !engine.has_sublayer() {
        return Err(WinErr::new(
            "install",
            0,
            "no private sublayer: a filter added against a zero key lands in the \
             default sublayer, where the block does not hold",
        ));
    }

    // Loopback first, on both families. See the module docs: replay depends
    // on it, and it mirrors `AllowSet::allows`.
    let loopback = [
        HostMatch::Cidr(IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 0)), 8),
        HostMatch::Ip(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST)),
    ];
    for host in &loopback {
        permit_entry(engine, user, host, None)?;
    }
    for entry in entries {
        permit_entry(engine, user, &entry.host, entry.port)?;
    }
    // The BLOCK ids are returned, and only those. A drop carrying one is ours
    // by construction, which is how the audit lane attributes without having
    // to infer from a SID that may not be set at all.
    let mut block_ids = Vec::with_capacity(2);
    for v6 in [false, true] {
        block_ids.push(add_filter(
            engine,
            "flowproof block (identity)",
            &[user.condition()],
            FWP_ACTION_BLOCK,
            BLOCK_WEIGHT,
            v6,
        )?);
    }
    Ok(block_ids)
}

/// One PERMIT filter for one resolved entry. The address storage is a local
/// that must outlive the `FwpmFilterAdd0` inside `add_filter`, which is why
/// this is one function rather than a builder returning conditions.
fn permit_entry(
    engine: &mut Engine,
    user: &UserCondition,
    host: &HostMatch,
    port: Option<u16>,
) -> Result<(), WinErr> {
    let (ip, prefix) = match host {
        HostMatch::Ip(ip) => (*ip, if ip.is_ipv4() { 32 } else { 128 }),
        HostMatch::Cidr(ip, p) => (*ip, *p),
        // Resolved away by `AllowSet::resolve`; a name never reaches here.
        HostMatch::Host(_) => return Ok(()),
    };

    let mut conds = vec![user.condition()];
    let v4_storage;
    let v6_storage;
    let v6_layer = ip.is_ipv6();

    match ip {
        IpAddr::V4(v4) => {
            v4_storage = FWP_V4_ADDR_AND_MASK {
                // WFP takes v4 addresses in HOST byte order, not network.
                addr: u32::from(v4),
                mask: v4_mask(prefix),
            };
            conds.push(FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_IP_REMOTE_ADDRESS,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_V4_ADDR_MASK,
                    Anonymous: FWP_CONDITION_VALUE0_0 {
                        v4AddrMask: &v4_storage as *const _ as *mut _,
                    },
                },
            });
        }
        IpAddr::V6(v6) => {
            v6_storage = FWP_V6_ADDR_AND_MASK {
                addr: v6.octets(),
                prefixLength: prefix,
            };
            conds.push(FWPM_FILTER_CONDITION0 {
                fieldKey: FWPM_CONDITION_IP_REMOTE_ADDRESS,
                matchType: FWP_MATCH_EQUAL,
                conditionValue: FWP_CONDITION_VALUE0 {
                    r#type: FWP_V6_ADDR_MASK,
                    Anonymous: FWP_CONDITION_VALUE0_0 {
                        v6AddrMask: &v6_storage as *const _ as *mut _,
                    },
                },
            });
        }
    }

    // No port condition == any port, as `AllowEntry::port_ok` treats `None`.
    // No protocol condition, ever - see the module docs.
    if let Some(p) = port {
        conds.push(FWPM_FILTER_CONDITION0 {
            fieldKey: FWPM_CONDITION_IP_REMOTE_PORT,
            matchType: FWP_MATCH_EQUAL,
            conditionValue: FWP_CONDITION_VALUE0 {
                r#type: FWP_UINT16,
                Anonymous: FWP_CONDITION_VALUE0_0 { uint16: p },
            },
        });
    }

    let label = match port {
        Some(p) => format!("flowproof permit {ip}/{prefix}:{p}"),
        None => format!("flowproof permit {ip}/{prefix}"),
    };
    add_filter(
        engine,
        &label,
        &conds,
        FWP_ACTION_PERMIT,
        PERMIT_WEIGHT,
        v6_layer,
    )?;
    Ok(())
}

fn add_filter(
    engine: &mut Engine,
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
    let mut name = wide(label);
    let mut desc = wide("flowproof per-run egress containment");
    let filter = FWPM_FILTER0 {
        filterKey: GUID::zeroed(),
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
        subLayerKey: engine.sublayer_key(),
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
    let mut id = 0u64;
    let rc = unsafe { FwpmFilterAdd0(engine.handle(), &filter, None, Some(&mut id)) };
    if rc != 0 {
        return Err(WinErr::new(
            "FwpmFilterAdd0",
            rc,
            format!("{label} -> {}", explain_fwp(rc)),
        ));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cidr_prefixes_become_the_right_masks() {
        assert_eq!(v4_mask(32), 0xFFFF_FFFF, "a single host");
        assert_eq!(v4_mask(24), 0xFFFF_FF00);
        assert_eq!(v4_mask(8), 0xFF00_0000, "127/8 is the loopback permit");
        assert_eq!(v4_mask(0), 0, "/0 matches everything");
        // Clamped, not shifted past the word width (panics in debug).
        assert_eq!(v4_mask(33), 0xFFFF_FFFF);
    }

    /// The SDDL is the one string here that silently means nothing when
    /// wrong: `CC` is `FWP_ACTRL_MATCH_FILTER` at this layer, and any other
    /// right yields a filter that adds fine and matches no one.
    #[test]
    fn the_user_sddl_uses_the_match_filter_right() {
        let sid = "S-1-5-21-1-2-3-1006";
        let sddl = format!("O:SYG:SYD:(A;;CC;;;{sid})");
        assert!(
            sddl.contains(";CC;"),
            "CC == FWP_ACTRL_MATCH_FILTER: {sddl}"
        );
        assert!(sddl.ends_with(&format!("{sid})")));
    }

    /// Installing against a zero sublayer key is REFUSED rather than landing
    /// in the default sublayer. WFP would accept such a filter; it would just
    /// arbitrate where our block does not hold - success-looking, enforcing
    /// nothing. This is what `has_sublayer` exists for.
    #[test]
    fn install_refuses_without_a_private_sublayer() {
        let mut engine = Engine::closed_for_test();
        assert!(!engine.has_sublayer());
        let user = UserCondition::for_sid("S-1-5-21-1-2-3-1006").expect("SDDL converts");

        let err = install(&mut engine, &user, &[]).expect_err("must refuse before adding anything");
        assert!(err.context.contains("default sublayer"), "{err}");
    }
}
