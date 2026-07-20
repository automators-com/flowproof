//! The recording agent: performs a flow once from a natural-language spec
//! and records a trace. Authoring backends are pluggable: the deterministic
//! rules resolver ([`rules`]) handles known app vocabularies, and the LLM
//! author ([`author`]) handles arbitrary steps by observing the live app's
//! scene graph. The replayer never touches this crate.

pub mod author;
pub mod heal;
pub mod llm;
pub mod recorder;
pub mod rules;
pub mod spec;

pub use heal::{heal, heal_with_author, HealError, HealReport};
pub use llm::{HttpModelClient, ModelClient};
pub use recorder::{record, record_with_author, Author, RecordError, RecordSummary};
pub use spec::{FlowSpec, SpecStep, SuiteManifest};

use std::env;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("backend configuration error: {0}")]
    Config(String),
    #[error("authoring failed for step '{step}': {reason}")]
    Authoring { step: String, reason: String },
}

/// Which model backend drives the authoring loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Anthropic Messages API.
    Anthropic,
    /// Any OpenAI-compatible endpoint (e.g. vLLM serving a local model).
    OpenAiCompatible,
}

/// Backend configuration, resolved from the environment.
///
/// Env names mirror the conventions used across Automators products:
/// `FLOWPROOF_AI_PROVIDER` (`anthropic` | `openai-compatible`),
/// `FLOWPROOF_AI_BASE_URL`, `FLOWPROOF_AI_API_KEY`, `FLOWPROOF_AI_MODEL`.
/// The API key falls back to `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendConfig {
    pub kind: BackendKind,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: Option<String>,
}

impl BackendConfig {
    pub fn from_env() -> Result<Self, AgentError> {
        let provider = env::var("FLOWPROOF_AI_PROVIDER").unwrap_or_default();
        Self::from_provider_name(&provider, env::var("FLOWPROOF_AI_BASE_URL").ok()).map(
            |mut config| {
                config.model = env::var("FLOWPROOF_AI_MODEL").ok();
                config.api_key =
                    env::var("FLOWPROOF_AI_API_KEY")
                        .ok()
                        .or_else(|| match config.kind {
                            BackendKind::Anthropic => env::var("ANTHROPIC_API_KEY").ok(),
                            BackendKind::OpenAiCompatible => env::var("OPENAI_API_KEY").ok(),
                        });
                config
            },
        )
    }

    /// Whether this configuration can actually make calls: Anthropic needs a
    /// key; an OpenAI-compatible endpoint needs a base url (key optional —
    /// local vLLM commonly runs without one).
    pub fn is_usable(&self) -> bool {
        match self.kind {
            BackendKind::Anthropic => self.api_key.is_some(),
            BackendKind::OpenAiCompatible => self.base_url.is_some(),
        }
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
            api_key: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_provider_is_anthropic() {
        let config = BackendConfig::from_provider_name("", None).expect("default config");
        assert_eq!(config.kind, BackendKind::Anthropic);
        assert!(!config.is_usable(), "no key -> not usable");
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
        assert!(ok.is_usable(), "local endpoints need no key");
    }

    #[test]
    fn unknown_provider_is_rejected() {
        let err = BackendConfig::from_provider_name("gemini", None)
            .expect_err("unknown provider must be rejected");
        assert!(matches!(err, AgentError::Config(_)));
    }
}
