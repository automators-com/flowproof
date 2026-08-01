//! Native application adapters. Where a target exposes a scriptable API we
//! prefer it over pixels: SAP GUI Scripting COM (`sap-com` feature), browser
//! via the DevTools protocol (`web` feature). Java Access Bridge comes later.

#[cfg(feature = "agent")]
pub mod agent_proxy;

#[cfg(feature = "agent")]
pub mod agent_runner;

#[cfg(feature = "agent")]
pub mod egress;

// The seccomp mechanism is Linux-only; on every other OS the `egress` module
// reports "not contained" and this module does not exist.
#[cfg(all(feature = "agent", target_os = "linux"))]
pub mod egress_linux;

// Windows containment (see `spike/windows-containment/LOG.md` for how it was
// established). This module installs the filters and runs the agent behind
// them; the tier it achieved travels back on the run rather than being
// predicted by a probe, because several steps can fail after the probe passes.
//
// Gated on the `windows` DEPENDENCY (either feature that pulls it) rather than
// on `agent`, and that is deliberate. `agent` pulls ureq, whose TLS stack
// pulls `ring`, whose build script cannot cross-compile from Linux - so
// gating this on `agent` would make `cargo check --target
// x86_64-pc-windows-msvc` impossible, and that command is the only way this
// Win32 code gets typechecked without a Windows runner. Under `sap-com` alone
// the module still builds, and the check is a few seconds on the Linux box.
// It holds no `Containment` for the same reason: that type is agent-gated.
#[cfg(all(windows, any(feature = "agent", feature = "sap-com")))]
pub mod egress_windows;

// Filesystem OBSERVATION. Cross-platform like `egress`, and for the same
// reason: the "nothing was observed" path must compile and be exercised on
// every OS, even though only Linux has a mechanism to observe with.
#[cfg(feature = "agent")]
pub mod fs_observe;

#[cfg(feature = "agent")]
pub mod mcp_core;

#[cfg(feature = "agent")]
pub mod mcp_http;

#[cfg(feature = "agent")]
pub mod mcp_stdio;

#[cfg(feature = "sap-com")]
pub mod sap_com;

#[cfg(feature = "vision")]
pub mod vision;

#[cfg(feature = "web")]
pub mod web;

#[cfg(feature = "agent")]
pub use agent_proxy::AgentProxy;

#[cfg(feature = "agent")]
pub use agent_runner::{AgentRun, RunError, Trigger};

#[cfg(feature = "agent")]
pub use egress::{AllowSet, Containment, EgressLog};

#[cfg(feature = "agent")]
pub use fs_observe::{FsEvent, FsLog};

#[cfg(feature = "agent")]
pub use mcp_core::{McpCall, McpDivergence, McpServerEvent};

#[cfg(feature = "agent")]
pub use mcp_http::{McpHttpLog, McpHttpServer};

#[cfg(feature = "agent")]
pub use mcp_stdio::{McpOut, McpPlan};

#[cfg(feature = "sap-com")]
pub use sap_com::SapAppDriver;

#[cfg(feature = "vision")]
pub use vision::VisionAppDriver;

#[cfg(feature = "web")]
pub use web::WebAppDriver;

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("adapter '{0}' is not implemented yet")]
    NotImplemented(&'static str),
    #[error("adapter '{0}' is not available on this platform")]
    UnsupportedPlatform(&'static str),
    #[error("web adapter: {0}")]
    Web(String),
}

/// Names of the adapters compiled into this build.
pub fn available_adapters() -> Vec<&'static str> {
    let mut adapters = Vec::new();
    if cfg!(feature = "sap-com") {
        adapters.push("sap-com");
    }
    if cfg!(feature = "vision") {
        adapters.push("vision");
    }
    if cfg!(feature = "web") {
        adapters.push("web");
    }
    adapters
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_adapters_matches_features() {
        let adapters = available_adapters();
        assert_eq!(adapters.contains(&"sap-com"), cfg!(feature = "sap-com"));
        assert_eq!(adapters.contains(&"web"), cfg!(feature = "web"));
    }
}
