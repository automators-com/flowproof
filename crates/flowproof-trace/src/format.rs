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
// Never stored in bulk: `parse` yields one at a time and callers unwrap
// immediately into Header/Vec<Step>, so variant size imbalance is moot.
#[allow(clippy::large_enum_variant)]
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
    /// The named surfaces of a MULTI-surface trace: `name -> AppInfo`,
    /// mirroring the spec's `apps:`. When present, `app` carries the
    /// reserved name `multi` with [`Adapter::Multi`] — deliberately not a
    /// copy of any one surface, so an engine without multi-surface support
    /// fails LOUDLY at load (an unknown adapter variant) instead of
    /// replaying every step against whichever surface happened to be
    /// first. Absent on single-surface traces, which serialize
    /// byte-identically to before this field existed.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub apps: std::collections::BTreeMap<String, AppInfo>,
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
    /// Browser launch/emulation config copied from the spec at record
    /// time, applied identically at record and every replay (web flows):
    /// viewport/mobile emulation, user-agent, extra Chrome flags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserSetup>,
    /// The named security control this flow validates, copied from the
    /// spec's `control:` block at record time. ADDITIVE and OMITTED when the
    /// flow has no `control:` block, so a flow that does not use the feature
    /// serializes BYTE-IDENTICAL to before. The id is the stable join key an
    /// audit report folds flows by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<Control>,
}

/// A named security control a flow validates. The `id` is author-chosen,
/// dotted, lowercase, and STABLE across renames, moves between suites, and
/// re-records, because it is the join key between what an auditor tracks and
/// what CI ran. `title`/`description` are author metadata. The id's format
/// and per-suite uniqueness are enforced in the spec crate; this type is the
/// trace-shared carrier.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    /// THE header slot for a window title, whichever spec key supplied it:
    /// `app.window_title` for windows flows, `window.title` for vision.
    /// Stored RAW - a `${VAR}` reference never arrives here resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    /// The command line a `windows` flow launched, stored RAW. Lives here
    /// rather than in a separate block for the same reason `url` does:
    /// per-adapter launch detail belongs on the app it describes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Window geometry APPLIED before the first step, so replay reproduces
    /// the shape recording used. When the spec omitted a position, this
    /// records where the window actually landed - which upgrades an
    /// unpinned position into a pinned one for free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<WindowGeometry>,
    /// For `web` traces: the URL the flow was recorded against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// For `sap` traces: the user the recording logged in as, stored RAW
    /// (a `${VAR}` reference never arrives here resolved).
    ///
    /// The identity is part of what a recording MEANS — an order created by
    /// a clerk and one created by an approver are different evidence — so a
    /// trace that could not name it would not be reviewable. The PASSWORD is
    /// deliberately not here and has no field: it is resolved from the spec
    /// at every launch, so a committed trace has nothing to leak and nothing
    /// to redact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub login_user: Option<String>,
    /// For a web SURFACE of a multi-surface trace: this surface's browser
    /// launch/emulation config, the per-surface counterpart of the
    /// header-level `browser` (which stays the single-surface spelling).
    /// Applied identically at record and every replay, so THIS surface
    /// keeps the shape it was recorded on. Additive optional: absent
    /// everywhere it is unused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserSetup>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Applied window geometry. Integers, never `${VAR}` references: geometry
/// is a determinism precondition, and a precondition that varies by
/// environment is not one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
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
    /// The sentinel adapter of a MULTI-surface header (`header.apps`
    /// present): per-surface adapters live on the surface entries, and
    /// this value is deliberately not one of them so an engine predating
    /// multi-surface fails to parse the trace — a loud "unknown variant"
    /// at load, never a replay against the wrong surface. Selector
    /// provenance never uses it.
    Multi,
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
    /// The named surface (a key of `header.apps`) that executed this step —
    /// how a multi-surface replay knows which driver a step belongs to.
    /// Absent on single-surface traces (the header's one `app` is the
    /// surface), which therefore serialize byte-identically to before this
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
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
    /// Move the pointer onto an element (a single `mouseMoved`, no
    /// press/release). Hover state persists until the next explicit
    /// pointer action.
    Hover(Params),
    Drag(Params),
    Scroll(Params),
    TypeText(TypeTextParams),
    /// Read a target's text into a flow-scoped name: `{"name": "<name>"}`.
    /// The VALUE is never stored - only the name - so a captured balance or
    /// order number stays out of the reviewable artifact.
    Capture(Params),
    /// Wait for a browser download to land and complete, capturing its
    /// resolved path into a flow-scoped name: `{"name": "<name>"}`, an
    /// optional `{"timeout_ms": …}`. No selectors - there is no UI target,
    /// like `TypeFocused`. The path is stored in `captures` at execution,
    /// never in the trace, the same indirection `Capture` uses - it is not
    /// secret, but it is a value only this run's filesystem can produce.
    CaptureDownload(Params),
    /// Drive a checkbox-like control to a state: `{"checked": bool}`.
    /// Set-state rather than toggle, so replaying it is idempotent.
    SetChecked(Params),
    PressKey(PressKeyParams),
    Upload(UploadParams),
    Wait(Params),
    Assert(Assertion),
}

/// A native JavaScript dialog (`alert`/`confirm`/`prompt`/`beforeunload`)
/// disposition, folded into a trigger action's open `params` bag under the
/// `dialog` key. STRICTLY ADDITIVE: a trace without a dialog serializes
/// byte-identically to before this field existed. A trace that USES `dialog`
/// needs an engine at least the version that introduced it - an older
/// replayer would ignore the field and never arm the handler, so the
/// declared dialog would hang rather than be answered (a normal
/// forward-compat note, not a format break). Web-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dialog {
    pub disposition: DialogDisposition,
    /// The recorded message text, matched `contains`. Omitted for the bare
    /// `accepting the dialog` / `dismissing the dialog` forms (match any
    /// message).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// How `message` is matched. Always `contains` in v1 - the one match
    /// mode - and always serialized, so the assertion semantics are explicit
    /// in the trace.
    #[serde(rename = "match", default = "default_dialog_match")]
    pub match_mode: String,
    /// The prompt reply supplied on accept. Authored input like
    /// `TypeText.text`: a `${VAR}` reference resolves at execution (record
    /// and every replay), so only the reference travels here - never the
    /// value the page received. Omitted for a plain accept/dismiss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply: Option<String>,
}

fn default_dialog_match() -> String {
    "contains".to_string()
}

/// Accept (OK, supplying any prompt reply) or dismiss (Cancel / close) a
/// native JavaScript dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DialogDisposition {
    Accept,
    Dismiss,
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

/// Browser launch/emulation config (web flows): viewport + mobile
/// emulation, user-agent override, and extra Chrome flags. Copied from the
/// spec into the trace header so record and every replay run the SAME
/// browser shape. `deny_unknown_fields` deliberately: a silently dropped
/// emulation field changes what the flow tests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSetup {
    /// Viewport / device emulation, applied before navigation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub viewport: Option<ViewportSetup>,
    /// Navigator user-agent override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// Extra Chrome command-line flags. Forces a private (non-shared)
    /// browser for the flow, since flags only apply at process start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// A pinned clock, applied before navigation so a date-dependent flow
    /// is deterministic (#58's sibling, GAP-P). Web-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock: Option<ClockSetup>,
    /// A pinned `Math.random`, the clock's sibling: applied before
    /// navigation so a flow against a page that mints random values is
    /// deterministic. Web-only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub random: Option<RandomSetup>,
    /// Where downloaded files land, applied at launch (record and every
    /// replay) so `Wait until the download completes as <name>` has a
    /// fixed place to look. Stored RAW - a `${VAR}` reference resolves at
    /// launch, never here. `None` = the driver creates its own per-launch
    /// temp directory, which is enough determinism for a wait that only
    /// needs "nothing else writes here," not a specific path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub downloads_dir: Option<String>,
}

/// The seed for the pinned PRNG. A literal, never a `${VAR}` - a seed
/// resolved from the environment would make one trace mean different
/// things on different machines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RandomSetup {
    pub seed: u32,
}

impl BrowserSetup {
    pub fn is_empty(&self) -> bool {
        self.viewport.is_none()
            && self.random.is_none()
            && self.user_agent.is_none()
            && self.args.is_empty()
            && self.clock.is_none()
            && self.downloads_dir.is_none()
    }
}

/// A pinned browser clock (GAP-P). `at` freezes what the page reads as
/// "now" - a fixed offset on `Date`, so the clock STARTS at `at` and
/// advances at real wall rate (not a hard freeze; v1 has no tick). Both
/// fields are LITERALS, never `${VAR}`: a determinism precondition that
/// varied by environment would not be one. Applied before navigation via a
/// `Date` shim plus a CDP timezone override, identically at record and
/// replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClockSetup {
    /// The pinned instant, RFC 3339 (`2026-01-15T09:00:00Z`). Pick a
    /// mid-day time so no step straddles a pinned midnight.
    pub at: String,
    /// An IANA timezone id (`Europe/Berlin`). Optional but recommended:
    /// without it, local dates and "last 7 days" boundaries still depend on
    /// the runner's zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// Device-metrics emulation: the mobile half of a browser setup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ViewportSetup {
    pub width: u32,
    pub height: u32,
    /// Device pixel ratio (default 1.0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_scale_factor: Option<f64>,
    /// Mobile layout mode (meta-viewport honored, mobile UA hints).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mobile: Option<bool>,
    /// Emulate a touch screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub touch: Option<bool>,
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
    /// Screenshot comparison against a named baseline PNG stored in the
    /// trace's sibling `<trace-stem>.baselines/` directory (`baseline` is
    /// the file name — the trace stays relocatable as a bundle). `masks`
    /// are selector strings (text anchor / `css:` / `id:`) whose element
    /// rects are blanked before compare, identically at record (baseline
    /// minting) and replay. `threshold` is the fraction of pixels allowed
    /// to differ (default 0: exact).
    VisualDiff {
        baseline: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        region: Option<Region>,
        #[serde(skip_serializing_if = "Option::is_none")]
        threshold: Option<f64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        masks: Vec<String>,
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
    /// Out-of-band spreadsheet probe: reads a workbook on disk directly
    /// (`calamine`), not through UI Automation over Excel's patchy grid
    /// support. `path` may carry `${captured.x}`/`${VAR}` references — the
    /// download this checks was itself captured by an earlier step — both
    /// resolved at probe time, never stored. The cell is addressed by
    /// EITHER `at` (an absolute `A1` reference) OR `column`+`row_contains`
    /// (a header/anchor pair, resolved like a table cell on a live page) —
    /// exactly one, enforced at parse time, not here. `expect` carries
    /// `equals`/`contains`/`timeout_ms`, same shape as `Sql`/`Api`.
    Spreadsheet {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        sheet: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        column: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        row_contains: Option<String>,
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

#[cfg(test)]
mod control_header_tests {
    use super::*;

    fn base_header() -> Header {
        Header {
            format: crate::FORMAT_NAME.to_string(),
            version: crate::FORMAT_VERSION,
            trace_id: "t-1".into(),
            recorded_at: "2026-07-24T00:00:00Z".into(),
            spec: None,
            apps: Default::default(),
            app: AppInfo {
                name: "web".into(),
                adapter: Adapter::Web,
                window_title: None,
                command: None,
                browser: None,
                geometry: None,
                url: Some("http://x".into()),
                login_user: None,
                version: None,
            },
            agent: None,
            env: EnvInfo {
                os: "test".into(),
                resolution: (1, 1),
                dpi_scale: None,
                locale: None,
            },
            recording: None,
            redaction: Vec::new(),
            session: None,
            mock: Vec::new(),
            browser: None,
            control: None,
        }
    }

    /// The additive invariant: a header with NO `control:` block serializes
    /// byte-identical to before the field existed - the `control` key is
    /// absent entirely, so an old reader and a byte-for-byte diff are both
    /// unaffected.
    #[test]
    fn a_header_without_control_omits_the_key_and_round_trips() {
        let header = base_header();
        let json = serde_json::to_string(&header).expect("serializes");
        assert!(
            !json.contains("control"),
            "no control key on a control-less header: {json}"
        );
        let back: Header = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, header);
        assert!(back.control.is_none());

        // A pre-feature header (no `control` field at all) still deserializes.
        let stripped: Header = serde_json::from_str(&json).expect("legacy parses");
        assert!(
            stripped.control.is_none(),
            "absent control defaults to None"
        );
    }

    /// A control-bearing header carries the id (and optional metadata) and
    /// survives a serialize round-trip unchanged.
    #[test]
    fn a_control_header_round_trips_with_its_id() {
        let mut header = base_header();
        header.control = Some(Control {
            id: "ac.customers.delete.viewer-denied".into(),
            title: Some("Viewer role is denied customer deletion".into()),
            description: None,
        });
        let json = serde_json::to_string(&header).expect("serializes");
        assert!(json.contains("ac.customers.delete.viewer-denied"), "{json}");
        assert!(!json.contains("description"), "absent metadata is omitted");
        let back: Header = serde_json::from_str(&json).expect("round-trips");
        assert_eq!(back, header);
    }
}
