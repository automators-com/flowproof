//! Native application adapters. Where a target exposes a scriptable API we
//! prefer it over pixels: SAP GUI Scripting COM (`sap-com` feature), browser
//! via the DevTools protocol (`web` feature). Java Access Bridge comes later.

#[cfg(feature = "sap-com")]
pub mod sap_com;

#[cfg(feature = "web")]
pub mod web;

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
