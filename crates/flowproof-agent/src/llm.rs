//! Model clients for the authoring agent: Anthropic Messages API and any
//! OpenAI-compatible `/chat/completions` endpoint (e.g. vLLM). Synchronous,
//! temperature 0, small budgets — authoring calls are rare (record/heal
//! only; replay never calls a model).

use serde_json::json;

use crate::{AgentError, BackendConfig, BackendKind};

const MAX_TOKENS: u32 = 1024;
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-5";

/// A minimal chat completion: system + user in, text out.
pub trait ModelClient {
    fn complete(&mut self, system: &str, user: &str) -> Result<String, AgentError>;
    /// `backend/model` identity recorded into the trace header.
    fn identity(&self) -> (String, String);
}

/// HTTP-backed client for the configured backend.
pub struct HttpModelClient {
    config: BackendConfig,
    agent: ureq::Agent,
}

impl HttpModelClient {
    /// Build from environment configuration. Returns `None` when no usable
    /// backend is configured (the recorder then stays rules-only).
    pub fn from_env() -> Option<Self> {
        let config = BackendConfig::from_env().ok()?;
        config.is_usable().then(|| Self::new(config))
    }

    pub fn new(config: BackendConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(ureq::Proxy::try_from_env())
            .build();
        Self {
            config,
            agent: agent_config.into(),
        }
    }

    fn model(&self) -> String {
        self.config
            .model
            .clone()
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_MODEL.to_string())
    }

    fn http_err(context: &str, err: impl std::fmt::Display) -> AgentError {
        AgentError::Config(format!("model call failed ({context}): {err}"))
    }

    fn complete_anthropic(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        let base = self
            .config
            .base_url
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());
        let key = self
            .config
            .api_key
            .clone()
            .ok_or_else(|| AgentError::Config("no API key for anthropic backend".into()))?;
        let response: serde_json::Value = self
            .agent
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send_json(json!({
                "model": self.model(),
                "max_tokens": MAX_TOKENS,
                "temperature": 0,
                "system": system,
                "messages": [{"role": "user", "content": user}],
            }))
            .map_err(|e| Self::http_err("anthropic", e))?
            .body_mut()
            .read_json()
            .map_err(|e| Self::http_err("anthropic response", e))?;
        response["content"][0]["text"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                AgentError::Config(format!("unexpected anthropic response shape: {response}"))
            })
    }

    fn complete_openai(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        let base = self.config.base_url.clone().ok_or_else(|| {
            AgentError::Config("openai-compatible backend needs a base url".into())
        })?;
        let mut request = self
            .agent
            .post(format!("{}/chat/completions", base.trim_end_matches('/')));
        if let Some(key) = &self.config.api_key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let response: serde_json::Value = request
            .send_json(json!({
                "model": self.model(),
                "max_tokens": MAX_TOKENS,
                "temperature": 0,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
            }))
            .map_err(|e| Self::http_err("openai-compatible", e))?
            .body_mut()
            .read_json()
            .map_err(|e| Self::http_err("openai-compatible response", e))?;
        response["choices"][0]["message"]["content"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                AgentError::Config(format!("unexpected completion response shape: {response}"))
            })
    }
}

impl ModelClient for HttpModelClient {
    fn complete(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        match self.config.kind {
            BackendKind::Anthropic => self.complete_anthropic(system, user),
            BackendKind::OpenAiCompatible => self.complete_openai(system, user),
        }
    }

    fn identity(&self) -> (String, String) {
        let backend = match self.config.kind {
            BackendKind::Anthropic => "anthropic",
            BackendKind::OpenAiCompatible => "openai-compatible",
        };
        (backend.to_string(), self.model())
    }
}
