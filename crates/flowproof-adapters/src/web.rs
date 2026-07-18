//! Web adapter (WebDriver/CDP) for browser targets and web views.

use crate::AdapterError;

/// Handle to a browser automation session.
#[derive(Debug, Default)]
pub struct WebSession {
    _private: (),
}

impl WebSession {
    pub fn connect(_endpoint: &str) -> Result<Self, AdapterError> {
        Err(AdapterError::NotImplemented("web"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_is_stubbed() {
        assert!(WebSession::connect("http://localhost:9515").is_err());
    }
}
