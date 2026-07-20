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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub steps: Vec<SpecStep>,
}

impl FlowSpec {
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
    AssertSql { assert_sql: SqlAssertSpec },
    AssertApi { assert_api: ApiAssertSpec },
    Assert { assert: String },
    Plain(String),
}

impl SpecStep {
    const FORMS: &'static str = "a plain string, `assert: <text>`, \
         `assert_sql: {...}`, `assert_api: {...}`, or `foreach: {...}`";

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
                    Some("assert_sql") => serde_yaml::from_value(inner)
                        .map(|assert_sql| SpecStep::AssertSql { assert_sql })
                        .map_err(|e| format!("in `assert_sql` step: {e}")),
                    Some("assert_api") => serde_yaml::from_value(inner)
                        .map(|assert_api| SpecStep::AssertApi { assert_api })
                        .map_err(|e| format!("in `assert_api` step: {e}")),
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
