//! SAP GUI Scripting adapter (COM). Windows-only at runtime; compiles
//! everywhere so feature-enabled builds stay green on Linux CI.

use crate::AdapterError;

/// Handle to a SAP GUI Scripting session (`GuiSession`).
#[derive(Debug, Default)]
pub struct SapSession {
    _private: (),
}

impl SapSession {
    /// Attach to a running SAP GUI instance via the Scripting API.
    pub fn attach() -> Result<Self, AdapterError> {
        if cfg!(windows) {
            Err(AdapterError::NotImplemented("sap-com"))
        } else {
            Err(AdapterError::UnsupportedPlatform("sap-com"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_is_stubbed() {
        assert!(SapSession::attach().is_err());
    }
}
