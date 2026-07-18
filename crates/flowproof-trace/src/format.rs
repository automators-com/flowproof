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
    Wait(Params),
    Assert(Assertion),
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
}
