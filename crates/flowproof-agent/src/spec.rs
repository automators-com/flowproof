//! YAML flow specs: natural-language steps plus a target app id.
//!
//! ```yaml
//! name: Add two numbers
//! app: calc
//! steps:
//!   - Type 5
//!   - Press plus
//!   - Type 3
//!   - Press equals
//!   - assert: display shows 8
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum SpecError {
    #[error("cannot read spec {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid spec: {0}")]
    Parse(#[from] serde_yaml::Error),
    #[error("spec has no steps")]
    Empty,
    #[error("invalid foreach: {0}")]
    Foreach(String),
    #[error("invalid clock: {0}")]
    Clock(String),
    #[error("invalid window: {0}")]
    Window(String),
    #[error("invalid agent flow: {0}")]
    Agent(String),
}

/// `app:` is either a registry id (`web`, `calc`, `notepad`, `sap`,
/// `vision`, `api`) or a Windows launch mapping. The scalar form is what
/// every existing spec uses and its meaning is unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AppSpec {
    Id(String),
    /// `app: {command, window_title}` - drive an arbitrary Windows program.
    /// Both fields may carry `${VAR}`, resolved at launch and stored RAW.
    ///
    /// `command` is executed code: the same trust surface as `env_from`.
    /// A spec is code.
    Launch {
        command: String,
        window_title: String,
    },
}

impl AppSpec {
    /// The app id a driver is selected by. The mapping form reports the
    /// reserved id `windows`.
    pub fn id(&self) -> &str {
        match self {
            AppSpec::Id(id) => id,
            AppSpec::Launch { .. } => "windows",
        }
    }

    pub fn launch_parts(&self) -> Option<(&str, &str)> {
        match self {
            AppSpec::Id(_) => None,
            AppSpec::Launch {
                command,
                window_title,
            } => Some((command, window_title)),
        }
    }
}

impl From<&str> for AppSpec {
    fn from(id: &str) -> Self {
        AppSpec::Id(id.to_string())
    }
}

impl From<String> for AppSpec {
    fn from(id: String) -> Self {
        AppSpec::Id(id)
    }
}

impl std::fmt::Display for AppSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

/// `window:` is either a bare title (vision shorthand) or a mapping of
/// title and/or geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum WindowSpec {
    Title(String),
    Full(WindowConfig),
}

/// The mapping form of `window:`. Geometry values are literal integers, not
/// `${VAR}` references: geometry is a determinism precondition, and a
/// precondition that varies by environment is not one.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WindowConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
}

impl WindowSpec {
    pub fn title(&self) -> Option<&str> {
        match self {
            WindowSpec::Title(t) => Some(t),
            WindowSpec::Full(c) => c.title.as_deref(),
        }
    }

    pub fn config(&self) -> WindowConfig {
        match self {
            WindowSpec::Title(t) => WindowConfig {
                title: Some(t.clone()),
                ..WindowConfig::default()
            },
            WindowSpec::Full(c) => c.clone(),
        }
    }
}

/// The system under test for an `app: agent` flow. Exactly one of two
/// drivers: a `command` flowproof starts, or a `url` flowproof POSTs to
/// drive an already-running service. `validate_agent` enforces the choice.
///
/// `command` is executed code, the same trust surface as `env_from` - a
/// spec is code. The proxy's base URL is injected into the process's
/// environment, so the agent talks to flowproof believing it is the model.
///
/// `url` names a service flowproof did NOT start: it is already running and
/// already points its model calls at the local `proxy_port`. flowproof binds
/// the proxy at that port and POSTs `{"prompt": ...}` to `url` to trigger a
/// turn. Because flowproof does not own the process, it cannot inject `env`
/// into it (only `command` can carry `env:`); `headers:` is how a `url:`
/// driver carries auth or routing on the trigger POST instead.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// The command that starts the system under test. Exactly one of
    /// `command` or `url` is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// The URL flowproof POSTs to drive an already-running service. Exactly
    /// one of `command` or `url` is set. May carry `${VAR}` references.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// The local port the proxy binds when driving a `url:` service - the
    /// port that service already points its model calls at. Required with
    /// `url`, meaningless with `command` (which gets an ephemeral port).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_port: Option<u16>,
    /// Extra environment for the process. Applied on top of the injected
    /// proxy URL, so a flow whose client reads a non-standard variable can
    /// name it here. Values may carry `${VAR}` references. `command:` only -
    /// flowproof cannot inject env into a service it did not start.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Headers sent on the trigger `POST <url>` (e.g. Authorization). `url:`
    /// only. Values may carry `${VAR}` references, resolved at execution and
    /// never stored, exactly like `assert_api` headers.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
}

/// One tool mocked at the model boundary. Its `result` is returned to the
/// agent as the tool's output, so a multi-step trajectory proceeds without
/// anything real being executed - and because the result is spec-authored,
/// the arguments a DOWNSTREAM call should thread from it are known when the
/// spec is written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolMock {
    pub name: String,
    /// The tool's return value, any JSON. Absent defaults to null, which
    /// makes the entry a declaration only: the tool is NOT mocked, its real
    /// result passes through unsubstituted (see `mocks_of` in the CLI's
    /// agent_flow, which keeps only non-null results), and the entry still
    /// validates an `assert_tool_call` target.
    #[serde(default)]
    pub result: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FlowSpec {
    pub name: String,
    /// App id resolved via `flowproof_driver::resolve_app` (e.g. `calc`),
    /// or `web` for browser flows.
    pub app: AppSpec,
    /// For `app: web`: the URL to open (relative paths become `file://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// For `app: sap`: the SAP Logon connection description to open when no
    /// session is already running (e.g. `S/4HANA Development`). Omitted =
    /// attach to whatever logged-in SAP GUI session exists. May carry
    /// `${VAR}` references, resolved at launch time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// The window this flow drives: which one, and what shape.
    ///
    /// A bare string is shorthand for `{title: …}` and is vision-only -
    /// `title` is an ATTACH selector for a window flowproof never launched,
    /// which is a different thing from `app.window_title`, the launch
    /// parameter naming which window of a process it started. Each app kind
    /// has exactly one spelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowSpec>,
    /// Regions to mask in every persisted frame (password fields are always
    /// masked, with or without rules here). Copied into the trace header at
    /// record time so replays redact identically without the spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redact: Vec<flowproof_driver::RedactionRule>,
    /// Session state (cookies, localStorage) applied before the page loads —
    /// how authenticated flows start without a login walk. Values may be
    /// `${VAR}` references, resolved at apply time and never stored. Copied
    /// into the trace header so replays authenticate identically.
    ///
    /// Accepted strictness gap: `SessionSetup` is the trace-shared type
    /// (trace v1 allows additive optional fields), so unknown keys INSIDE
    /// `session:` are not rejected — only spec-owned types deny them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<flowproof_trace::format::SessionSetup>,
    /// Skip this flow (visible as junit `skipped`, exit 0) unless every
    /// listed environment variable is set and non-empty — first-class
    /// env-flag gating (`RUN_AGENT_E2E`-style) instead of invisible bash
    /// guards. Checked after suite env applies, so `suite.yaml` can
    /// satisfy it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skip_unless_env: Vec<String>,
    /// Network mock rules (web flows): requests matching `url_contains`
    /// are answered locally with the canned response — at record AND every
    /// replay identically (the rules travel in the trace header). The tool
    /// for third-party calls and hard-to-provoke server states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mock: Vec<flowproof_trace::format::MockRule>,
    /// Browser launch/emulation config (web flows): viewport/mobile
    /// emulation, user-agent override, extra Chrome flags. Copied into the
    /// trace header so record and every replay run the same browser shape.
    /// A suite's `browser:` applies when the spec has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<flowproof_trace::format::BrowserSetup>,
    /// The system under test, for `app: agent` flows. Required for that
    /// app and meaningless for any other; `validate` enforces both halves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentSpec>,
    /// Tools mocked at the model boundary (`app: agent`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolMock>,
    /// Forbid any tool call no `assert_tool_call` listed (`app: agent`).
    /// The default is subsequence matching, which tolerates unlisted
    /// calls; `strict: true` is for flows where the exact call set is the
    /// contract.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strict: bool,
    pub steps: Vec<SpecStep>,
}

impl FlowSpec {
    /// Check `window:` against the app kind. Each app kind has exactly ONE
    /// spelling for naming a window, and geometry means nothing for a
    /// browser or an api flow, so every wrong combination is a parse error
    /// that names the right spelling rather than being silently ignored.
    /// Check the `app: agent` surface: the `agent:` block, the `tools:`
    /// block, and the prompt/tool-call steps all belong to that app and
    /// only that app. Each half is enforced, so a misplaced block is a
    /// parse error naming the mismatch rather than something silently
    /// ignored - the same contract every other app kind gets.
    fn validate_agent(&self) -> Result<(), SpecError> {
        let bad = |m: String| Err(SpecError::Agent(m));
        let is_agent = self.app.id() == "agent";

        let agent_only_step = self.steps.iter().find(|step| {
            matches!(
                step,
                SpecStep::Prompt { .. }
                    | SpecStep::AssertToolCall { .. }
                    | SpecStep::AssertNoToolCall { .. }
            )
        });

        if is_agent {
            let Some(agent) = self.agent.as_ref() else {
                return bad(
                    "an `app: agent` flow needs an `agent:` block naming the command to run \
                     or a url: to drive"
                        .into(),
                );
            };
            // Exactly one system under test: a command flowproof starts, or a
            // url it drives. A spec is code; two systems under test is not a
            // choice flowproof can make for the author.
            match (&agent.command, &agent.url) {
                (None, None) => {
                    return bad(
                        "an `app: agent` flow needs an `agent:` block naming the command to run \
                         or a url: to drive"
                            .into(),
                    );
                }
                (Some(_), Some(_)) => {
                    return bad(
                        "agent.command and agent.url are two systems under test; a flow drives \
                         exactly one"
                            .into(),
                    );
                }
                (Some(command), None) => {
                    if command.trim().is_empty() {
                        return bad(
                            "`agent.command` is blank; a spec is code, but an empty command is not"
                                .into(),
                        );
                    }
                    // headers: rides the url: trigger POST; a command driver
                    // has nowhere to put them.
                    if !agent.headers.is_empty() {
                        return bad(
                            "`agent.headers` is sent on the `url:` trigger POST; a `command:` \
                             driver has no request to attach them to"
                                .into(),
                        );
                    }
                    if agent.proxy_port.is_some() {
                        return bad(
                            "`agent.proxy_port` only means something with `url:`; a `command:` \
                             driver gets an ephemeral port"
                                .into(),
                        );
                    }
                }
                (None, Some(url)) => {
                    if url.trim().is_empty() {
                        return bad(
                            "`agent.url` is blank; a spec is code, but an empty url is not".into(),
                        );
                    }
                    if agent.proxy_port.is_none() {
                        return bad(
                            "agent.url needs a proxy_port: the running service must already point \
                             its model calls at that local port"
                                .into(),
                        );
                    }
                    // env cannot be injected into a service flowproof did not
                    // start; naming it here is a mistake, never silently
                    // ignored.
                    if !agent.env.is_empty() {
                        return bad(
                            "`agent.env` is injected into a process flowproof starts; a `url:` \
                             driver names an already-running service, so use `headers:` to carry \
                             what it needs on the trigger"
                                .into(),
                        );
                    }
                }
            }
            if !self
                .steps
                .iter()
                .any(|s| matches!(s, SpecStep::Prompt { .. }))
            {
                return bad(
                    "an `app: agent` flow has no `prompt:` step, so the agent is never given                      anything to do"
                        .into(),
                );
            }
            // Tool names must be distinct: a duplicate is almost always a
            // copy-paste, and two mocks for one name has no defined winner.
            let mut seen = std::collections::BTreeSet::new();
            for tool in &self.tools {
                if !seen.insert(tool.name.as_str()) {
                    return bad(format!("tool `{}` is mocked twice", tool.name));
                }
            }
            return Ok(());
        }

        // Not an agent flow: the agent-only surface must be absent.
        if self.agent.is_some() {
            return bad(format!(
                "`agent:` belongs to an `app: agent` flow, but this is `app: {}`",
                self.app.id()
            ));
        }
        if !self.tools.is_empty() {
            return bad(format!(
                "`tools:` mocks the model boundary of an `app: agent` flow, but this is `app: {}`",
                self.app.id()
            ));
        }
        if self.strict {
            return bad("`strict:` only means something for an `app: agent` flow".into());
        }
        if let Some(step) = agent_only_step {
            return bad(format!(
                "`{}` is an agent step, but this is `app: {}`",
                step.intent().split(':').next().unwrap_or("that step"),
                self.app.id()
            ));
        }
        Ok(())
    }

    /// Check `browser.clock` (GAP-P). Web-only, both fields are LITERALS
    /// (a determinism precondition cannot vary by environment), and `at`
    /// must be a real RFC 3339 instant - a typo that silently disabled the
    /// pin and ran at wall time is exactly the failure this feature exists
    /// to remove.
    fn validate_clock(&self) -> Result<(), SpecError> {
        let Some(clock) = self.browser.as_ref().and_then(|b| b.clock.as_ref()) else {
            return Ok(());
        };
        let bad = |m: String| Err(SpecError::Clock(m));
        if self.app.id() != "web" {
            return bad(format!(
                "clock control is web-only, but this is `app: {}`",
                self.app.id()
            ));
        }
        if clock.at.contains("${") {
            return bad("`at` is a literal instant, never a `${VAR}`: a pinned clock                         that varied by environment would not be deterministic"
                .into());
        }
        if chrono::DateTime::parse_from_rfc3339(&clock.at).is_err() {
            return bad(format!(
                "`{}` is not an RFC 3339 instant (e.g. `2026-01-15T09:00:00Z`)",
                clock.at
            ));
        }
        if let Some(tz) = &clock.timezone {
            if tz.contains("${") {
                return bad("`timezone` is a literal IANA id, never a `${VAR}`".into());
            }
            if tz.trim().is_empty() {
                return bad("`timezone` is empty; omit it or give an IANA id".into());
            }
        }
        Ok(())
    }

    fn validate_window(&self) -> Result<(), SpecError> {
        let Some(window) = &self.window else {
            return Ok(());
        };
        let config = window.config();
        let bad = |m: String| Err(SpecError::Window(m));

        // Shape first: it is the same whatever the app.
        match (config.width, config.height) {
            (Some(w), Some(h)) if w > 0 && h > 0 => {}
            (None, None) => {}
            (Some(0), _) | (_, Some(0)) => return bad("width and height must be positive".into()),
            _ => return bad("width and height go together: give both or neither".into()),
        }
        match (config.x, config.y) {
            (Some(_), Some(_)) if config.width.is_none() => {
                return bad("x and y need width and height to be set too".into())
            }
            (Some(_), None) | (None, Some(_)) => {
                return bad("x and y go together: give both or neither".into())
            }
            _ => {}
        }

        let has_geometry = config.width.is_some();
        let has_title = config.title.is_some();
        match self.app.id() {
            "vision" => Ok(()),
            "web" => {
                if has_title {
                    return bad(
                        "a web flow has no window title: the flow's `url:` selects the page".into(),
                    );
                }
                if has_geometry {
                    return bad(
                        "a web flow sizes its page with `browser: viewport`, not `window:`".into(),
                    );
                }
                Ok(())
            }
            "api" => bad("an api flow has no window".into()),
            "sap" => {
                if has_title {
                    return bad("a sap flow attaches by `connection:`, not a window title".into());
                }
                if has_geometry {
                    return bad("window geometry is not implemented for sap".into());
                }
                Ok(())
            }
            // Windows-driven apps: the registry ids and the `app:` mapping.
            _ => {
                if has_title {
                    return bad(
                        "name the window with `app: {command, window_title}`; `window.title` \
                         is for vision flows, which attach to a window they did not launch"
                            .into(),
                    );
                }
                Ok(())
            }
        }
    }

    /// The reason to skip this flow, if its `skip_unless_env` gate is not
    /// satisfied — naming every missing/empty variable.
    pub fn skip_reason(&self) -> Option<String> {
        let missing: Vec<&str> = self
            .skip_unless_env
            .iter()
            .filter(|var| std::env::var(var.as_str()).map_or(true, |v| v.is_empty()))
            .map(String::as_str)
            .collect();
        (!missing.is_empty()).then(|| format!("required env not set: {}", missing.join(", ")))
    }
}

/// A step: a plain natural-language action, a UI assertion, or an
/// out-of-band business-data assertion (SQL / API) — the posted record is
/// often the truth an enterprise test must verify, not the pixels.
///
/// Serialize stays derived-untagged (the wire shape specs are written in);
/// Deserialize is manual so unknown or misspelled fields are PARSE ERRORS
/// that name the offending key. The untagged derive can't do that: it
/// would either silently drop unknown fields (a 0.2.1 `assert_api` with
/// `headers:` ran on 0.2.0 with the auth silently gone) or collapse every
/// mistake into "did not match any variant".
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum SpecStep {
    AssertSql {
        assert_sql: SqlAssertSpec,
    },
    AssertApi {
        assert_api: ApiAssertSpec,
    },
    AssertScreenshot {
        assert_screenshot: ScreenshotAssertSpec,
    },
    Assert {
        assert: String,
    },
    /// `app: agent`: send a user turn to the system under test.
    Prompt {
        prompt: String,
    },
    /// `app: agent`: assert a tool was called, in prose (see agent_steps).
    AssertToolCall {
        assert_tool_call: String,
    },
    /// `app: agent`: assert a tool was NEVER called anywhere in the run.
    AssertNoToolCall {
        assert_no_tool_call: String,
    },
    Plain(String),
}

impl SpecStep {
    const FORMS: &'static str = "a plain string, `assert: <text>`, \
         `assert_sql: {...}`, `assert_api: {...}`, `assert_screenshot: {...}`, \
         `prompt: <text>`, `assert_tool_call: <text>`, \
         `assert_no_tool_call: <text>`, or `foreach: {...}`";

    fn from_yaml(value: serde_yaml::Value) -> Result<Self, String> {
        use serde_yaml::Value;
        match value {
            Value::String(s) => Ok(SpecStep::Plain(s)),
            Value::Mapping(map) => {
                let keys: Vec<String> = map
                    .keys()
                    .map(|k| match k.as_str() {
                        Some(s) => s.to_string(),
                        None => format!("{k:?}"),
                    })
                    .collect();
                if map.len() != 1 {
                    return Err(format!(
                        "a step mapping must have exactly one key, got {}; \
                         recognized step forms are {}",
                        keys.iter()
                            .map(|k| format!("`{k}`"))
                            .collect::<Vec<_>>()
                            .join(", "),
                        Self::FORMS
                    ));
                }
                let (key, inner) = map.into_iter().next().expect("len checked above");
                match key.as_str() {
                    Some("assert") => match inner {
                        Value::String(s) => Ok(SpecStep::Assert { assert: s }),
                        _ => Err("`assert:` takes a string (the expectation text)".into()),
                    },
                    Some("prompt") => match inner {
                        Value::String(s) => Ok(SpecStep::Prompt { prompt: s }),
                        _ => Err("`prompt:` takes a string (the user turn)".into()),
                    },
                    Some("assert_tool_call") => match inner {
                        Value::String(s) => Ok(SpecStep::AssertToolCall {
                            assert_tool_call: s,
                        }),
                        _ => Err("`assert_tool_call:` takes a string (see the docs)".into()),
                    },
                    Some("assert_no_tool_call") => match inner {
                        Value::String(s) => Ok(SpecStep::AssertNoToolCall {
                            assert_no_tool_call: s,
                        }),
                        _ => Err("`assert_no_tool_call:` takes a string (a tool name)".into()),
                    },
                    Some("assert_sql") => serde_yaml::from_value(inner)
                        .map(|assert_sql| SpecStep::AssertSql { assert_sql })
                        .map_err(|e| format!("in `assert_sql` step: {e}")),
                    Some("assert_api") => serde_yaml::from_value(inner)
                        .map(|assert_api| SpecStep::AssertApi { assert_api })
                        .map_err(|e| format!("in `assert_api` step: {e}")),
                    Some("assert_screenshot") => serde_yaml::from_value(inner)
                        .map(|assert_screenshot| SpecStep::AssertScreenshot { assert_screenshot })
                        .map_err(|e| format!("in `assert_screenshot` step: {e}")),
                    // A foreach reaching typed parsing means it was not
                    // expanded — it is only valid as a direct entry in a
                    // spec's `steps:` (FlowSpec::parse expands it there).
                    Some("foreach") => {
                        Err("`foreach:` is only valid as a top-level entry in a spec's \
                         `steps:` list (nested foreach is not supported)"
                            .into())
                    }
                    _ => Err(format!(
                        "unknown step key `{}`; recognized step forms are {}",
                        keys[0],
                        Self::FORMS
                    )),
                }
            }
            other => Err(format!(
                "a step must be {}; got {}",
                Self::FORMS,
                yaml_kind(&other)
            )),
        }
    }
}

/// Parse a strict `X.Y.Z` version into a comparable triple. Deliberately
/// tiny (no semver dep): flowproof versions are plain triples.
fn parse_version_triple(v: &str) -> Result<(u64, u64, u64), String> {
    let parts: Vec<&str> = v.split('.').collect();
    let parse = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    match parts.as_slice() {
        [a, b, c] => match (parse(a), parse(b), parse(c)) {
            (Some(a), Some(b), Some(c)) => Ok((a, b, c)),
            _ => Err(format!("invalid version `{v}` (expected X.Y.Z)")),
        },
        _ => Err(format!("invalid version `{v}` (expected X.Y.Z)")),
    }
}

/// Human name for a YAML node kind, for error messages.
fn yaml_kind(value: &serde_yaml::Value) -> &'static str {
    use serde_yaml::Value;
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Sequence(_) => "a sequence",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}

impl<'de> serde::Deserialize<'de> for SpecStep {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Buffering through Value is safe: specs are always YAML.
        let value = serde_yaml::Value::deserialize(deserializer)?;
        SpecStep::from_yaml(value).map_err(serde::de::Error::custom)
    }
}

/// ```yaml
/// - assert_screenshot:
///     name: dashboard                  # baseline PNG name (no extension)
///     mask: ["css:.clock", "Sync"]     # selectors blanked before compare
///     threshold: 0.001                 # fraction of pixels allowed to differ
/// ```
/// Record mints (or refreshes) the masked baseline; replay captures with
/// the SAME masks and compares. Masks are the tool for timestamps,
/// avatars, and other legitimately-volatile regions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenshotAssertSpec {
    /// Baseline name — the file `<name>.png` in the trace's sibling
    /// baselines directory.
    pub name: String,
    /// Selectors (text anchor / `css:` / `id:`) whose element rects are
    /// blanked before compare, at record and replay alike.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask: Vec<String>,
    /// Fraction of pixels allowed to differ (default 0: exact match).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f64>,
}

/// ```yaml
/// - assert_sql:
///     connection: reporting            # env FLOWPROOF_SQL_REPORTING
///     query: SELECT count(*) FROM templates WHERE name = 'X'
///     equals: "2"                      # first column of first row, as text
/// ```
/// The connection NAME travels in the trace; the connection string only
/// ever lives in the environment. `query`/`equals` may carry `${VAR}` refs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SqlAssertSpec {
    pub connection: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equals: Option<String>,
    /// Auto-wait bound override (default 10s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

/// ```yaml
/// - assert_api:
///     request: POST ${DM_API}/connections/test
///     headers:                         # optional; values may be ${VAR} refs
///       Authorization: Bearer ${DM_SESSION_TOKEN}
///     body:                            # optional JSON (mapping or string);
///       provider: postgres             # ${VAR} refs resolve in string leaves
///     status: 200                      # optional; default = any 2xx
///     body_contains: TestTemplate      # optional
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiAssertSpec {
    /// `METHOD url` — the url may carry `${VAR}` refs (base URLs, tokens).
    pub request: String,
    /// Request headers (e.g. Authorization). Values may carry `${VAR}`
    /// refs — the trace stores the raw reference, never the token.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub headers: std::collections::BTreeMap<String, String>,
    /// Request body: any YAML (mapping/list/string), sent as JSON. `${VAR}`
    /// refs inside string values resolve at probe time. POST/PUT/PATCH only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_contains: Option<String>,
    /// Auto-wait bound override (default 10s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,
}

impl SpecStep {
    pub fn intent(&self) -> String {
        match self {
            SpecStep::Assert { assert } => assert.clone(),
            SpecStep::Plain(text) => text.clone(),
            SpecStep::AssertSql { assert_sql } => {
                format!("sql {}: {}", assert_sql.connection, assert_sql.query)
            }
            SpecStep::AssertApi { assert_api } => format!("api {}", assert_api.request),
            SpecStep::AssertScreenshot { assert_screenshot } => {
                format!("screenshot matches {}", assert_screenshot.name)
            }
            SpecStep::Prompt { prompt } => format!("prompt: {prompt}"),
            SpecStep::AssertToolCall { assert_tool_call } => {
                format!("assert_tool_call: {assert_tool_call}")
            }
            SpecStep::AssertNoToolCall {
                assert_no_tool_call,
            } => {
                format!("assert_no_tool_call: {assert_no_tool_call}")
            }
        }
    }
}

impl FlowSpec {
    pub fn parse(yaml: &str) -> Result<Self, SpecError> {
        // The Value round-trip costs line/column info in errors (names
        // still appear); only pay it when a foreach is actually present.
        let spec: FlowSpec = if yaml.contains("foreach") {
            let mut doc: serde_yaml::Value = serde_yaml::from_str(yaml)?;
            expand_foreach(&mut doc)?;
            serde_yaml::from_value(doc)?
        } else {
            serde_yaml::from_str(yaml)?
        };
        if spec.steps.is_empty() {
            return Err(SpecError::Empty);
        }
        spec.validate_window()?;
        spec.validate_agent()?;
        spec.validate_clock()?;
        Ok(spec)
    }

    pub fn load(path: &Path) -> Result<Self, SpecError> {
        let yaml = std::fs::read_to_string(path).map_err(|source| SpecError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::parse(&yaml)
    }
}

/// A `foreach:` entry in `steps:` — a values matrix over a step template,
/// removing the copy-paste class where one block repeats N times with a
/// single value changing:
///
/// ```yaml
/// steps:
///   - foreach:
///       values: [mysql, mssql, oracle]     # scalars, or mappings
///       steps:
///         - assert_api:
///             request: POST ${API}/connections/test
///             body: { type: "${each}" }
/// ```
///
/// Expansion happens at PARSE time, before typed deserialization and long
/// before any `${VAR}` env resolution — each iteration becomes ordinary
/// spec steps (`${each}` for scalar values, `${each.<key>}` for mapping
/// values), so recording, replay, traces, and step ids are untouched and
/// `${each}` can never collide with env secret resolution (leftovers are
/// rejected).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForeachSpec {
    values: Vec<serde_yaml::Value>,
    steps: Vec<serde_yaml::Value>,
}

/// Render a YAML scalar as the text `${each}` interpolates to.
fn scalar_text(value: &serde_yaml::Value) -> Option<String> {
    use serde_yaml::Value;
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// Substitute `${each}` / `${each.<key>}` tokens in one string for one
/// iteration value. Whole-string tokens are handled by the caller (node
/// replacement, preserving YAML types); this does textual interpolation.
fn substitute_each(text: &str, value: &serde_yaml::Value) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("${each") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find('}') else {
            return Err(format!("malformed `${{each` token in `{text}`"));
        };
        let token = &after[..=end];
        let key = &token[6..token.len() - 1]; // "" or ".key"
        let replacement = if key.is_empty() {
            scalar_text(value).ok_or_else(|| {
                format!(
                    "`${{each}}` needs a scalar iteration value, but got a mapping — \
                     use `${{each.<key>}}` (value: {value:?})"
                )
            })?
        } else if let Some(key) = key.strip_prefix('.') {
            let serde_yaml::Value::Mapping(map) = value else {
                return Err(format!(
                    "`{token}` needs a mapping iteration value, but got a scalar \
                     — use `${{each}}` (value: {value:?})"
                ));
            };
            let entry = map
                .get(serde_yaml::Value::String(key.to_string()))
                .ok_or_else(|| format!("`{token}`: iteration value has no key `{key}`"))?;
            scalar_text(entry).ok_or_else(|| format!("`{token}`: key `{key}` is not a scalar"))?
        } else {
            return Err(format!(
                "malformed token `{token}` (expected `${{each}}` or `${{each.<key>}}`)"
            ));
        };
        out.push_str(&replacement);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Deep-substitute one iteration value through a cloned template node.
/// A string that IS exactly one token is replaced by the value node itself
/// (numbers stay numbers — `status: ${each.status}` keeps its type).
fn substitute_node(
    node: &serde_yaml::Value,
    value: &serde_yaml::Value,
) -> Result<serde_yaml::Value, String> {
    use serde_yaml::Value;
    Ok(match node {
        Value::String(s) => {
            let whole_each = s == "${each}";
            let whole_key = s.starts_with("${each.")
                && s.ends_with('}')
                && s.matches("${").count() == 1
                && !s[7..s.len() - 1].contains('}');
            if whole_each {
                value.clone()
            } else if whole_key {
                let key = &s[7..s.len() - 1];
                let Value::Mapping(map) = value else {
                    return Err(format!(
                        "`{s}` needs a mapping iteration value, but got a scalar (value: {value:?})"
                    ));
                };
                map.get(Value::String(key.to_string()))
                    .cloned()
                    .ok_or_else(|| format!("`{s}`: iteration value has no key `{key}`"))?
            } else if s.contains("${each") {
                Value::String(substitute_each(s, value)?)
            } else {
                node.clone()
            }
        }
        Value::Sequence(items) => Value::Sequence(
            items
                .iter()
                .map(|i| substitute_node(i, value))
                .collect::<Result<_, _>>()?,
        ),
        Value::Mapping(map) => Value::Mapping(
            map.iter()
                .map(|(k, v)| Ok((k.clone(), substitute_node(v, value)?)))
                .collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

/// Does any string in this node still carry an (unsubstituted) `${each` token?
fn has_each_token(node: &serde_yaml::Value) -> bool {
    use serde_yaml::Value;
    match node {
        Value::String(s) => s.contains("${each"),
        Value::Sequence(items) => items.iter().any(has_each_token),
        Value::Mapping(map) => map.values().any(has_each_token),
        _ => false,
    }
}

/// Is this node a single-key mapping keyed `foreach`?
fn is_foreach_entry(node: &serde_yaml::Value) -> bool {
    matches!(node, serde_yaml::Value::Mapping(map)
        if map.len() == 1 && map.keys().next().and_then(|k| k.as_str()) == Some("foreach"))
}

/// Expand every `foreach:` entry in the document's `steps:` sequence into
/// flat, ordinary steps. Runs before typed deserialization.
fn expand_foreach(doc: &mut serde_yaml::Value) -> Result<(), SpecError> {
    use serde_yaml::Value;
    let Some(steps) = doc
        .as_mapping_mut()
        .and_then(|m| m.get_mut(Value::String("steps".into())))
        .and_then(|s| s.as_sequence_mut())
    else {
        return Ok(()); // No steps sequence: the typed parse reports it.
    };
    let mut expanded: Vec<Value> = Vec::with_capacity(steps.len());
    for entry in steps.drain(..) {
        if !is_foreach_entry(&entry) {
            expanded.push(entry);
            continue;
        }
        let Value::Mapping(map) = entry else {
            unreachable!("is_foreach_entry checked the shape")
        };
        let inner = map.into_iter().next().expect("single key checked").1;
        let spec: ForeachSpec = serde_yaml::from_value(inner)
            .map_err(|e| SpecError::Foreach(format!("in `foreach` step: {e}")))?;
        if spec.values.is_empty() {
            return Err(SpecError::Foreach("`values` must not be empty".into()));
        }
        if spec.steps.is_empty() {
            return Err(SpecError::Foreach(
                "`steps` (the template) must not be empty".into(),
            ));
        }
        if spec.steps.iter().any(is_foreach_entry) {
            return Err(SpecError::Foreach(
                "nested foreach is not supported — flatten the matrix into one \
                 `values` list"
                    .into(),
            ));
        }
        for value in &spec.values {
            for template in &spec.steps {
                let step = substitute_node(template, value)
                    .map_err(|e| SpecError::Foreach(format!("for value {value:?}: {e}")))?;
                if has_each_token(&step) {
                    return Err(SpecError::Foreach(format!(
                        "unsubstituted `${{each...}}` token remains after expansion \
                         for value {value:?} — check the token spelling"
                    )));
                }
                expanded.push(step);
            }
        }
    }
    *steps = expanded;
    Ok(())
}

/// Optional `suite.yaml` next to a directory of specs: the sequencing a
/// suite otherwise needs a hand-written harness for. `before_each` /
/// `after_each` shell commands run around every flow (the seed and cleanup
/// the eval's 912-line harness mostly existed to do); `env` is exported to
/// every flow and every hook; `order` pins spec order when it matters.
// PartialEq only: `browser.viewport.device_scale_factor` is an f64.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuiteManifest {
    /// Minimum flowproof version this suite's specs need (`X.Y.Z`). The
    /// CLI refuses to run/record when it is older — the guard against
    /// silently-weakened behavior when a spec uses vocabulary an older
    /// engine would have dropped (before 0.2.2, unknown spec fields were
    /// ignored instead of rejected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// Environment variables exported to every flow and hook. Values may
    /// carry `${VAR}` references, resolved from the ambient environment.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    /// Shell command whose stdout becomes env vars (KEY=VALUE lines) for
    /// every flow and hook — the bridge from an external data CLI (e.g.
    /// DataMaker minting a valid Material/Supplier/Plant from SAP) into a
    /// spec's `${VAR}` references. Runs once, before `env` is applied, so
    /// `env` can compose or override captured values. Fails closed: a
    /// non-zero exit or a malformed line aborts instead of running flows
    /// against half-seeded data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_from: Option<String>,
    /// Shell command run before each flow (seed). Runs via `sh -c` with the
    /// spec path in `FLOWPROOF_SPEC`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_each: Option<String>,
    /// Shell command run after each flow (cleanup), pass or fail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_each: Option<String>,
    /// Explicit spec order (paths relative to the suite dir). Specs not
    /// listed run after, in the default sorted order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub order: Vec<String>,
    /// Browser launch/emulation defaults for every flow in the suite
    /// (web): a flow's own `browser:` wins outright when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<flowproof_trace::format::BrowserSetup>,
}

impl SuiteManifest {
    /// Load `suite.yaml` from `dir` if present; `Ok(None)` when there is
    /// none (a suite without a manifest runs exactly as before).
    pub fn load_from_dir(dir: &Path) -> Result<Option<Self>, SpecError> {
        let path = dir.join("suite.yaml");
        if !path.exists() {
            return Ok(None);
        }
        let yaml = std::fs::read_to_string(&path).map_err(|source| SpecError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Some(serde_yaml::from_str(&yaml)?))
    }

    /// Find the suite manifest governing a single spec: walk up from the
    /// spec's directory to the nearest `suite.yaml` (git-style; nearest
    /// wins). This is how `record` and single-spec `run` share the suite's
    /// env and data — a flow behaves the same alone as inside its suite.
    /// Returns the manifest plus the directory it was found in.
    /// Enforce `min_version:` against the running engine version. Pass
    /// `env!("CARGO_PKG_VERSION")`; a parameter keeps this unit-testable.
    pub fn check_min_version(&self, current: &str) -> Result<(), String> {
        let Some(min) = &self.min_version else {
            return Ok(());
        };
        let min_v = parse_version_triple(min)?;
        let cur_v = parse_version_triple(current)?;
        if cur_v < min_v {
            return Err(format!(
                "this suite needs flowproof >= {min}, but this is flowproof {current} — \
                 upgrade flowproof (or lower the suite's min_version)"
            ));
        }
        Ok(())
    }

    pub fn discover(spec: &Path) -> Result<Option<(Self, std::path::PathBuf)>, SpecError> {
        // Canonicalize so a bare `calc.flow.yaml` walks up from the real
        // directory, not the empty relative parent.
        let spec = spec.canonicalize().unwrap_or_else(|_| spec.to_path_buf());
        let mut dir = spec.parent();
        while let Some(d) = dir {
            if let Some(manifest) = Self::load_from_dir(d)? {
                return Ok(Some((manifest, d.to_path_buf())));
            }
            dir = d.parent();
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALC_SPEC: &str = "\
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
";

    #[test]
    fn parses_the_calc_spec() {
        let spec = FlowSpec::parse(CALC_SPEC).expect("spec parses");
        assert_eq!(spec.name, "Add two numbers");
        assert_eq!(spec.app.id(), "calc");
        assert_eq!(spec.steps.len(), 5);
        assert_eq!(spec.steps[0], SpecStep::Plain("Type 5".into()));
        assert_eq!(
            spec.steps[4],
            SpecStep::Assert {
                assert: "display shows 8".into()
            }
        );
    }

    #[test]
    fn foreach_scalar_values_expand_flat_in_order() {
        let spec = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - Type start\n  - foreach:\n      values: [mysql, mssql, oracle]\n      steps:\n        - assert_api:\n            request: POST ${API}/connections/test\n            body:\n              type: \"${each}\"\n  - Type end\n",
        )
        .expect("parses");
        assert_eq!(spec.steps.len(), 5, "1 + 3 expanded + 1");
        assert_eq!(spec.steps[0], SpecStep::Plain("Type start".into()));
        for (i, ty) in ["mysql", "mssql", "oracle"].iter().enumerate() {
            let SpecStep::AssertApi { assert_api } = &spec.steps[i + 1] else {
                panic!("expected expanded assert_api at {}", i + 1);
            };
            assert_eq!(assert_api.body.as_ref().expect("body")["type"], *ty);
            // Non-token text is untouched.
            assert_eq!(assert_api.request, "POST ${API}/connections/test");
        }
        assert_eq!(spec.steps[4], SpecStep::Plain("Type end".into()));
    }

    #[test]
    fn foreach_mapping_values_substitute_keys_and_preserve_types() {
        let spec = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      values:\n        - {path: health, status: 200}\n        - {path: missing, status: 404}\n      steps:\n        - assert_api:\n            request: GET ${API}/${each.path}\n            status: ${each.status}\n",
        )
        .expect("parses");
        assert_eq!(spec.steps.len(), 2);
        let SpecStep::AssertApi { assert_api } = &spec.steps[1] else {
            panic!("expected assert_api");
        };
        assert_eq!(assert_api.request, "GET ${API}/missing");
        // Whole-string token: the NODE was replaced, number stays a number.
        assert_eq!(assert_api.status, Some(404));
    }

    #[test]
    fn foreach_rejects_nested_foreach_and_names_errors() {
        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      values: [a]\n      steps:\n        - foreach:\n            values: [b]\n            steps: [Type 1]\n",
        )
        .expect_err("nested must fail");
        assert!(err.to_string().contains("nested"), "{err}");

        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      values: [{a: 1}]\n      steps:\n        - Type ${each.missing}\n",
        )
        .expect_err("missing key must fail");
        assert!(err.to_string().contains("missing"), "{err}");

        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      values: []\n      steps: [Type 1]\n",
        )
        .expect_err("empty values must fail");
        assert!(err.to_string().contains("values"), "{err}");

        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      value: [a]\n      steps: [Type 1]\n",
        )
        .expect_err("typo'd foreach field must fail");
        assert!(err.to_string().contains("value"), "{err}");

        // ${each} against a mapping value is ambiguous — must be named.
        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - foreach:\n      values: [{a: 1}]\n      steps:\n        - Type prefix ${each}\n",
        )
        .expect_err("interpolating a mapping must fail");
        assert!(err.to_string().contains("${each.<key>}"), "{err}");
    }

    #[test]
    fn specs_without_foreach_are_untouched() {
        // The fast path (no Value round-trip) and the semantic no-op.
        let spec = FlowSpec::parse("name: x\napp: web\nsteps:\n  - Type hello\n").expect("parses");
        assert_eq!(spec.steps.len(), 1);
    }

    #[test]
    fn skip_unless_env_gates_on_unset_and_empty() {
        let spec: FlowSpec = FlowSpec::parse(
            "name: x\napp: web\nskip_unless_env: [SUE_FLAG_A, SUE_FLAG_B]\nsteps:\n  - Type 1\n",
        )
        .expect("parses");
        std::env::remove_var("SUE_FLAG_A");
        std::env::set_var("SUE_FLAG_B", "");
        let reason = spec.skip_reason().expect("both missing/empty");
        assert!(
            reason.contains("SUE_FLAG_A") && reason.contains("SUE_FLAG_B"),
            "names all missing vars: {reason}"
        );
        std::env::set_var("SUE_FLAG_A", "1");
        let reason = spec.skip_reason().expect("one still empty");
        assert!(!reason.contains("SUE_FLAG_A") && reason.contains("SUE_FLAG_B"));
        std::env::set_var("SUE_FLAG_B", "yes");
        assert!(spec.skip_reason().is_none(), "satisfied gate runs");
        std::env::remove_var("SUE_FLAG_A");
        std::env::remove_var("SUE_FLAG_B");
    }

    #[test]
    fn unknown_top_level_field_is_a_named_parse_error() {
        let err = FlowSpec::parse("name: x\napp: web\nurll: http://x\nsteps:\n  - Type 1\n")
            .expect_err("typo'd field must fail");
        let msg = err.to_string();
        assert!(msg.contains("urll"), "names the field: {msg}");
    }

    #[test]
    fn browser_block_parses_and_rejects_typos() {
        let spec = FlowSpec::parse(
            "name: x\napp: web\nurl: http://x\nbrowser:\n  viewport:\n    width: 390\n    height: 844\n    mobile: true\n  user_agent: probe\n  args: [\"--lang=fr-FR\"]\nsteps:\n  - Type 1\n",
        )
        .expect("browser block parses");
        let browser = spec.browser.expect("browser present");
        assert_eq!(browser.viewport.as_ref().map(|v| v.width), Some(390));
        assert_eq!(browser.args, vec!["--lang=fr-FR"]);

        // A dropped emulation field would change what the flow tests —
        // typos inside browser: are parse errors naming the field.
        let err = FlowSpec::parse(
            "name: x\napp: web\nurl: http://x\nbrowser:\n  viewport:\n    width: 390\n    height: 844\n    mobil: true\nsteps:\n  - Type 1\n",
        )
        .expect_err("typo'd viewport field must fail");
        assert!(err.to_string().contains("mobil"), "names the field: {err}");

        // Suite manifests accept the same block.
        let manifest: SuiteManifest =
            serde_yaml::from_str("browser:\n  user_agent: probe\n").expect("manifest parses");
        assert_eq!(
            manifest.browser.and_then(|b| b.user_agent).as_deref(),
            Some("probe")
        );
    }

    #[test]
    fn typoed_assert_api_field_error_names_field_and_step_kind() {
        let err = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - assert_api:\n      request: GET http://x\n      statuss: 200\n",
        )
        .expect_err("typo'd inner field must fail");
        let msg = err.to_string();
        assert!(msg.contains("statuss"), "names the field: {msg}");
        assert!(msg.contains("assert_api"), "names the step kind: {msg}");
    }

    #[test]
    fn unknown_step_key_error_names_key_and_lists_forms() {
        let err = FlowSpec::parse("name: x\napp: web\nsteps:\n  - assert_apy:\n      request: x\n")
            .expect_err("unknown step key must fail");
        let msg = err.to_string();
        assert!(msg.contains("assert_apy"), "names the key: {msg}");
        assert!(msg.contains("assert_api"), "lists recognized forms: {msg}");
    }

    #[test]
    fn multi_key_step_mapping_names_all_keys() {
        let err = FlowSpec::parse(
            "name: x\napp: web\nsteps:\n  - assert: page shows X\n    timeout: 3\n",
        )
        .expect_err("two-key step mapping must fail");
        let msg = err.to_string();
        assert!(msg.contains("exactly one key"), "{msg}");
        assert!(
            msg.contains("assert") && msg.contains("timeout"),
            "names both keys: {msg}"
        );
    }

    #[test]
    fn non_string_non_mapping_step_is_rejected() {
        let err = FlowSpec::parse("name: x\napp: web\nsteps:\n  - 42\n")
            .expect_err("numeric step must fail");
        assert!(err.to_string().contains("a number"), "{err}");
    }

    #[test]
    fn spec_step_serializes_and_reparses_identically() {
        // Serialize stays derived-untagged; manual Deserialize must accept
        // exactly that wire shape.
        let spec = FlowSpec::parse(
            "name: x\napp: api\nsteps:\n  - Type 1\n  - assert: page shows X\n  - assert_api:\n      request: GET http://x\n      status: 200\n",
        )
        .expect("parses");
        let yaml = serde_yaml::to_string(&spec.steps).expect("serializes");
        let back: Vec<SpecStep> = serde_yaml::from_str(&yaml).expect("round-trips");
        assert_eq!(back, spec.steps);
    }

    #[test]
    fn version_triples_parse_strictly() {
        assert_eq!(parse_version_triple("0.2.1").expect("ok"), (0, 2, 1));
        assert_eq!(parse_version_triple("10.20.30").expect("ok"), (10, 20, 30));
        for bad in ["1.2", "v1.2.3", "1.2.3.4", "1.x.3", "", "1..3"] {
            assert!(parse_version_triple(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn min_version_gate_compares_triples() {
        let manifest: SuiteManifest =
            serde_yaml::from_str("min_version: \"0.3.0\"\n").expect("parses");
        manifest.check_min_version("0.3.0").expect("equal passes");
        manifest.check_min_version("0.10.0").expect("newer passes");
        let err = manifest
            .check_min_version("0.2.1")
            .expect_err("older engine must be refused");
        assert!(err.contains("0.3.0") && err.contains("0.2.1"), "{err}");
        // No min_version = no gate.
        assert!(SuiteManifest::default().check_min_version("0.0.1").is_ok());
    }

    #[test]
    fn unknown_suite_manifest_field_is_rejected() {
        let err = serde_yaml::from_str::<SuiteManifest>("env_form: echo A=1\n")
            .expect_err("typo'd manifest key must fail");
        assert!(err.to_string().contains("env_form"), "{err}");
    }

    #[test]
    fn parses_a_suite_manifest() {
        let yaml = "\
env:
  DM_BASE_URL: http://localhost:3000
before_each: pnpm seed
after_each: pnpm cleanup
order:
  - smoke/login.flow.yaml
  - templates/list.flow.yaml
";
        let manifest: SuiteManifest = serde_yaml::from_str(yaml).expect("manifest parses");
        assert_eq!(
            manifest.env.get("DM_BASE_URL").map(String::as_str),
            Some("http://localhost:3000")
        );
        assert_eq!(manifest.before_each.as_deref(), Some("pnpm seed"));
        assert_eq!(manifest.after_each.as_deref(), Some("pnpm cleanup"));
        assert_eq!(manifest.order.len(), 2);
    }

    #[test]
    fn empty_manifest_fields_are_all_optional() {
        // A suite.yaml with just env, or an empty one, is valid.
        let manifest: SuiteManifest = serde_yaml::from_str("env: {}\n").expect("parses");
        assert!(manifest.before_each.is_none() && manifest.order.is_empty());
        assert!(manifest.env_from.is_none());
    }

    #[test]
    fn env_from_parses_and_is_optional() {
        let manifest: SuiteManifest =
            serde_yaml::from_str("env_from: datamaker sap pick --format env\n").expect("parses");
        assert_eq!(
            manifest.env_from.as_deref(),
            Some("datamaker sap pick --format env")
        );
    }

    #[test]
    fn discover_finds_the_nearest_manifest_walking_up() {
        let root = std::env::temp_dir().join("flowproof-suite-discover");
        let nested = root.join("smoke").join("deep");
        std::fs::create_dir_all(&nested).expect("dirs");
        std::fs::write(root.join("suite.yaml"), "env: {A: '1'}\n").expect("outer manifest");
        let spec = nested.join("x.flow.yaml");
        std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");

        let (found, dir) = SuiteManifest::discover(&spec)
            .expect("no error")
            .expect("manifest found from nested spec");
        assert_eq!(found.env.get("A").map(String::as_str), Some("1"));
        assert!(dir.ends_with("flowproof-suite-discover"));

        // Nearest wins: a manifest closer to the spec shadows the outer one.
        std::fs::write(nested.join("suite.yaml"), "env: {A: '2'}\n").expect("inner manifest");
        let (found, _) = SuiteManifest::discover(&spec)
            .expect("no error")
            .expect("manifest found");
        assert_eq!(found.env.get("A").map(String::as_str), Some("2"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn rejects_empty_steps() {
        let err = FlowSpec::parse("name: x\napp: calc\nsteps: []\n").expect_err("must fail");
        assert!(matches!(err, SpecError::Empty));
    }
}

#[cfg(test)]
mod app_and_window_tests {
    use super::*;

    fn spec(yaml: &str) -> Result<FlowSpec, SpecError> {
        FlowSpec::parse(yaml)
    }

    /// The scalar form is what every existing spec uses, and its meaning is
    /// unchanged: this is the backward-compatibility guarantee.
    #[test]
    fn a_scalar_app_still_parses_and_means_the_same() {
        let s = spec("name: n\napp: calc\nsteps:\n  - Type 5\n").expect("parses");
        assert_eq!(s.app.id(), "calc");
        assert!(s.app.launch_parts().is_none());
    }

    /// #66: drive an arbitrary Windows program. Both fields keep their
    /// `${VAR}` refs RAW so they resolve again at every replay.
    #[test]
    fn the_mapping_form_reports_the_windows_id_and_keeps_refs_raw() {
        let s = spec(
            "name: n\napp:\n  command: notepad.exe\n  window_title: ${APP_WINDOW}\nsteps:\n  - Type hi\n",
        )
        .expect("parses");
        assert_eq!(s.app.id(), "windows");
        let (command, title) = s.app.launch_parts().expect("mapping form");
        assert_eq!(command, "notepad.exe");
        assert_eq!(
            title, "${APP_WINDOW}",
            "the reference must survive unresolved"
        );
    }

    /// A bare `window:` string stays the vision shorthand it has always been.
    #[test]
    fn a_bare_window_string_is_vision_shorthand() {
        let s = spec("name: n\napp: vision\nwindow: Citrix Receiver\nsteps:\n  - Type 5\n")
            .expect("parses");
        assert_eq!(
            s.window.as_ref().and_then(|w| w.title()),
            Some("Citrix Receiver")
        );
    }

    /// Vision is the one app kind that can name a window AND pin its shape,
    /// which is the case that killed the "geometry lives under its own key"
    /// option: OCR baselines depend on both.
    #[test]
    fn vision_may_pin_title_and_geometry_together() {
        let s = spec(
            "name: n\napp: vision\nwindow:\n  title: Citrix\n  width: 1280\n  height: 720\nsteps:\n  - Type 5\n",
        )
        .expect("parses");
        let c = s.window.expect("window").config();
        assert_eq!(c.title.as_deref(), Some("Citrix"));
        assert_eq!((c.width, c.height), (Some(1280), Some(720)));
    }

    #[test]
    fn geometry_shape_rules_are_enforced() {
        let half = spec("name: n\napp: calc\nwindow:\n  width: 800\nsteps:\n  - Type 5\n")
            .expect_err("width without height");
        assert!(half.to_string().contains("go together"), "{half}");

        let zero =
            spec("name: n\napp: calc\nwindow:\n  width: 0\n  height: 600\nsteps:\n  - Type 5\n")
                .expect_err("zero size");
        assert!(zero.to_string().contains("positive"), "{zero}");

        let floating = spec("name: n\napp: calc\nwindow:\n  x: 10\n  y: 10\nsteps:\n  - Type 5\n")
            .expect_err("position without size");
        assert!(
            floating.to_string().contains("width and height"),
            "{floating}"
        );
    }

    /// Each app kind has exactly one spelling for naming a window, and the
    /// error names the right one rather than just refusing.
    #[test]
    fn a_window_title_on_a_windows_flow_names_the_right_spelling() {
        let err = spec("name: n\napp: notepad\nwindow:\n  title: Untitled\nsteps:\n  - Type 5\n")
            .expect_err("title is vision-only");
        let m = err.to_string();
        assert!(m.contains("app: {command, window_title}"), "{m}");
        assert!(m.contains("vision"), "the message explains why: {m}");
    }

    #[test]
    fn web_and_api_and_sap_reject_window_with_a_reason() {
        let web = spec(
            "name: n\napp: web\nurl: x\nwindow:\n  width: 800\n  height: 600\nsteps:\n  - Type 5\n",
        )
        .expect_err("web sizes with browser:");
        assert!(web.to_string().contains("browser: viewport"), "{web}");

        let api =
            spec("name: n\napp: api\nwindow:\n  width: 800\n  height: 600\nsteps:\n  - Type 5\n")
                .expect_err("api has no window");
        assert!(api.to_string().contains("no window"), "{api}");

        let sap =
            spec("name: n\napp: sap\nwindow:\n  width: 800\n  height: 600\nsteps:\n  - Type 5\n")
                .expect_err("sap geometry unimplemented");
        assert!(sap.to_string().contains("not implemented for sap"), "{sap}");
    }

    // ---- app: agent surface ----

    const AGENT_SPEC: &str = r#"
name: Booking assistant
app: agent
agent:
  command: python3 assistant.py
  env:
    ANTHROPIC_API_KEY: ${SECRET}
tools:
  - name: search_flights
    result: { flights: [KQ311] }
  - name: create_booking
    result: { booking: B-1042 }
steps:
  - prompt: Book me a flight to Nairobi
  - assert_tool_call: search_flights where destination contains NBO
  - assert_no_tool_call: charge_card
  - assert: reply contains booked
"#;

    #[test]
    fn a_full_agent_flow_parses() {
        let flow = spec(AGENT_SPEC).expect("parses");
        assert_eq!(flow.app.id(), "agent");
        let agent = flow.agent.expect("agent block");
        assert_eq!(agent.command.as_deref(), Some("python3 assistant.py"));
        assert_eq!(
            agent.env.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("${SECRET}")
        );
        assert_eq!(flow.tools.len(), 2);
        assert_eq!(flow.tools[0].name, "search_flights");
        assert!(matches!(flow.steps[0], SpecStep::Prompt { .. }));
        assert!(matches!(flow.steps[1], SpecStep::AssertToolCall { .. }));
        assert!(matches!(flow.steps[2], SpecStep::AssertNoToolCall { .. }));
    }

    /// The #66/#67 lesson in spec form: the surface belongs to ONE app and
    /// every wrong combination is a parse error that names the mismatch.
    #[test]
    fn the_agent_surface_is_rejected_off_agent_flows() {
        // agent: block on a non-agent app.
        let err =
            spec("name: n\napp: web\nurl: http://x\nagent:\n  command: x\nsteps:\n  - Go to /\n")
                .expect_err("agent block on web");
        assert!(err.to_string().contains("app: web"), "{err}");

        // tools: on a non-agent app.
        let err = spec("name: n\napp: calc\ntools:\n  - name: t\nsteps:\n  - Type 5\n")
            .expect_err("tools on calc");
        assert!(err.to_string().contains("tools:"), "{err}");

        // an agent STEP on a non-agent app.
        let err = spec("name: n\napp: calc\nsteps:\n  - prompt: hi\n").expect_err("prompt on calc");
        assert!(err.to_string().contains("agent step"), "{err}");

        // strict: off an agent flow.
        let err = spec("name: n\napp: calc\nstrict: true\nsteps:\n  - Type 5\n")
            .expect_err("strict on calc");
        assert!(err.to_string().contains("strict:"), "{err}");
    }

    #[test]
    fn an_agent_flow_needs_a_command_and_a_prompt() {
        let err =
            spec("name: n\napp: agent\nsteps:\n  - prompt: hi\n").expect_err("no agent block");
        assert!(err.to_string().contains("agent:"), "{err}");

        let err = spec("name: n\napp: agent\nagent:\n  command: '   '\nsteps:\n  - prompt: hi\n")
            .expect_err("blank command");
        assert!(err.to_string().contains("blank"), "{err}");

        let err = spec(
            "name: n\napp: agent\nagent:\n  command: x\nsteps:\n  - assert: reply contains hi\n",
        )
        .expect_err("no prompt step");
        assert!(err.to_string().contains("prompt:"), "{err}");
    }

    /// A url: driver parses and keeps its `${VAR}` header refs raw.
    #[test]
    fn a_url_driven_agent_flow_parses() {
        let flow = spec(
            "name: n\napp: agent\nagent:\n  url: http://localhost:8080/run\n  proxy_port: 8123\n  headers:\n    Authorization: Bearer ${TOKEN}\nsteps:\n  - prompt: hi\n",
        )
        .expect("parses");
        let agent = flow.agent.expect("agent block");
        assert_eq!(agent.command, None);
        assert_eq!(agent.url.as_deref(), Some("http://localhost:8080/run"));
        assert_eq!(agent.proxy_port, Some(8123));
        assert_eq!(
            agent.headers.get("Authorization").map(String::as_str),
            Some("Bearer ${TOKEN}"),
            "the reference must survive unresolved"
        );
    }

    /// D1: every cross-field rule fails with its named error.
    #[test]
    fn the_command_url_choice_is_enforced_with_named_errors() {
        // Neither: the grown "needs a command ... or a url:" message.
        let err = spec("name: n\napp: agent\nagent: {}\nsteps:\n  - prompt: hi\n")
            .expect_err("neither command nor url");
        assert!(err.to_string().contains("command"), "{err}");
        assert!(err.to_string().contains("url:"), "{err}");

        // Both: two systems under test.
        let err = spec(
            "name: n\napp: agent\nagent:\n  command: x\n  url: http://y\n  proxy_port: 9\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("both command and url");
        assert!(err.to_string().contains("two systems under test"), "{err}");

        // url without proxy_port.
        let err = spec("name: n\napp: agent\nagent:\n  url: http://y\nsteps:\n  - prompt: hi\n")
            .expect_err("url without proxy_port");
        assert!(err.to_string().contains("needs a proxy_port"), "{err}");

        // env with url.
        let err = spec(
            "name: n\napp: agent\nagent:\n  url: http://y\n  proxy_port: 9\n  env:\n    K: v\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("env with url");
        assert!(err.to_string().contains("agent.env"), "{err}");

        // headers with command.
        let err = spec(
            "name: n\napp: agent\nagent:\n  command: x\n  headers:\n    K: v\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("headers with command");
        assert!(err.to_string().contains("agent.headers"), "{err}");

        // proxy_port with command.
        let err = spec(
            "name: n\napp: agent\nagent:\n  command: x\n  proxy_port: 9\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("proxy_port with command");
        assert!(err.to_string().contains("proxy_port"), "{err}");

        // blank/whitespace url is rejected exactly like a blank command.
        let err = spec(
            "name: n\napp: agent\nagent:\n  url: '   '\n  proxy_port: 9\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("blank url");
        assert!(err.to_string().contains("blank"), "{err}");
    }

    #[test]
    fn a_duplicate_tool_mock_is_rejected() {
        let err = spec(
            "name: n\napp: agent\nagent:\n  command: x\ntools:\n  - name: t\n  - name: t\nsteps:\n  - prompt: hi\n",
        )
        .expect_err("dup tool");
        assert!(err.to_string().contains("mocked twice"), "{err}");
    }

    /// A tool mock's result defaults to an empty object, for a tool whose
    /// call matters but whose output the agent ignores.
    #[test]
    fn a_tool_result_is_optional() {
        let flow = spec("name: n\napp: agent\nagent:\n  command: x\ntools:\n  - name: log_it\nsteps:\n  - prompt: hi\n")
            .expect("parses");
        assert_eq!(flow.tools[0].result, serde_json::json!(null));
    }

    // ---- browser.clock (GAP-P) ----

    const CLOCK_SPEC: &str = r#"
name: pinned
app: web
url: http://x
browser:
  clock:
    at: "2026-01-15T09:00:00Z"
    timezone: "Europe/Berlin"
steps:
  - assert: page shows Dashboard
"#;

    #[test]
    fn a_pinned_clock_parses() {
        let flow = spec(CLOCK_SPEC).expect("parses");
        let clock = flow.browser.expect("browser").clock.expect("clock");
        assert_eq!(clock.at, "2026-01-15T09:00:00Z");
        assert_eq!(clock.timezone.as_deref(), Some("Europe/Berlin"));
    }

    #[test]
    fn clock_is_web_only() {
        let err = spec(
            "name: n\napp: calc\nbrowser:\n  clock:\n    at: \"2026-01-15T09:00:00Z\"\nsteps:\n  - Type 5\n",
        )
        .expect_err("clock on calc");
        assert!(err.to_string().contains("web-only"), "{err}");
    }

    #[test]
    fn clock_at_must_be_a_real_instant_and_a_literal() {
        // A typo that would silently disable the pin is a parse error.
        let err = spec(
            "name: n\napp: web\nurl: http://x\nbrowser:\n  clock:\n    at: \"last tuesday\"\nsteps:\n  - assert: page shows x\n",
        )
        .expect_err("bad at");
        assert!(err.to_string().contains("RFC 3339"), "{err}");

        // A ${VAR} would let record and replay pin different times.
        let err = spec(
            "name: n\napp: web\nurl: http://x\nbrowser:\n  clock:\n    at: ${WHEN}\nsteps:\n  - assert: page shows x\n",
        )
        .expect_err("var at");
        assert!(err.to_string().contains("literal"), "{err}");
    }

    #[test]
    fn a_clock_with_no_at_is_rejected() {
        let err = spec(
            "name: n\napp: web\nurl: http://x\nbrowser:\n  clock:\n    timezone: UTC\nsteps:\n  - assert: page shows x\n",
        )
        .expect_err("no at");
        // `at` is required by the type, so this fails at deserialization.
        assert!(
            err.to_string().contains("at") || err.to_string().contains("missing"),
            "{err}"
        );
    }
}
