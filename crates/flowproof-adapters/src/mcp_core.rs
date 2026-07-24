//! The transport-independent core of the MCP boundary: the recorded-call
//! shape, the strict-positional matcher, the argument diff, and the local
//! answer for a mocked tool.
//!
//! Both MCP transports reuse this UNCHANGED. `mcp_stdio` bridges JSON-RPC
//! over a subprocess's stdin/stdout; `mcp_http` hosts an in-process HTTP
//! listener the agent dials. Neither owns the matching or mocking rules -
//! an HTTP lane and a stdio lane are indistinguishable in the trace, so a
//! stdio-recorded lane replays through an HTTP-declared server and vice
//! versa, which is only possible because the same functions decide it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One recorded MCP exchange: the request's method and params, and the
/// result served back. `arguments`/`params` live as JSON (not re-encoded
/// strings) because MCP carries them as JSON objects on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCall {
    pub method: String,
    #[serde(default)]
    pub params: Value,
    pub result: Value,
}

/// Where a replay lane diverged: the 0-based lane index and the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpDivergence {
    pub index: usize,
    pub detail: String,
}

/// One server-initiated MCP message that crossed toward the agent out of
/// band of a client call: a NOTIFICATION (a `method`, no `id`) in v3.3, and
/// - reserved for v3.4 - a server-initiated REQUEST (`id` + `answer`).
///
/// `after` is the lane counter's value when the event crossed toward the
/// agent: the number of client calls answered before it. It is RECORDED and
/// REPLAYED, never MATCHED - a notification racing at n vs n+1 changes bytes,
/// not the verdict, so the anchor is an emission cue, not an assertion.
///
/// The v3.4 fields (`id`, `answer`) stay skipped when absent so a v3.3 event
/// (a notification) serializes with just `after`/`method`/`params`, and a
/// future request event adds them without a format break.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerEvent {
    /// The lane counter's value when the event crossed toward the agent.
    pub after: usize,
    /// The JSON-RPC method (`notifications/...` for a notification).
    pub method: String,
    /// The notification's params, verbatim.
    #[serde(default)]
    pub params: Value,
    /// v3.4 only: a server-initiated request's id. Kept skipped in v3.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    /// v3.4 only: the agent's answer to a server-initiated request. Kept
    /// skipped in v3.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<Value>,
}

impl McpServerEvent {
    /// A v3.3 notification event anchored at `after`.
    pub fn notification(after: usize, method: String, params: Value) -> Self {
        Self {
            after,
            method,
            params,
            id: None,
            answer: None,
        }
    }
}

/// The named record failure BOTH transports raise on a server-initiated
/// REQUEST (sampling / elicitation / roots-list): recording it is v3.4, so
/// v3.3 fails loudly rather than corrupt a lane. Shared so the stdio bridge
/// and the HTTP forwarder word it identically.
pub(crate) fn server_request_named(method: &str) -> String {
    format!(
        "the real MCP server sent a server-initiated request (`{method}`) mid-response; \
         recording server-initiated traffic is v3.4"
    )
}

/// Match an incoming request against the recorded lane entry, mirroring the
/// cassette's method-first, envelope-then-body doctrine. `initialize`
/// matches `protocolVersion` but NOT `clientInfo`/`capabilities` (the
/// "ignored knobs" precedent: they are recorded and reported, not matched).
pub(crate) fn match_call(
    recorded: &McpCall,
    method: &str,
    params: Option<&Value>,
) -> Result<(), String> {
    if method != recorded.method {
        return Err(format!(
            "method changed: recorded `{}`, replayed `{method}`",
            recorded.method
        ));
    }
    let null = Value::Null;
    let params = params.unwrap_or(&null);
    match method {
        "initialize" => {
            let want = recorded.params.get("protocolVersion");
            let got = params.get("protocolVersion");
            if want != got {
                return Err(format!(
                    "initialize protocolVersion changed: recorded {}, replayed {}",
                    show(want),
                    show(got)
                ));
            }
            Ok(())
        }
        "tools/call" => {
            let want_name = recorded.params.get("name").and_then(Value::as_str);
            let got_name = params.get("name").and_then(Value::as_str);
            if want_name != got_name {
                return Err(format!(
                    "tools/call name changed: recorded {}, replayed {}",
                    want_name
                        .map(|n| format!("`{n}`"))
                        .unwrap_or("absent".into()),
                    got_name
                        .map(|n| format!("`{n}`"))
                        .unwrap_or("absent".into()),
                ));
            }
            let want_args = recorded.params.get("arguments").unwrap_or(&null);
            let got_args = params.get("arguments").unwrap_or(&null);
            match json_diff_path(want_args, got_args, "arguments") {
                Some(detail) => Err(detail),
                None => Ok(()),
            }
        }
        _ => match json_diff_path(&recorded.params, params, "params") {
            Some(detail) => Err(detail),
            None => Ok(()),
        },
    }
}

/// The first path at which two JSON values diverge, named
/// (`arguments.city`, `params.items[2].id`). `None` when they are equal.
/// Deterministic: object keys are compared in sorted order.
fn json_diff_path(recorded: &Value, incoming: &Value, path: &str) -> Option<String> {
    if recorded == incoming {
        return None;
    }
    match (recorded, incoming) {
        (Value::Object(want), Value::Object(got)) => {
            let mut keys: Vec<&String> = want.keys().chain(got.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                match (want.get(key), got.get(key)) {
                    (Some(a), Some(b)) => {
                        if let Some(detail) = json_diff_path(a, b, &format!("{path}.{key}")) {
                            return Some(detail);
                        }
                    }
                    (Some(_), None) => {
                        return Some(format!(
                            "{path}.{key} missing: recorded present, replayed absent"
                        ))
                    }
                    (None, Some(_)) => {
                        return Some(format!(
                            "{path}.{key} added: recorded absent, replayed present"
                        ))
                    }
                    (None, None) => {}
                }
            }
            None
        }
        (Value::Array(want), Value::Array(got)) => {
            if want.len() != got.len() {
                return Some(format!(
                    "{path} length changed: recorded {}, replayed {}",
                    want.len(),
                    got.len()
                ));
            }
            for (i, (a, b)) in want.iter().zip(got).enumerate() {
                if let Some(detail) = json_diff_path(a, b, &format!("{path}[{i}]")) {
                    return Some(detail);
                }
            }
            None
        }
        _ => Some(format!(
            "{path} changed: recorded {}, replayed {}",
            abbreviate(&recorded.to_string()),
            abbreviate(&incoming.to_string())
        )),
    }
}

/// The MCP `tools/call` result shape for a mocked tool: a single text
/// content block carrying the mock. A string mock is used verbatim;
/// anything else is JSON-encoded. Built identically at record (the local
/// answer) and served verbatim at replay, so the two never disagree.
pub(crate) fn mock_tool_result(mock: &Value) -> Value {
    let text = match mock {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    serde_json::json!({
        "content": [ { "type": "text", "text": text } ],
        "isError": false,
    })
}

/// The JSON-RPC success envelope for a request id and result. Shared so the
/// stdio writer and the HTTP body builder never disagree on shape.
pub(crate) fn result_envelope(id: &Value, result: &Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// The JSON-RPC error envelope a diverged or past-the-end request gets, so
/// the agent is answered rather than left waiting forever. In-band on the
/// stdio pipe and in the HTTP 200 body alike.
pub(crate) fn error_envelope(id: &Value, detail: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": -32000, "message": detail },
    })
}

/// The JSON-RPC notification envelope (a method and params, NO id) a
/// recorded server event is re-emitted as at replay. Shared so the stdio
/// pipe writer and the HTTP SSE framer never disagree on shape.
pub(crate) fn notification_envelope(method: &str, params: &Value) -> Value {
    serde_json::json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// Render an optional JSON value for a divergence line.
fn show(value: Option<&Value>) -> String {
    value
        .map(Value::to_string)
        .unwrap_or_else(|| "absent".into())
}

/// Truncate a value's JSON for a diff line: the first divergent stretch is
/// what identifies it, same as the cassette's `abbreviate`.
fn abbreviate(text: &str) -> String {
    const LIMIT: usize = 160;
    if text.chars().count() <= LIMIT {
        return text.to_string();
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("{head}...")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value, result: Value) -> McpCall {
        McpCall {
            method: method.into(),
            params,
            result,
        }
    }

    #[test]
    fn identical_tools_call_matches() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } }),
            Value::Null,
        );
        let incoming =
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } });
        assert!(match_call(&recorded, "tools/call", Some(&incoming)).is_ok());
    }

    #[test]
    fn a_changed_argument_names_the_first_divergent_path() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Nairobi" } }),
            Value::Null,
        );
        let incoming =
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "Paris" } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("arguments.city"), "{err}");
        assert!(err.contains("Nairobi") && err.contains("Paris"), "{err}");
    }

    #[test]
    fn a_changed_method_is_named() {
        let recorded = call("tools/list", serde_json::json!({}), Value::Null);
        let err = match_call(&recorded, "resources/list", None).expect_err("must diverge");
        assert!(err.contains("method changed"), "{err}");
        assert!(
            err.contains("tools/list") && err.contains("resources/list"),
            "{err}"
        );
    }

    #[test]
    fn a_changed_tool_name_is_named_before_arguments() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "get_weather", "arguments": { "city": "X" } }),
            Value::Null,
        );
        let incoming = serde_json::json!({ "name": "send_alert", "arguments": { "city": "Y" } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("name changed"), "{err}");
        assert!(!err.contains("arguments"), "name wins over args: {err}");
    }

    /// `initialize` matches `protocolVersion` but ignores `clientInfo` and
    /// `capabilities` - they are recorded and reported, never matched.
    #[test]
    fn initialize_matches_protocol_version_and_ignores_client_knobs() {
        let recorded = call(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "clientInfo": { "name": "recorder", "version": "1.0" },
                "capabilities": {},
            }),
            Value::Null,
        );
        // Different clientInfo and capabilities, same protocolVersion: match.
        let incoming = serde_json::json!({
            "protocolVersion": "2024-11-05",
            "clientInfo": { "name": "replayer", "version": "9.9" },
            "capabilities": { "roots": {} },
        });
        assert!(match_call(&recorded, "initialize", Some(&incoming)).is_ok());
        // A changed protocolVersion IS a divergence.
        let bumped = serde_json::json!({ "protocolVersion": "2025-06-18" });
        let err = match_call(&recorded, "initialize", Some(&bumped)).expect_err("must diverge");
        assert!(err.contains("protocolVersion"), "{err}");
    }

    #[test]
    fn a_string_mock_becomes_a_text_content_block_verbatim() {
        let result = mock_tool_result(&Value::String("sunny".into()));
        assert_eq!(result["content"][0]["type"], "text");
        assert_eq!(result["content"][0]["text"], "sunny");
        assert_eq!(result["isError"], false);
    }

    #[test]
    fn a_structured_mock_is_json_encoded_in_the_text_block() {
        let result = mock_tool_result(&serde_json::json!({ "temp": 25, "sky": "clear" }));
        // serde_json sorts keys, so the encoding is canonical/deterministic.
        assert_eq!(result["content"][0]["text"], r#"{"sky":"clear","temp":25}"#);
    }

    #[test]
    fn a_missing_argument_is_named() {
        let recorded = call(
            "tools/call",
            serde_json::json!({ "name": "t", "arguments": { "a": 1, "b": 2 } }),
            Value::Null,
        );
        let incoming = serde_json::json!({ "name": "t", "arguments": { "a": 1 } });
        let err = match_call(&recorded, "tools/call", Some(&incoming)).expect_err("must diverge");
        assert!(err.contains("arguments.b"), "{err}");
        assert!(err.contains("missing"), "{err}");
    }
}
