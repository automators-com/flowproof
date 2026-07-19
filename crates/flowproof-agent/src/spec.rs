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

/// A step is either a plain natural-language action or an assertion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpecStep {
    Assert { assert: String },
    Plain(String),
}

impl SpecStep {
    pub fn intent(&self) -> &str {
        match self {
            SpecStep::Assert { assert } => assert,
            SpecStep::Plain(text) => text,
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
