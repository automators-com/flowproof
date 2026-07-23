//! Native application adapters. Where a target exposes a scriptable API we
//! prefer it over pixels: SAP GUI Scripting COM (`sap-com` feature), browser
//! via the DevTools protocol (`web` feature). Java Access Bridge comes later.

#[cfg(feature = "agent")]
pub mod agent_proxy;

#[cfg(feature = "agent")]
pub mod agent_runner;

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
pub use agent_runner::{AgentRun, RunError};

#[cfg(feature = "agent")]
pub use mcp_core::{McpCall, McpDivergence};

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
