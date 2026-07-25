//! Out-of-band probes: assert business-data correctness directly against
//! the database or an API, not just the pixels. In enterprise E2E the
//! posted record is often the truth a test must verify — the fourth
//! provenance next to uia, sap-com, and vision.
//!
//! Credentials NEVER travel in traces: a probe names a connection, and the
//! connection string / base configuration resolves from the local
//! environment at run time (`FLOWPROOF_SQL_<NAME>`).

use crate::DriverError;

/// One out-of-band check, with every `${VAR}` reference already resolved
/// by the caller (the engine owns secret indirection; this module never
/// sees a raw reference).
#[derive(Debug, Clone, PartialEq)]
pub enum OobProbe {
    /// Run `query` on the named connection; the FIRST COLUMN of the FIRST
    /// ROW, rendered as text, must equal `equals` (when set — otherwise the
    /// query merely has to succeed and return at least one row).
    Sql {
        /// Name resolved via env `FLOWPROOF_SQL_<NAME>` (uppercased,
        /// non-alphanumerics become `_`) holding a postgres connection
        /// string. The trace stores only the name.
        connection: String,
        query: String,
        equals: Option<String>,
    },
    /// Fire an HTTP request; the response must match `status` (default:
    /// any 2xx) and, when set, its body must contain `body_contains`.
    Api {
        method: String,
        url: String,
        body: Option<serde_json::Value>,
        /// Request headers, already resolved (probes never see `${VAR}`
        /// refs — the engine owns secret indirection).
        headers: std::collections::BTreeMap<String, String>,
        status: Option<u16>,
        body_contains: Option<String>,
        /// A dotted path into the JSON response body: each segment is a plain
        /// object key or a decimal array index (e.g. `results.0.balance`).
        /// When set, the body must parse as JSON and the path must resolve to
        /// a scalar leaf. Pairs with `body_contains`; both may be set.
        body_json: Option<String>,
        /// When set (only meaningful with `body_json`), the resolved leaf
        /// must equal this value. Restricted to string/number/bool; any
        /// `${VAR}` reference in a string has already been resolved by the
        /// caller (the trace keeps the raw ref, never the value).
        equals: Option<serde_json::Value>,
    },
}

/// Environment variable name for a SQL connection: `FLOWPROOF_SQL_<NAME>`.
pub fn sql_connection_var(name: &str) -> String {
    let suffix: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("FLOWPROOF_SQL_{suffix}")
}

/// One evaluation attempt. `Ok(Ok(body))`: the expectation holds; `body` is
/// the API response text for an `Api` probe (the corpus a secret-leak scan
/// reads) and `None` for a `Sql` probe. `Ok(Err(reason))`: it does not YET
/// hold (the caller's auto-wait loop may poll again: the row may still be
/// committing, the API converging). `Err(_)`: a configuration error polling
/// cannot fix (missing connection env), failed immediately and loudly.
pub fn check(probe: &OobProbe) -> Result<Result<Option<String>, String>, DriverError> {
    match probe {
        OobProbe::Sql {
            connection,
            query,
            equals,
        } => {
            let var = sql_connection_var(connection);
            let Ok(conn_string) = std::env::var(&var) else {
                return Err(DriverError::Uia(format!(
                    "sql connection '{connection}' is not configured: set {var} \
                     to a postgres connection string"
                )));
            };
            let mut client = match postgres::Client::connect(&conn_string, postgres::NoTls) {
                Ok(client) => client,
                Err(e) => return Ok(Err(format!("connecting to '{connection}': {e}"))),
            };
            let rows = match client.query(query.as_str(), &[]) {
                Ok(rows) => rows,
                Err(e) => return Ok(Err(format!("query failed: {e}"))),
            };
            let Some(row) = rows.first() else {
                return Ok(Err("query returned no rows".into()));
            };
            let Some(expected) = equals else {
                return Ok(Ok(None));
            };
            let actual = first_column_as_text(row);
            if actual.as_deref() == Some(expected.as_str()) {
                Ok(Ok(None))
            } else {
                Ok(Err(format!(
                    "expected first column '{expected}', got '{}'",
                    actual.as_deref().unwrap_or("<unreadable column>")
                )))
            }
        }
        OobProbe::Api {
            method,
            url,
            body,
            headers,
            status,
            body_contains,
            body_json,
            equals,
        } => {
            // Non-2xx statuses are data here, not transport errors — the
            // expectation decides what counts as passing.
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .proxy(ureq::Proxy::try_from_env())
                .http_status_as_error(false)
                .build()
                .into();
            let sent = match method.to_ascii_uppercase().as_str() {
                m @ ("GET" | "DELETE" | "HEAD") => {
                    let mut builder = match m {
                        "GET" => agent.get(url.as_str()),
                        "DELETE" => agent.delete(url.as_str()),
                        _ => agent.head(url.as_str()),
                    };
                    for (name, value) in headers {
                        builder = builder.header(name.as_str(), value.as_str());
                    }
                    builder.call()
                }
                m @ ("POST" | "PUT" | "PATCH") => {
                    let mut builder = match m {
                        "POST" => agent.post(url.as_str()),
                        "PUT" => agent.put(url.as_str()),
                        _ => agent.patch(url.as_str()),
                    };
                    for (name, value) in headers {
                        builder = builder.header(name.as_str(), value.as_str());
                    }
                    match body {
                        Some(json) => {
                            // Auto json content-type only when the user
                            // didn't set one — their header wins.
                            if !headers
                                .keys()
                                .any(|k| k.eq_ignore_ascii_case("content-type"))
                            {
                                builder = builder.header("content-type", "application/json");
                            }
                            builder.send(json.to_string())
                        }
                        None => builder.send(""),
                    }
                }
                other => {
                    return Err(DriverError::Uia(format!(
                        "unsupported http method '{other}' in api assertion"
                    )))
                }
            };
            let mut response = match sent {
                Ok(response) => response,
                Err(e) => return Ok(Err(format!("request failed: {e}"))),
            };
            let code = response.status().as_u16();
            let text = response.body_mut().read_to_string().unwrap_or_default();
            if let Err(reason) = check_status(*status, code) {
                return Ok(Err(reason));
            }
            if let Some(needle) = body_contains {
                if !text.contains(needle.as_str()) {
                    return Ok(Err(format!(
                        "response body does not contain '{needle}' (status {code})"
                    )));
                }
            }
            if let Some(path) = body_json {
                let value: serde_json::Value = match serde_json::from_str(&text) {
                    Ok(value) => value,
                    Err(_) => {
                        return Ok(Err(format!(
                            "response body is not valid JSON (status {code})"
                        )))
                    }
                };
                let leaf = match resolve_json_path(&value, path) {
                    Ok(leaf) => leaf,
                    Err(segment) => {
                        return Ok(Err(format!(
                            "path '{path}' stops at segment '{segment}' (status {code})"
                        )))
                    }
                };
                if leaf.is_object() || leaf.is_array() {
                    return Ok(Err(
                        "path resolves to a non-scalar; assert a leaf value".to_string()
                    ));
                }
                // `body_json` with no `equals` is an existence check: reaching
                // a scalar leaf is the whole assertion (mirrors assert_sql,
                // where an omitted `equals` means a row merely has to exist).
                if let Some(expected) = equals {
                    if let Err(reason) = compare_json_leaf(leaf, expected, code) {
                        return Ok(Err(reason));
                    }
                }
            }
            // The response body IS the corpus element an `assert_no_secret_leak`
            // scan reads: return it so the caller can scan it in memory. It is
            // never persisted: the trace keeps only the request and the raw
            // `${VAR}` expectations.
            Ok(Ok(Some(text)))
        }
    }
}

fn check_status(expected: Option<u16>, actual: u16) -> Result<(), String> {
    match expected {
        Some(want) if actual == want => Ok(()),
        Some(want) => Err(format!("expected status {want}, got {actual}")),
        None if (200..300).contains(&actual) => Ok(()),
        None => Err(format!("expected a 2xx status, got {actual}")),
    }
}

/// Render a row's first column as text across the common postgres types.
fn first_column_as_text(row: &postgres::Row) -> Option<String> {
    if let Ok(v) = row.try_get::<_, String>(0) {
        return Some(v);
    }
    if let Ok(v) = row.try_get::<_, i64>(0) {
        return Some(v.to_string());
    }
    if let Ok(v) = row.try_get::<_, i32>(0) {
        return Some(v.to_string());
    }
    if let Ok(v) = row.try_get::<_, f64>(0) {
        return Some(v.to_string());
    }
    if let Ok(v) = row.try_get::<_, bool>(0) {
        return Some(v.to_string());
    }
    None
}

/// Walk a dotted `path` into `root`. Each segment is either a plain object
/// key or a decimal array index; there are no wildcards, filters, brackets,
/// or quoting, so a key that literally contains a dot is unreachable. On a
/// miss, returns the segment where traversal died (for the failure message).
fn resolve_json_path<'a>(
    root: &'a serde_json::Value,
    path: &str,
) -> Result<&'a serde_json::Value, String> {
    let mut current = root;
    for segment in path.split('.') {
        match current {
            serde_json::Value::Object(map) => match map.get(segment) {
                Some(next) => current = next,
                None => return Err(segment.to_string()),
            },
            serde_json::Value::Array(items) => {
                match segment.parse::<usize>().ok().and_then(|i| items.get(i)) {
                    Some(next) => current = next,
                    None => return Err(segment.to_string()),
                }
            }
            _ => return Err(segment.to_string()),
        }
    }
    Ok(current)
}

/// Compare a scalar JSON `leaf` against the `expected` value in two tiers:
/// (a) if BOTH are numbers, an f64 numeric compare; (b) otherwise, an exact
/// compare of their canonical text renderings. Tier (b) means a string leaf
/// never numeric-matches a number ("0953" != 953), and a `${VAR}` that
/// resolved to a string still matches an integer leaf textually.
fn compare_json_leaf(
    leaf: &serde_json::Value,
    expected: &serde_json::Value,
    code: u16,
) -> Result<(), String> {
    if leaf.is_number() && expected.is_number() {
        if leaf.as_f64() == expected.as_f64() {
            return Ok(());
        }
    } else if json_leaf_as_text(leaf) == json_leaf_as_text(expected) {
        return Ok(());
    }
    Err(format!(
        "expected '{}' at path, got '{}' (status {code})",
        json_leaf_as_text(expected),
        json_leaf_as_text(leaf)
    ))
}

/// Canonical text rendering of a scalar JSON value, matching the
/// `first_column_as_text` style: a string as-is, a number/bool via serde_json
/// (`150953`, `true`), null as `null`.
fn json_leaf_as_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_names_map_to_env_vars() {
        assert_eq!(sql_connection_var("reporting"), "FLOWPROOF_SQL_REPORTING");
        assert_eq!(sql_connection_var("dm-main"), "FLOWPROOF_SQL_DM_MAIN");
    }

    #[test]
    fn missing_sql_connection_fails_closed_before_any_polling() {
        let probe = OobProbe::Sql {
            connection: "definitely-not-configured".into(),
            query: "SELECT 1".into(),
            equals: None,
        };
        let err = check(&probe).expect_err("must be a config error");
        assert!(err
            .to_string()
            .contains("FLOWPROOF_SQL_DEFINITELY_NOT_CONFIGURED"));
    }

    #[test]
    fn status_defaults_accept_any_2xx() {
        assert!(check_status(None, 204).is_ok());
        assert!(check_status(None, 404).is_err());
        assert!(check_status(Some(201), 201).is_ok());
        assert!(check_status(Some(200), 500).is_err());
    }

    fn body() -> serde_json::Value {
        serde_json::json!({
            "results": [
                {"balance": 150953, "name": "Ted", "vip": true, "note": null},
                {"balance": 42}
            ]
        })
    }

    #[test]
    fn path_resolves_object_keys_and_array_indices() {
        let v = body();
        assert_eq!(
            resolve_json_path(&v, "results.0.balance").expect("resolves"),
            &serde_json::json!(150953)
        );
        assert_eq!(
            resolve_json_path(&v, "results.1.balance").expect("resolves"),
            &serde_json::json!(42)
        );
        assert_eq!(
            resolve_json_path(&v, "results.0.name").expect("resolves"),
            &serde_json::json!("Ted")
        );
    }

    #[test]
    fn path_names_the_segment_where_traversal_dies() {
        let v = body();
        // A missing object key.
        assert_eq!(
            resolve_json_path(&v, "results.0.nope").expect_err("misses"),
            "nope"
        );
        // An out-of-range array index.
        assert_eq!(resolve_json_path(&v, "results.9").expect_err("misses"), "9");
        // Descending into a scalar.
        assert_eq!(
            resolve_json_path(&v, "results.0.balance.x").expect_err("misses"),
            "x"
        );
    }

    #[test]
    fn path_can_resolve_to_a_non_scalar() {
        // The resolver itself returns containers; the caller rejects them.
        let v = body();
        assert!(resolve_json_path(&v, "results.0")
            .expect("resolves")
            .is_object());
        assert!(resolve_json_path(&v, "results")
            .expect("resolves")
            .is_array());
    }

    #[test]
    fn equals_numeric_tier_matches_number_to_number() {
        assert!(
            compare_json_leaf(&serde_json::json!(150953), &serde_json::json!(150953), 200).is_ok()
        );
        assert!(
            compare_json_leaf(&serde_json::json!(150953), &serde_json::json!(42), 200).is_err()
        );
        // Numeric equality is by value, not by textual spelling.
        assert!(compare_json_leaf(&serde_json::json!(150.0), &serde_json::json!(150), 200).is_ok());
    }

    #[test]
    fn equals_string_leaf_never_numeric_matches_a_number() {
        // The "0953" == 953 false positive: a string leaf drops to the text
        // tier, where "0953" != "953".
        assert!(
            compare_json_leaf(&serde_json::json!("0953"), &serde_json::json!(953), 200).is_err()
        );
        // A string leaf still matches an equal string.
        assert!(
            compare_json_leaf(&serde_json::json!("0953"), &serde_json::json!("0953"), 200).is_ok()
        );
    }

    #[test]
    fn equals_resolved_var_string_matches_integer_leaf_textually() {
        // A ${VAR} that resolved to "150953" (a string) still matches the
        // integer leaf 150953 via the canonical-text tier.
        assert!(compare_json_leaf(
            &serde_json::json!(150953),
            &serde_json::json!("150953"),
            200
        )
        .is_ok());
    }

    #[test]
    fn equals_bool_and_null_compare_by_canonical_text() {
        assert!(compare_json_leaf(&serde_json::json!(true), &serde_json::json!(true), 200).is_ok());
        assert!(
            compare_json_leaf(&serde_json::json!(true), &serde_json::json!(false), 200).is_err()
        );
        // A bool leaf against the string "true" matches textually.
        assert!(
            compare_json_leaf(&serde_json::json!(true), &serde_json::json!("true"), 200).is_ok()
        );
        assert_eq!(json_leaf_as_text(&serde_json::json!(null)), "null");
        assert_eq!(json_leaf_as_text(&serde_json::json!(true)), "true");
        assert_eq!(json_leaf_as_text(&serde_json::json!(150953)), "150953");
        assert_eq!(json_leaf_as_text(&serde_json::json!("hi")), "hi");
    }
}
