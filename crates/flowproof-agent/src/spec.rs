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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowSpec {
    pub name: String,
    /// App id resolved via `flowproof_driver::resolve_app` (e.g. `calc`),
    /// or `web` for browser flows.
    pub app: String,
    /// For `app: web`: the URL to open (relative paths become `file://`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// For `app: sap`: the SAP Logon connection description to open when no
    /// session is already running (e.g. `S/4HANA Development`). Omitted =
    /// attach to whatever logged-in SAP GUI session exists. May carry
    /// `${VAR}` references, resolved at launch time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection: Option<String>,
    /// For `app: vision`: the title (substring, case-insensitive) of the
    /// window to drive as pixels — the Citrix/RDP client, or any window.
    /// May carry `${VAR}` references, resolved at launch time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// Regions to mask in every persisted frame (password fields are always
    /// masked, with or without rules here). Copied into the trace header at
    /// record time so replays redact identically without the spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redact: Vec<flowproof_driver::RedactionRule>,
    /// Session state (cookies, localStorage) applied before the page loads —
    /// how authenticated flows start without a login walk. Values may be
    /// `${VAR}` references, resolved at apply time and never stored. Copied
    /// into the trace header so replays authenticate identically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<flowproof_trace::format::SessionSetup>,
    pub steps: Vec<SpecStep>,
}

/// A step: a plain natural-language action, a UI assertion, or an
/// out-of-band business-data assertion (SQL / API) — the posted record is
/// often the truth an enterprise test must verify, not the pixels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpecStep {
    AssertSql { assert_sql: SqlAssertSpec },
    AssertApi { assert_api: ApiAssertSpec },
    Assert { assert: String },
    Plain(String),
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
///     request: GET ${DM_API}/templates
///     status: 200                      # optional; default = any 2xx
///     body_contains: TestTemplate      # optional
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiAssertSpec {
    /// `METHOD url` — the url may carry `${VAR}` refs (base URLs, tokens).
    pub request: String,
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
        }
    }
}

impl FlowSpec {
    pub fn parse(yaml: &str) -> Result<Self, SpecError> {
        let spec: FlowSpec = serde_yaml::from_str(yaml)?;
        if spec.steps.is_empty() {
            return Err(SpecError::Empty);
        }
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
        assert_eq!(spec.app, "calc");
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
    fn rejects_empty_steps() {
        let err = FlowSpec::parse("name: x\napp: calc\nsteps: []\n").expect_err("must fail");
        assert!(matches!(err, SpecError::Empty));
    }
}
