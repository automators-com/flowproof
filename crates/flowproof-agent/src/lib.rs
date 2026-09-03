//! The recording agent: performs a flow once from a natural-language spec
//! and records a trace. Authoring backends are pluggable: the deterministic
//! rules resolver ([`rules`]) handles known app vocabularies, and the LLM
//! author ([`author`]) handles arbitrary steps by observing the live app's
//! scene graph. The replayer never touches this crate.

pub mod agent_steps;
pub mod author;
pub mod clarify;
pub mod doc_author;
pub mod doc_formats;
pub mod draft_assembly;
pub mod heal;
pub mod llm;
pub mod recorder;
pub mod rules;
pub mod spec;

pub use clarify::{Clarification, ClarifyStage};
pub use heal::{heal, heal_with_author, HealError, HealReport};
pub use llm::{HttpModelClient, ModelClient};
pub use recorder::{
    record, record_incremental, record_incremental_with_options, record_with_author,
    record_with_author_and_options, surface_targets, Author, RecordError, RecordSummary,
};
pub use spec::{
    check_control_ids, FlowSpec, LoginSpec, McpServerSpec, SessionRef, SpecStep, SuiteManifest,
};

use std::env;

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("backend configuration error: {0}")]
    Config(String),
    #[error("authoring failed for step '{step}': {reason}")]
    Authoring { step: String, reason: String },
    /// The screen disputes the step BEFORE this one. Raised while authoring
    /// `step`, but `step` is the witness, not the culprit — the failure is
    /// reported here because this is the earliest point it was detectable.
    #[error(
        "cannot record '{step}': the previous step left a problem behind — the page reports: {evidence}"
    )]
    PreviousStepIncomplete { step: String, evidence: String },
}

/// Which model backend drives the authoring loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendKind {
    /// Anthropic Messages API.
    Anthropic,
    /// OpenAI's public API, using the OpenAI-compatible chat completions shape.
    OpenAi,
    /// Any custom OpenAI-compatible endpoint (e.g. vLLM serving a local model).
    OpenAiCompatible,
}

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

/// Backend configuration, resolved from the environment.
///
/// Env names mirror the conventions used across Automators products:
/// `FLOWPROOF_AI_PROVIDER` (`anthropic` | `openai`),
/// `FLOWPROOF_AI_BASE_URL`, `FLOWPROOF_AI_API_KEY`, `FLOWPROOF_AI_MODEL`.
/// The API key falls back to `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`. The old
/// `openai-compatible` provider spelling remains accepted for custom endpoints
/// that set `FLOWPROOF_AI_BASE_URL`.
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
                            BackendKind::OpenAi | BackendKind::OpenAiCompatible => {
                                env::var("OPENAI_API_KEY").ok()
                            }
                        });
                config
            },
        )
    }

    /// Whether this configuration can actually make calls: Anthropic and the
    /// public OpenAI API need a key; a custom OpenAI-compatible endpoint needs
    /// a base url (key optional — local vLLM commonly runs without one).
    pub fn is_usable(&self) -> bool {
        match self.kind {
            BackendKind::Anthropic | BackendKind::OpenAi => self.api_key.is_some(),
            BackendKind::OpenAiCompatible => self.base_url.is_some(),
        }
    }

    fn from_provider_name(provider: &str, base_url: Option<String>) -> Result<Self, AgentError> {
        match provider {
            "" | "anthropic" => Ok(Self {
                kind: BackendKind::Anthropic,
                base_url,
                model: None,
                api_key: None,
            }),
            "openai" => Ok(Self {
                kind: BackendKind::OpenAi,
                base_url: base_url.or_else(|| Some(DEFAULT_OPENAI_BASE_URL.to_string())),
                model: None,
                api_key: None,
            }),
            "openai-compatible" => {
                let Some(base_url) = base_url else {
                    return Err(AgentError::Config(
                        "FLOWPROOF_AI_BASE_URL is required for the legacy openai-compatible provider; use FLOWPROOF_AI_PROVIDER=openai for the public OpenAI API".into(),
                    ));
                };
                Ok(Self {
                    kind: BackendKind::OpenAiCompatible,
                    base_url: Some(base_url),
                    model: None,
                    api_key: None,
                })
            }
            other => Err(AgentError::Config(format!(
                "unknown FLOWPROOF_AI_PROVIDER '{other}' (expected 'anthropic' or 'openai')"
            ))),
        }
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
    fn openai_uses_the_public_default_base_url() {
        let ok = BackendConfig::from_provider_name("openai", None).expect("openai config");
        assert_eq!(ok.kind, BackendKind::OpenAi);
        assert_eq!(
            ok.base_url.as_deref(),
            Some("https://api.openai.com/v1"),
            "public OpenAI should not require users to configure a base URL"
        );
        assert!(!ok.is_usable(), "public OpenAI still needs a key");
    }

    #[test]
    fn legacy_openai_compatible_still_requires_base_url() {
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
