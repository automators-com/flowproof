//! Serde types for trace format v1. The normative definition is
//! `docs/trace-format.md` + `schema/trace-v1.schema.json`; a fixture test
//! keeps these types and the schema in agreement.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::SelectorTier;

/// `[x, y, width, height]` in physical pixels.
pub type Region = (i64, i64, u64, u64);

/// Open-ended parameter bag for actions whose shape v1 does not pin down.
pub type Params = Map<String, Value>;

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("invalid trace line: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported trace format '{format}' version {version}")]
    UnsupportedFormat { format: String, version: u32 },
}

/// One line of a trace file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TraceLine {
    Header(Header),
    Step(Step),
}

impl TraceLine {
    /// Parse a single JSON-lines line. If it is a header, the format
    /// identity is verified.
    pub fn parse(line: &str) -> Result<Self, TraceError> {
        let parsed: TraceLine = serde_json::from_str(line)?;
        if let TraceLine::Header(header) = &parsed {
            if header.format != crate::FORMAT_NAME || header.version != crate::FORMAT_VERSION {
                return Err(TraceError::UnsupportedFormat {
                    format: header.format.clone(),
                    version: header.version,
                });
            }
        }
        Ok(parsed)
    }
}

/// Line 1 of every trace file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Header {
    pub format: String,
    pub version: u32,
    pub trace_id: String,
    pub recorded_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spec: Option<SpecRef>,
    pub app: AppInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentInfo>,
    pub env: EnvInfo,
    /// The authoring execution's recording bundle, if one was captured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingRef>,
    /// Redaction rules copied from the spec at record time, so every replay
    /// masks identically without needing the spec. Free-form rule objects
    /// (the driver's redaction layer owns their schema).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redaction: Vec<Value>,
    /// Session state applied before the page loads (cookies, localStorage),
    /// copied from the spec so replays authenticate identically. Values may
    /// be `${VAR}` secret references — resolved at apply time, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionSetup>,
    /// Network mock rules copied from the spec at record time, applied
    /// identically at record and every replay (web flows): a request whose
    /// URL matches is answered locally, never leaving the browser. What was
    /// mocked at record MUST be mocked at replay — that is what keeps the
    /// two executions equivalent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mock: Vec<MockRule>,
}

/// One network mock: match by URL substring (and optionally method), answer
/// with a canned response. `body` is any JSON — a string is served verbatim
/// (`text/plain` default), anything else serializes to JSON
/// (`application/json` default); `content_type` overrides either.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MockRule {
    /// Substring the request URL must contain.
    pub url_contains: String,
    /// Uppercase HTTP method filter; absent = any method.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default = "default_mock_status")]
    pub status: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
}

fn default_mock_status() -> u16 {
    200
}

/// Pre-launch session state: how authenticated app flows start without a
/// login UI walk (the Playwright storageState / cookie-fixture pattern).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SessionSetup {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cookies: Vec<SessionCookie>,
    /// Seeded into localStorage before any page script runs.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub local_storage: std::collections::BTreeMap<String, String>,
}

impl SessionSetup {
    /// Resolve every `${VAR}` reference for application, returning
    /// `(cookies as (name, value, domain), local_storage pairs)`. The setup
    /// itself — and the trace — keeps the references.
    #[allow(clippy::type_complexity)]
    pub fn resolved(
        &self,
    ) -> Result<
        (Vec<(String, String, Option<String>)>, Vec<(String, String)>),
        crate::secret::MissingSecret,
    > {
        let cookies = self
            .cookies
            .iter()
            .map(|c| {
                Ok((
                    c.name.clone(),
                    crate::secret::resolve_refs(&c.value)?,
                    c.domain.clone(),
                ))
            })
            .collect::<Result<Vec<_>, crate::secret::MissingSecret>>()?;
        let local_storage = self
            .local_storage
            .iter()
            .map(|(k, v)| Ok((k.clone(), crate::secret::resolve_refs(v)?)))
            .collect::<Result<Vec<_>, crate::secret::MissingSecret>>()?;
        Ok((cookies, local_storage))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionCookie {
    pub name: String,
    /// May be a `${VAR}` reference — resolved from the environment at the
    /// moment the cookie is set, recording and every replay.
    pub value: String,
    /// Defaults to the flow URL's host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
}

/// Reference to a recording bundle from the artifact that owns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecordingRef {
    /// Bundle format discriminator (e.g. `filmstrip/1`).
    pub format: String,
    /// Bundle directory, relative to the owning artifact's location.
    pub dir: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
}

/// A step's time range within its execution's recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepRecording {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Link back to the YAML flow spec the trace was recorded from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpecRef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub adapter: Adapter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// For `web` traces: the URL the flow was recorded against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Perception/adapter source. Doubles as selector provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Adapter {
    Uia,
    SapCom,
    Web,
    Vision,
    /// No UI at all: the flow is out-of-band assertions only (SQL / API).
    Api,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub backend: AgentBackend,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentBackend {
    Anthropic,
    OpenaiCompatible,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EnvInfo {
    pub os: String,
    pub resolution: (u32, u32),
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dpi_scale: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
}

/// One recorded step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub id: String,
    pub intent: String,
    pub action: Action,
    pub selectors: Vec<Selector>,
    pub sync: Sync,
    pub artifacts: Artifacts,
}

/// The action performed in a step. Adjacently tagged as
/// `{"type": …, "params": …}` to match the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "params", rename_all = "snake_case")]
pub enum Action {
    Launch(Params),
    FocusWindow(Params),
    Click(Params),
    DoubleClick(Params),
    RightClick(Params),
    Drag(Params),
    Scroll(Params),
    TypeText(TypeTextParams),
    PressKey(PressKeyParams),
    Upload(UploadParams),
    Wait(Params),
    Assert(Assertion),
}

/// Params for `upload`: set a file on a file-chooser input. The path is
/// stored as written in the spec; relative paths resolve against the
/// process working directory at execution time (record and replay alike).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UploadParams {
    pub path: String,
    #[serde(flatten)]
    pub extra: Params,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeTextParams {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submit: Option<bool>,
    #[serde(flatten)]
    pub extra: Params,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PressKeyParams {
    pub key: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modifiers: Vec<KeyModifier>,
    #[serde(flatten)]
    pub extra: Params,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyModifier {
    Ctrl,
    Alt,
    Shift,
    Win,
    /// The portable primary modifier (Playwright's `ControlOrMeta`):
    /// stored neutrally in the trace and resolved at EXECUTION time —
    /// Meta on macOS, Ctrl everywhere else — so a trace recorded on one
    /// OS replays on another.
    Mod,
}

/// First-class assertion steps (`action.type == "assert"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Assertion {
    ElementState {
        expect: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        selector_ref: Option<usize>,
    },
    OcrText {
        text: String,
        #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
        match_mode: Option<MatchMode>,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
    },
    VisualDiff {
        baseline: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
        #[serde(skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
    },
    /// Out-of-band DB probe. `connection` is a name resolved from local
    /// config at run time; credentials never live in the trace.
    Sql {
        connection: String,
        query: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        expect: Option<Value>,
    },
    /// Out-of-band HTTP probe; secrets referenced by name only.
    Api {
        request: ApiRequest,
        #[serde(skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expect: Option<Value>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    Equals,
    Contains,
    Regex,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiRequest {
    pub method: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<Value>,
    /// Request headers (e.g. Authorization). Values are stored as raw
    /// `${VAR}` references and resolved only when the probe fires.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
}

/// One rung of the selector ladder as recorded for a step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selector {
    pub tier: SelectorTier,
    pub provenance: Adapter,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f64>,
    pub payload: Params,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sync {
    pub pre: Vec<Condition>,
    pub post: Vec<Condition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Condition {
    ElementExists {
        timeout_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        selector_ref: Option<usize>,
    },
    ElementState {
        timeout_ms: u64,
        expect: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        selector_ref: Option<usize>,
    },
    WindowTitle {
        timeout_ms: u64,
        equals: String,
    },
    OcrTextPresent {
        timeout_ms: u64,
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
    },
    VisualStable {
        timeout_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
    },
}

/// Content-addressed screenshot references (`sha256:<hex>`); blobs live in
/// the artifact store, not in the trace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Artifacts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_screenshot: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_screenshot: Option<String>,
    /// This step's time range in the header's recording bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<StepRecording>,
}
