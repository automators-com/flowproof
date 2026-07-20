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

/// One evaluation attempt. `Ok(Ok(()))` — the expectation holds.
/// `Ok(Err(reason))` — it does not YET hold (the caller's auto-wait loop
/// may poll again: the row may still be committing, the API converging).
/// `Err(_)` — a configuration error polling cannot fix (missing
/// connection env), failed immediately and loudly.
pub fn check(probe: &OobProbe) -> Result<Result<(), String>, DriverError> {
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
                return Ok(Ok(()));
            };
            let actual = first_column_as_text(row);
            if actual.as_deref() == Some(expected.as_str()) {
                Ok(Ok(()))
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
            Ok(Ok(()))
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
}
