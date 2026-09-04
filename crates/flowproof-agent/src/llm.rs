//! Model clients for the authoring agent: Anthropic Messages API and any
//! OpenAI-compatible `/chat/completions` endpoint (e.g. vLLM). Synchronous,
//! small budgets — authoring calls are rare (record/heal only; replay never
//! calls a model).

use serde_json::json;

use crate::{AgentError, BackendConfig, BackendKind};

// Sonnet 5 can spend more than 1K tokens reasoning before it emits the small
// structured answer authoring needs. With a 1,024-token ceiling the API can
// therefore return a perfectly valid message containing only a signed
// `thinking` block and `stop_reason: "max_tokens"`.
//
// 8K was chosen when a step's answer was small. It is not enough any more: a
// step meaning a whole form answers with a dozen-odd grounded actions, and the
// scene it reasons over now carries each field's value, validity and options.
// The Tricentis vehicle form exhausted 8K on the authoring call and failed the
// record outright — a ceiling that turns a workable step into an error is the
// wrong ceiling. 16K keeps the request bounded with room for both.
const ANTHROPIC_MAX_TOKENS: u32 = 16_384;
const OPENAI_COMPATIBLE_MAX_TOKENS: u32 = 1024;
const DEFAULT_ANTHROPIC_MODEL: &str = "claude-sonnet-5";
const DEFAULT_OPENAI_MODEL: &str = "gpt-5";
const DEFAULT_ANTHROPIC_BASE_URL: &str = "https://api.anthropic.com";

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
        Self::from_env_result().ok().flatten()
    }

    /// Like [`Self::from_env`], but preserves invalid configuration as an
    /// error so recording cannot mistake a typo for an intentional fallback.
    pub fn from_env_result() -> Result<Option<Self>, AgentError> {
        let config = BackendConfig::from_env()?;
        Ok(config.is_usable().then(|| Self::new(config)))
    }

    pub fn new(config: BackendConfig) -> Self {
        let agent_config = ureq::Agent::config_builder()
            .tls_config(
                ureq::tls::TlsConfig::builder()
                    .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                    .build(),
            )
            .proxy(ureq::Proxy::try_from_env())
            // Without this the agent has NO timeout, and a model request that
            // never answers hangs the recording forever - not slowly, but
            // permanently, holding a browser open with no output and nothing
            // to interrupt. A recording that fails is recoverable; one that
            // hangs is not, and it looks like a slow app rather than a stuck
            // tool. Generous enough for a large reply with reasoning ahead of
            // it, far short of "never".
            .timeout_global(Some(std::time::Duration::from_secs(180)))
            // A non-2xx must reach us as a RESPONSE, not as an opaque error:
            // the provider explains itself in the body, and that explanation
            // is the whole diagnostic. See `json_or_error`.
            .http_status_as_error(false)
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
            .unwrap_or_else(|| match self.config.kind {
                BackendKind::Anthropic => DEFAULT_ANTHROPIC_MODEL.to_string(),
                BackendKind::OpenAi | BackendKind::OpenAiCompatible => {
                    DEFAULT_OPENAI_MODEL.to_string()
                }
            })
    }

    fn http_err(context: &str, err: impl std::fmt::Display) -> AgentError {
        AgentError::Config(format!("model call failed ({context}): {err}"))
    }

    /// How much of a provider's error body to quote. An API error is a
    /// sentence; a corporate proxy's HTML interstitial is not, and the first
    /// part of it still identifies the proxy. Bounded so neither floods the
    /// terminal.
    const MAX_ERROR_BODY: usize = 1000;

    /// Read the JSON response, or turn a non-2xx into an error that QUOTES
    /// the provider's own explanation.
    ///
    /// Without the body, every provider-side rejection reads `http status:
    /// 400` and says nothing. The sentence that actually solves it —
    /// "`temperature` is deprecated for this model" — is in the body and was
    /// being discarded, which is the difference between a one-minute fix and
    /// an afternoon.
    ///
    /// The key is redacted if it appears: a misconfigured gateway can echo
    /// the request back, and this text is exactly what someone pastes into a
    /// bug report.
    fn json_or_error(
        context: &str,
        key: Option<&str>,
        response: &mut ureq::http::Response<ureq::Body>,
    ) -> Result<serde_json::Value, AgentError> {
        let status = response.status();
        if !status.is_success() {
            let body = response.body_mut().read_to_string().unwrap_or_default();
            let mut detail = body.trim().to_string();
            if let Some(key) = key.filter(|k| !k.is_empty()) {
                detail = detail.replace(key, "<redacted>");
            }
            if detail.chars().count() > Self::MAX_ERROR_BODY {
                detail = detail
                    .chars()
                    .take(Self::MAX_ERROR_BODY)
                    .collect::<String>()
                    + "…";
            }
            let suffix = if detail.is_empty() {
                " (no response body)".to_string()
            } else {
                format!(": {detail}")
            };
            return Err(AgentError::Config(format!(
                "model call failed ({context}): http status {}{suffix}",
                status.as_u16()
            )));
        }
        response
            .body_mut()
            .read_json()
            .map_err(|e| Self::http_err(&format!("{context} response"), e))
    }

    fn complete_anthropic(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        let base = self
            .config
            .base_url
            .clone()
            .or_else(|| std::env::var("ANTHROPIC_BASE_URL").ok())
            .unwrap_or_else(|| DEFAULT_ANTHROPIC_BASE_URL.to_string());
        let key = self
            .config
            .api_key
            .clone()
            .ok_or_else(|| AgentError::Config("no API key for anthropic backend".into()))?;
        // No `temperature`. The API deprecated it on current models — every
        // Sonnet 5 / Opus 5 request carrying it is rejected with a 400 — so a
        // hardcoded value made LLM authoring unusable on the DEFAULT model,
        // and would break again on every model shipped after this one.
        // Authoring does not need it: the model is handed the live scene and
        // must copy a target token from it verbatim, the result is recorded
        // for review, and replay never calls a model at all.
        let mut response = self
            .agent
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send_json(json!({
                "model": self.model(),
                "max_tokens": ANTHROPIC_MAX_TOKENS,
                "system": system,
                "messages": [{"role": "user", "content": user}],
            }))
            .map_err(|e| Self::http_err("anthropic", e))?;
        let response: serde_json::Value =
            Self::json_or_error("anthropic", Some(&key), &mut response)?;
        // The FIRST TEXT block, not the first block. A reasoning model puts a
        // `thinking` block ahead of its answer, so indexing [0] blindly reads
        // a block with no `text` field and reports the response shape as
        // unexpected — when the response is perfectly ordinary and the answer
        // is one element further on.
        let text = response["content"]
            .as_array()
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find(|b| b["type"] == "text")
                    .and_then(|b| b["text"].as_str())
            })
            .map(str::to_string);
        if let Some(text) = text {
            return Ok(text);
        }

        // Reaching the output ceiling before the first text block is not a
        // malformed provider response. Name the real failure without dumping
        // the (large, opaque) thinking signature into the terminal.
        if response["stop_reason"].as_str() == Some("max_tokens") {
            return Err(AgentError::Config(format!(
                "anthropic model '{}' exhausted its {ANTHROPIC_MAX_TOKENS}-token output budget before producing a text answer",
                self.model()
            )));
        }

        Err(AgentError::Config(format!(
            "unexpected anthropic response shape: {response}"
        )))
    }

    fn complete_openai(&mut self, system: &str, user: &str) -> Result<String, AgentError> {
        let base = self
            .config
            .base_url
            .clone()
            .ok_or_else(|| AgentError::Config("openai backend needs a base url".into()))?;
        let mut request = self
            .agent
            .post(format!("{}/chat/completions", base.trim_end_matches('/')));
        if let Some(key) = &self.config.api_key {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        // `temperature` stays here: OpenAI's API and a local vLLM both accept
        // it, and determinism is worth having where it is free. The
        // deprecation that forced it out of the Anthropic request is specific
        // to that API's current models.
        let mut response = request
            .send_json(json!({
                "model": self.model(),
                "max_tokens": OPENAI_COMPATIBLE_MAX_TOKENS,
                "temperature": 0,
                "messages": [
                    {"role": "system", "content": system},
                    {"role": "user", "content": user},
                ],
            }))
            .map_err(|e| Self::http_err("openai-compatible", e))?;
        let response: serde_json::Value = Self::json_or_error(
            "openai-compatible",
            self.config.api_key.as_deref(),
            &mut response,
        )?;
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
            BackendKind::OpenAi | BackendKind::OpenAiCompatible => {
                self.complete_openai(system, user)
            }
        }
    }

    fn identity(&self) -> (String, String) {
        let backend = match self.config.kind {
            BackendKind::Anthropic => "anthropic",
            BackendKind::OpenAi => "openai",
            BackendKind::OpenAiCompatible => "openai-compatible",
        };
        (backend.to_string(), self.model())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_uses_provider_specific_default_models() {
        let anthropic = HttpModelClient::new(BackendConfig {
            kind: BackendKind::Anthropic,
            base_url: None,
            model: None,
            api_key: Some("sk-ant".into()),
        });
        assert_eq!(
            anthropic.identity(),
            ("anthropic".into(), "claude-sonnet-5".into())
        );

        let openai = HttpModelClient::new(BackendConfig {
            kind: BackendKind::OpenAi,
            base_url: Some("https://api.openai.com/v1".into()),
            model: None,
            api_key: Some("sk-openai".into()),
        });
        assert_eq!(openai.identity(), ("openai".into(), "gpt-5".into()));
    }
}
