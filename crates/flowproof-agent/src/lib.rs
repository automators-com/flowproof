//! The recording agent: performs a flow once from a natural-language spec
//! and records a trace. Model backends are pluggable; this slice ships a
//! deterministic rule-based resolver for Windows Calculator ([`rules`]).
//! The replayer never touches this crate.

pub mod heal;
pub mod recorder;
pub mod rules;
pub mod spec;

pub use heal::{heal, HealError, HealReport};
pub use recorder::{record, RecordError, RecordSummary};
pub use spec::{FlowSpec, SpecStep};

use std::env;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("agent loop is not implemented yet")]
    NotImplemented,
    #[error("backend configuration error: {0}")]
    Config(String),
}

/// Which model backend drives the computer-use loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Anthropic computer-use API.
    Anthropic,
    /// Any OpenAI-compatible endpoint (e.g. vLLM serving a local model).
    OpenAiCompatible,
}

/// Backend configuration, resolved from the environment.
///
/// Env names mirror the conventions used across Automators products:
/// `FLOWPROOF_AI_PROVIDER` (`anthropic` | `openai-compatible`),
/// `FLOWPROOF_AI_BASE_URL`, `FLOWPROOF_AI_API_KEY`, `FLOWPROOF_AI_MODEL`.
/// With nothing set, defaults to Anthropic (key read at call time).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendConfig {
    pub kind: BackendKind,
    pub base_url: Option<String>,
    pub model: Option<String>,
}

impl BackendConfig {
    pub fn from_env() -> Result<Self, AgentError> {
        let provider = env::var("FLOWPROOF_AI_PROVIDER").unwrap_or_default();
        Self::from_provider_name(&provider, env::var("FLOWPROOF_AI_BASE_URL").ok()).map(
            |mut config| {
                config.model = env::var("FLOWPROOF_AI_MODEL").ok();
                config
            },
        )
    }

    fn from_provider_name(provider: &str, base_url: Option<String>) -> Result<Self, AgentError> {
        let kind = match provider {
            "" | "anthropic" => BackendKind::Anthropic,
            "openai-compatible" => BackendKind::OpenAiCompatible,
            other => {
                return Err(AgentError::Config(format!(
                    "unknown FLOWPROOF_AI_PROVIDER '{other}' (expected 'anthropic' or 'openai-compatible')"
                )))
            }
        };
        if kind == BackendKind::OpenAiCompatible && base_url.is_none() {
            return Err(AgentError::Config(
                "FLOWPROOF_AI_BASE_URL is required for the openai-compatible provider".into(),
            ));
        }
        Ok(Self {
            kind,
            base_url,
            model: None,
        })
    }
}

/// The planner loop: observe (screenshot + scene graph) -> plan -> act via
/// the driver -> record a trace step. Stub until the trace format lands.
#[derive(Debug)]
pub struct Recorder {
    pub config: BackendConfig,
}

impl Recorder {
    pub fn new(config: BackendConfig) -> Self {
        Self { config }
    }

    pub fn record(&mut self, _spec_intent: &str) -> Result<(), AgentError> {
        Err(AgentError::NotImplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_is_anthropic() {
        let config = BackendConfig::from_provider_name("", None).expect("default config");
        assert_eq!(config.kind, BackendKind::Anthropic);
    }

    #[test]
    fn openai_compatible_requires_base_url() {
        let err = BackendConfig::from_provider_name("openai-compatible", None)
            .expect_err("missing base url must be rejected");
        assert!(matches!(err, AgentError::Config(_)));

        let ok = BackendConfig::from_provider_name(
            "openai-compatible",
            Some("http://localhost:8000/v1".into()),
        )
        .expect("config with base url");
        assert_eq!(ok.kind, BackendKind::OpenAiCompatible);
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let err = BackendConfig::from_provider_name("gemini", None)
            .expect_err("unknown provider must be rejected");
        assert!(matches!(err, AgentError::Config(_)));
    }
}
