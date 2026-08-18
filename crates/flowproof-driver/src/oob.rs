//! Out-of-band probes: assert business-data correctness directly against
//! the database or an API, not just the pixels. In enterprise E2E the
//! posted record is often the truth a test must verify — the fourth
//! provenance next to uia, sap-com, and vision.
//!
//! Credentials NEVER travel in traces: a probe names a connection, and the
//! connection string / base configuration resolves from the local
//! environment at run time (`FLOWPROOF_SQL_<NAME>`).

use calamine::Reader as _;

use crate::DriverError;

/// One out-of-band check, with every `${VAR}` reference already resolved
/// by the caller (the engine owns secret indirection; this module never
/// sees a raw reference).
#[derive(Debug, Clone, PartialEq)]
// Never stored in bulk: one probe is built per assertion step and consumed
// immediately, so the Sql/Api size imbalance costs nothing worth an extra
// indirection on the hot field.
#[allow(clippy::large_enum_variant)]
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
        /// A response-header NAME (case-INSENSITIVE, per HTTP). When set and
        /// the header repeats, its values join with ", " (HTTP field-value
        /// semantics) before matching. Alone it is an existence check (the
        /// header must be present). Evaluated after `body_json`.
        header: Option<String>,
        /// When set (only meaningful with `header`), the joined header value
        /// must EQUAL this string (exact, case-SENSITIVE value). Any `${VAR}`
        /// reference has already been resolved by the caller. Mutually
        /// exclusive with `header_contains`.
        header_equals: Option<String>,
        /// When set (only meaningful with `header`), the joined header value
        /// must CONTAIN this substring (case-SENSITIVE value). Any `${VAR}`
        /// reference has already been resolved by the caller. Mutually
        /// exclusive with `header_equals`.
        header_contains: Option<String>,
        /// When set (only meaningful with `body_json`), the path must resolve
        /// to an ARRAY satisfying this count. Mutually exclusive with
        /// `equals`; exactly-N and at-least-N are one field, so the two
        /// cannot contradict each other by construction.
        count: Option<ArrayCount>,
        /// Override the method-derived retry policy (see `is_retryable`):
        /// `Some(true)` polls a write until the expectation holds,
        /// `Some(false)` sends a read exactly once.
        retry: Option<bool>,
    },
    /// Out-of-band spreadsheet probe: the exported file on disk, not the
    /// pixel Excel happens to render. A read like `Sql`, so it is always
    /// retryable — a just-finished download may still be mid-write when the
    /// first poll fires.
    Spreadsheet {
        /// Resolved path to the workbook. Format is auto-detected from the
        /// extension (xlsx/xlsm/xlsb/xls/ods — whatever `calamine` reads).
        path: String,
        /// Sheet name; the workbook's first sheet when unset.
        sheet: Option<String>,
        /// An `A1`-style cell reference (`"B2"`), absolute within the
        /// sheet. Mutually exclusive with `column`/`row_contains` (the
        /// caller enforces this at parse time, same as `assert_api`'s
        /// cross-field rules).
        at: Option<String>,
        /// Header text identifying the column: matched exact-after-trim,
        /// then unique-contains, against the sheet's first row — the same
        /// two-rung ladder the web adapter's `CellQuery` uses, for a static
        /// file instead of a live DOM.
        column: Option<String>,
        /// Anchor text identifying the row: the row (excluding the header)
        /// where ANY cell's text contains it must be unique.
        row_contains: Option<String>,
        /// The cell's text (via its `Display` rendering) must equal this.
        equals: Option<String>,
        /// The cell's text must contain this substring. At most one of
        /// `equals`/`contains` is set (enforced at parse time); with
        /// neither, resolving the cell is the whole assertion.
        contains: Option<String>,
    },
}

/// How many elements the array at `body_json` must hold. One value rather
/// than two optional ones: "exactly N" and "at least N" are the same
/// question asked two ways, so they cannot both be set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrayCount {
    Exactly(u64),
    AtLeast(u64),
}

/// The JSON kind at a path, for a failure that says what was actually
/// there instead of only what was wanted.
fn json_kind(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "a boolean",
        serde_json::Value::Number(_) => "a number",
        serde_json::Value::String(_) => "a string",
        serde_json::Value::Array(_) => "an array",
        serde_json::Value::Object(_) => "an object",
    }
}

/// Whether a failing probe may be re-sent while auto-waiting.
///
/// Auto-wait re-fires the probe until the expectation holds, which is right
/// for an OBSERVATION (the row is still committing, the API is converging)
/// and wrong for a MUTATION: the probe IS the write, so polling a failing
/// `POST` delivers it once per interval. A single failing step measured 41
/// deliveries against a counting server, which in a real suite means ~41
/// duplicate payments - and it only happens when a test fails, when the
/// state is hardest to reason about.
///
/// The rule is the METHOD, not the error kind. A response mismatch proves
/// the request was delivered, but a transport error does NOT prove it was
/// not: a connection reset after the bytes were flushed is the canonical
/// duplicate-write case, and the underlying client does not distinguish
/// that from a refusal reliably enough to bet writes on it.
pub fn is_retryable(probe: &OobProbe) -> bool {
    match probe {
        // A query is a read for this purpose; `assert_sql` asserts state.
        OobProbe::Sql { .. } => true,
        OobProbe::Api { method, retry, .. } => {
            retry.unwrap_or_else(|| matches!(method.to_ascii_uppercase().as_str(), "GET" | "HEAD"))
        }
        // A spreadsheet read, like a query: the file may still be mid-write
        // right after a download lands, and polling never resends anything.
        OobProbe::Spreadsheet { .. } => true,
    }
}

/// The hint appended to a non-retried probe's failure, so a flow that
/// relied on polling a write self-diagnoses on its first failure.
pub const RETRY_HINT: &str =
    "this method is not retried, so it was sent once; add `retry: true` to the step to poll until the expectation holds";

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
            // `retry` steers the caller's poll loop, not this single send.
            retry: _,
            count,
            method,
            url,
            body,
            headers,
            status,
            body_contains,
            body_json,
            equals,
            header,
            header_equals,
            header_contains,
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
            // Pluck the named header while the immutable borrow is live, BEFORE
            // the mutable body read. The outer Option mirrors `header` (is an
            // assertion present); the inner Option is presence in the response
            // (None = absent). Repeats join with ", " per HTTP field-value
            // semantics. The value never enters the trace: it lives only here.
            let header_actual: Option<Option<String>> = header
                .as_ref()
                .map(|name| joined_header(response.headers(), name));
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
                // A count asks about a COLLECTION, so it is the one predicate
                // that wants a non-scalar. Its own type error is more precise
                // than the generic leaf rule, so it is checked first.
                if let Some(want) = count {
                    let Some(items) = leaf.as_array() else {
                        return Ok(Err(format!(
                            "path '{path}' is {}, count requires an array (status {code})",
                            json_kind(leaf)
                        )));
                    };
                    let found = items.len() as u64;
                    let (holds, expectation) = match want {
                        ArrayCount::Exactly(n) => (found == *n, format!("exactly {n}")),
                        ArrayCount::AtLeast(n) => (found >= *n, format!("at least {n}")),
                    };
                    if !holds {
                        return Ok(Err(format!(
                            "path '{path}' has {found} elements, expected {expectation} (status {code})"
                        )));
                    }
                } else {
                    if leaf.is_object() || leaf.is_array() {
                        return Ok(Err(
                            "path resolves to a non-scalar; assert a leaf value".to_string()
                        ));
                    }
                    // `body_json` with no `equals` is an existence check:
                    // reaching a scalar leaf is the whole assertion (mirrors
                    // assert_sql, where an omitted `equals` means a row merely
                    // has to exist).
                    if let Some(expected) = equals {
                        if let Err(reason) = compare_json_leaf(leaf, expected, code) {
                            return Ok(Err(reason));
                        }
                    }
                }
            }
            if let Some(name) = header {
                // `header_actual` is Some(..) here (it was built from `header`).
                let actual = header_actual.as_ref().and_then(|inner| inner.as_deref());
                if let Err(reason) = check_header(
                    name,
                    actual,
                    header_equals.as_deref(),
                    header_contains.as_deref(),
                    code,
                ) {
                    return Ok(Err(reason));
                }
            }
            // The response body IS the corpus element an `assert_no_secret_leak`
            // scan reads: return it so the caller can scan it in memory. It is
            // never persisted: the trace keeps only the request and the raw
            // `${VAR}` expectations.
            Ok(Ok(Some(text)))
        }
        OobProbe::Spreadsheet {
            path,
            sheet,
            at,
            column,
            row_contains,
            equals,
            contains,
        } => {
            let mut workbook = match calamine::open_workbook_auto(path) {
                Ok(workbook) => workbook,
                // A file that has not landed yet (a download still writing)
                // is the common case here: a MISS the caller may poll past,
                // not a configuration error.
                Err(e) => return Ok(Err(format!("opening '{path}': {e}"))),
            };
            let sheet_name = match sheet {
                Some(name) => name.clone(),
                None => {
                    let Some(first) = workbook.sheet_names().into_iter().next() else {
                        return Err(DriverError::Uia(format!("'{path}' has no sheets")));
                    };
                    first
                }
            };
            let range = match workbook.worksheet_range(&sheet_name) {
                Ok(range) => range,
                Err(e) => return Ok(Err(format!("reading sheet '{sheet_name}': {e}"))),
            };
            let cell = match at {
                Some(reference) => {
                    let Some((row, col)) = parse_a1_cell(reference) else {
                        return Err(DriverError::Uia(format!(
                            "'{reference}' is not a valid cell reference (e.g. 'B2')"
                        )));
                    };
                    let (start_row, start_col) = range.start().unwrap_or((0, 0));
                    let (Some(row), Some(col)) = (
                        row.checked_sub(start_row as usize),
                        col.checked_sub(start_col as usize),
                    ) else {
                        return Ok(Err(format!(
                            "'{reference}' is outside sheet '{sheet_name}''s used range"
                        )));
                    };
                    range.get((row, col))
                }
                None => {
                    // Parse-time validation (mirroring `assert_api`'s
                    // cross-field rules) guarantees exactly one of `at` or
                    // this pair is set by the time a probe reaches here.
                    let (Some(column), Some(anchor)) = (column, row_contains) else {
                        return Err(DriverError::Uia(
                            "assert_spreadsheet needs either `at` or both `column` and \
                             `row_contains`"
                                .into(),
                        ));
                    };
                    let col = match find_spreadsheet_column(&range, column) {
                        Ok(col) => col,
                        Err(reason) => return Ok(Err(reason)),
                    };
                    let row = match find_spreadsheet_row(&range, anchor) {
                        Ok(row) => row,
                        Err(reason) => return Ok(Err(reason)),
                    };
                    range.get((row, col))
                }
            };
            let actual = cell.map(ToString::to_string).unwrap_or_default();
            if let Some(expected) = equals {
                if &actual != expected {
                    return Ok(Err(format!("expected cell '{expected}', got '{actual}'")));
                }
            } else if let Some(needle) = contains {
                if !actual.contains(needle.as_str()) {
                    return Ok(Err(format!(
                        "expected cell to contain '{needle}', got '{actual}'"
                    )));
                }
            }
            Ok(Ok(None))
        }
    }
}

/// Parse an absolute `A1`-style reference (`"B2"`) into a zero-indexed
/// `(row, col)` pair. Column letters are base-26 (`A`=1 … `Z`=26, `AA`=27,
/// …), matching spreadsheet convention; anything that isn't
/// `[A-Za-z]+[0-9]+` is not a cell reference.
fn parse_a1_cell(reference: &str) -> Option<(usize, usize)> {
    let reference = reference.trim();
    let split = reference.find(|c: char| !c.is_ascii_alphabetic())?;
    let (col_part, row_part) = reference.split_at(split);
    if col_part.is_empty() || row_part.is_empty() || !row_part.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut col = 0usize;
    for c in col_part.chars() {
        col = col * 26 + (c.to_ascii_uppercase() as usize - 'A' as usize + 1);
    }
    let row: usize = row_part.parse().ok()?;
    Some((row.checked_sub(1)?, col.checked_sub(1)?))
}

/// Find `column`'s index in the sheet's first row: exact-after-trim, then
/// unique-contains — the same two-rung ladder the web adapter's `CellQuery`
/// uses for a live DOM table, applied here to a static header row.
fn find_spreadsheet_column(
    range: &calamine::Range<calamine::Data>,
    column: &str,
) -> Result<usize, String> {
    let Some(header) = range.rows().next() else {
        return Err("sheet has no header row".to_string());
    };
    let want = column.trim();
    let mut exact = Vec::new();
    let mut partial = Vec::new();
    for (i, cell) in header.iter().enumerate() {
        let text = cell.to_string();
        let trimmed = text.trim();
        if trimmed == want {
            exact.push(i);
        } else if trimmed.contains(want) {
            partial.push(i);
        }
    }
    match exact.as_slice() {
        [i] => Ok(*i),
        [] => match partial.as_slice() {
            [i] => Ok(*i),
            [] => Err(format!("no column header matches '{column}'")),
            many => Err(format!(
                "column header '{column}' matches {} headers - use a more specific header text",
                many.len()
            )),
        },
        many => Err(format!(
            "column header '{column}' is not unique ({} exact matches)",
            many.len()
        )),
    }
}

/// Find the unique data row (excluding the header) where ANY cell's text
/// contains `anchor`. Every anchor rung elsewhere in flowproof (the web
/// adapter's cell/scope resolvers) uses the same "contains, must be unique"
/// rule; a static sheet gets the identical contract.
fn find_spreadsheet_row(
    range: &calamine::Range<calamine::Data>,
    anchor: &str,
) -> Result<usize, String> {
    let matches: Vec<usize> = range
        .rows()
        .enumerate()
        .skip(1)
        .filter(|(_, row)| row.iter().any(|cell| cell.to_string().contains(anchor)))
        .map(|(i, _)| i)
        .collect();
    match matches.as_slice() {
        [i] => Ok(*i),
        [] => Err(format!("no row contains '{anchor}'")),
        many => Err(format!(
            "row anchor '{anchor}' matches {} rows - use a more specific anchor",
            many.len()
        )),
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

/// Pluck a response header by NAME (case-INSENSITIVE, per HTTP: `http`'s
/// `HeaderMap` lowercases and compares names insensitively). Returns `None`
/// when the header is absent; when it repeats, the values join with ", "
/// (HTTP field-value semantics). Non-UTF-8 values are skipped.
fn joined_header(headers: &ureq::http::HeaderMap, name: &str) -> Option<String> {
    let values: Vec<&str> = headers
        .get_all(name)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join(", "))
    }
}

/// Match a response header against the expectation. `actual` is the joined
/// header value (`None` = the header is absent). With neither `equals` nor
/// `contains` the check is existence (present passes). Value comparison is
/// case-SENSITIVE (exact for `equals`, substring for `contains`). At most one
/// of `equals`/`contains` is set (the caller enforces this at parse time).
fn check_header(
    name: &str,
    actual: Option<&str>,
    equals: Option<&str>,
    contains: Option<&str>,
    code: u16,
) -> Result<(), String> {
    let Some(actual) = actual else {
        return Err(format!("response has no '{name}' header (status {code})"));
    };
    if let Some(want) = equals {
        if actual != want {
            return Err(format!(
                "header '{name}' is '{actual}', expected equals '{want}' (status {code})"
            ));
        }
    } else if let Some(want) = contains {
        if !actual.contains(want) {
            return Err(format!(
                "header '{name}' is '{actual}', expected contains '{want}' (status {code})"
            ));
        }
    }
    Ok(())
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

    fn header_map(pairs: &[(&str, &str)]) -> ureq::http::HeaderMap {
        use ureq::http::header::{HeaderName, HeaderValue};
        let mut map = ureq::http::HeaderMap::new();
        for (name, value) in pairs {
            map.append(
                HeaderName::from_bytes(name.as_bytes()).expect("valid name"),
                HeaderValue::from_str(value).expect("valid value"),
            );
        }
        map
    }

    #[test]
    fn header_lookup_is_case_insensitive_on_the_name() {
        // The response spells it `Content-Type`; the spec may ask for any case.
        let headers = header_map(&[("Content-Type", "application/json")]);
        assert_eq!(
            joined_header(&headers, "content-type").as_deref(),
            Some("application/json")
        );
        assert_eq!(
            joined_header(&headers, "CONTENT-TYPE").as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn header_absent_lookup_is_none() {
        let headers = header_map(&[("Content-Type", "application/json")]);
        assert_eq!(joined_header(&headers, "X-Missing"), None);
    }

    #[test]
    fn repeated_header_values_join_with_comma_space() {
        // A repeated field folds to one value per HTTP field-value semantics.
        let headers = header_map(&[("Set-Cookie", "a=1"), ("Set-Cookie", "b=2")]);
        assert_eq!(
            joined_header(&headers, "set-cookie").as_deref(),
            Some("a=1, b=2")
        );
    }

    #[test]
    fn header_present_equals_matches_exactly() {
        assert!(check_header(
            "Content-Type",
            Some("application/json"),
            Some("application/json"),
            None,
            200,
        )
        .is_ok());
    }

    #[test]
    fn header_present_contains_matches_substring() {
        assert!(check_header(
            "Content-Type",
            Some("application/json; charset=utf-8"),
            None,
            Some("application/json"),
            200,
        )
        .is_ok());
    }

    #[test]
    fn header_alone_is_an_existence_check() {
        // Present with neither equals nor contains: existence passes.
        assert!(check_header("Content-Type", Some("text/html"), None, None, 200).is_ok());
    }

    #[test]
    fn absent_header_soft_fails_with_a_named_message() {
        let err = check_header("X-Does-Not-Exist", None, None, Some("json"), 200)
            .expect_err("absent header must fail");
        assert_eq!(
            err,
            "response has no 'X-Does-Not-Exist' header (status 200)"
        );
    }

    #[test]
    fn header_value_mismatch_names_actual_and_expected() {
        let err = check_header(
            "Content-Type",
            Some("application/json"),
            Some("text/html"),
            None,
            200,
        )
        .expect_err("mismatch must fail");
        assert_eq!(
            err,
            "header 'Content-Type' is 'application/json', expected equals 'text/html' (status 200)"
        );
        // The contains tier reports its own predicate in the message.
        let err = check_header(
            "Content-Type",
            Some("application/json"),
            None,
            Some("xml"),
            200,
        )
        .expect_err("contains mismatch must fail");
        assert_eq!(
            err,
            "header 'Content-Type' is 'application/json', expected contains 'xml' (status 200)"
        );
    }

    #[test]
    fn spreadsheet_is_always_retried() {
        let probe = OobProbe::Spreadsheet {
            path: "x.xlsx".into(),
            sheet: None,
            at: Some("A1".into()),
            column: None,
            row_contains: None,
            equals: None,
            contains: None,
        };
        assert!(is_retryable(&probe));
    }

    #[test]
    fn a1_references_parse_to_zero_indexed_row_col() {
        assert_eq!(parse_a1_cell("A1"), Some((0, 0)));
        assert_eq!(parse_a1_cell("B2"), Some((1, 1)));
        assert_eq!(parse_a1_cell("aa10"), Some((9, 26)));
        assert_eq!(parse_a1_cell(" C5 "), Some((4, 2)));
    }

    #[test]
    fn malformed_a1_references_are_rejected() {
        assert_eq!(parse_a1_cell("2B"), None);
        assert_eq!(parse_a1_cell("B0"), None, "spreadsheet rows are 1-based");
        assert_eq!(parse_a1_cell("B"), None);
        assert_eq!(parse_a1_cell("2"), None);
        assert_eq!(parse_a1_cell(""), None);
    }

    #[test]
    fn a_missing_workbook_is_a_pollable_miss_not_a_hard_error() {
        // A download the polling loop hasn't seen land yet reads as a MISS
        // (Ok(Err(..))), so the caller's auto-wait can retry it - not a
        // config error that aborts immediately.
        let probe = OobProbe::Spreadsheet {
            path: "/definitely/does/not/exist.xlsx".into(),
            sheet: None,
            at: Some("A1".into()),
            column: None,
            row_contains: None,
            equals: None,
            contains: None,
        };
        let outcome = check(&probe).expect("a missing file is a soft miss, not Err");
        assert!(outcome.is_err(), "the cell cannot resolve yet");
    }

    fn sheet_with_header_and_rows() -> calamine::Range<calamine::Data> {
        use calamine::{Cell, Data, Range};
        let cells = vec![
            Cell::new((0, 0), Data::String("Order Type".to_string())),
            Cell::new((0, 1), Data::String("Net Price".to_string())),
            Cell::new((0, 2), Data::String("Price Currency".to_string())),
            Cell::new((1, 0), Data::String("ZOR - 100-100".to_string())),
            Cell::new((1, 1), Data::Float(12.5)),
            Cell::new((1, 2), Data::String("USD".to_string())),
            Cell::new((2, 0), Data::String("ZOR - 100-200".to_string())),
            Cell::new((2, 1), Data::Float(30.0)),
            Cell::new((2, 2), Data::String("USD".to_string())),
        ];
        Range::from_sparse(cells)
    }

    #[test]
    fn column_resolves_by_exact_header_match() {
        let sheet = sheet_with_header_and_rows();
        assert_eq!(find_spreadsheet_column(&sheet, "Net Price"), Ok(1));
    }

    #[test]
    fn column_falls_back_to_unique_contains() {
        // No header is exactly "Order", but "Order Type" is the only one
        // that contains it, so this rung resolves unambiguously.
        let sheet = sheet_with_header_and_rows();
        assert_eq!(find_spreadsheet_column(&sheet, "Order"), Ok(0));
    }

    #[test]
    fn column_contains_ambiguity_is_an_error() {
        let sheet = sheet_with_header_and_rows();
        let err = find_spreadsheet_column(&sheet, "Price").expect_err("ambiguous");
        assert!(err.contains("Price") && err.contains('2'), "{err}");
    }

    #[test]
    fn column_not_found_names_what_was_asked_for() {
        let sheet = sheet_with_header_and_rows();
        let err = find_spreadsheet_column(&sheet, "Nope").expect_err("no such header");
        assert!(err.contains("Nope"), "{err}");
    }

    #[test]
    fn row_resolves_by_unique_anchor_across_any_cell() {
        let sheet = sheet_with_header_and_rows();
        assert_eq!(find_spreadsheet_row(&sheet, "100-200"), Ok(2));
    }

    #[test]
    fn row_anchor_matching_every_row_is_ambiguous() {
        let sheet = sheet_with_header_and_rows();
        let err = find_spreadsheet_row(&sheet, "ZOR").expect_err("matches both data rows");
        assert!(err.contains("ZOR") && err.contains('2'), "{err}");
    }

    #[test]
    fn row_anchor_with_no_match_names_it() {
        let sheet = sheet_with_header_and_rows();
        let err = find_spreadsheet_row(&sheet, "999-999").expect_err("no such row");
        assert!(err.contains("999-999"), "{err}");
    }

    #[test]
    fn resolved_cell_reads_via_column_and_row_contains() {
        let sheet = sheet_with_header_and_rows();
        let col = find_spreadsheet_column(&sheet, "Net Price").expect("column");
        let row = find_spreadsheet_row(&sheet, "100-200").expect("row");
        let cell = sheet.get((row, col)).expect("in range");
        assert_eq!(cell.to_string(), "30");
    }
}
