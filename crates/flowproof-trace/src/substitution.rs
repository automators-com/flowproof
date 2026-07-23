//! Result substitution at the model boundary: the transform that makes
//! `tools:` mocks work in v1 (Fable's mechanism D).
//!
//! flowproof only sees the model boundary. When the model asks for a tool
//! call, a real agent runs its own tool and sends the result back in its
//! NEXT request, as a `role: tool` message correlated by `tool_call_id` to
//! the call. That message is the one place a spec-authored mock can reach
//! the model, so this rewrites it: for any tool the spec gave a `result:`,
//! the agent's real result content is replaced with the canonical JSON of
//! the mock BEFORE the request is forwarded (record) or matched (replay).
//!
//! The same transform runs in both phases, in the same place, which is
//! the whole point:
//!
//! - At record it keeps the trajectory DATA-deterministic. The real model
//!   still runs, but every result it conditions on is spec-authored, so a
//!   downstream call's arguments (`create_booking where flight.id equals
//!   KQ311`) are known when the spec is written.
//! - At replay it makes matching immune to tool VOLATILITY. The agent's
//!   real tool still executes and may return a fresh timestamp or id every
//!   run; replacing it before matching means the compared bytes are
//!   spec-constant, so a volatile tool does not fail replay on an
//!   unchanged system.
//!
//! The id-to-name mapping is self-contained within one request: the same
//! message list carries the assistant turns whose `tool_calls` name each
//! id, and the tool turns that answer them. No cross-request state.

use std::collections::BTreeMap;

use crate::cassette::{Message, TurnRequest};

/// The effective `tools:` mocks: tool name to its canonical result JSON.
/// A tool present here is substituted; a tool absent (a name-only `tools:`
/// entry, or a tool the spec never mentioned) passes through untouched.
pub type Mocks = BTreeMap<String, serde_json::Value>;

/// What a substitution pass changed, for the per-exchange trace annotation
/// and the run log.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Substitutions {
    /// `tool_call_id`s whose result content was replaced by a mock.
    pub tool_call_ids: Vec<String>,
}

impl Substitutions {
    pub fn is_empty(&self) -> bool {
        self.tool_call_ids.is_empty()
    }
}

/// Canonical serialization of a mock result: `serde_json::Value` sorts
/// object keys (no `preserve_order` feature), so two specs that wrote the
/// same object in a different key order produce identical bytes, and a
/// recording does not drift on a cosmetic edit.
pub fn canonical(value: &serde_json::Value) -> String {
    value.to_string()
}

/// Rewrite `request` in place, replacing each mocked tool result. Returns
/// which `tool_call_id`s were touched.
///
/// A `role: tool` message is substituted when its `tool_call_id` names a
/// call whose tool has a mock. The id-to-name map is built from the
/// assistant `tool_calls` in this same request, so a tool message that
/// references an id no earlier assistant turn issued is left alone rather
/// than guessed at.
pub fn apply(request: &mut TurnRequest, mocks: &Mocks) -> Substitutions {
    if mocks.is_empty() {
        return Substitutions::default();
    }
    let id_to_name = tool_call_names(&request.messages);
    let mut touched = Vec::new();
    for message in &mut request.messages {
        if message.role != "tool" {
            continue;
        }
        let Some(id) = message.tool_call_id.as_deref() else {
            continue;
        };
        let Some(name) = id_to_name.get(id) else {
            continue;
        };
        let Some(mock) = mocks.get(name.as_str()) else {
            continue;
        };
        message.content = Some(canonical(mock));
        touched.push(id.to_string());
    }
    Substitutions {
        tool_call_ids: touched,
    }
}

/// Map every `tool_call_id` an assistant turn issued to the tool it named.
fn tool_call_names(messages: &[Message]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for message in messages {
        for call in &message.tool_calls {
            map.insert(call.id.clone(), call.name.clone());
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cassette::{Message, ToolCall};

    fn mocks(pairs: &[(&str, serde_json::Value)]) -> Mocks {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn assistant_call(id: &str, name: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments: "{}".into(),
            }],
            tool_call_id: None,
        }
    }

    fn tool_result(id: &str, content: &str) -> Message {
        Message {
            role: "tool".into(),
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: Some(id.into()),
        }
    }

    fn request(messages: Vec<Message>) -> TurnRequest {
        TurnRequest {
            model: "gpt-4o".into(),
            messages,
            tools: vec!["search_flights".into()],
        }
    }

    /// The headline: the agent's real tool result is replaced with the
    /// spec's mock before the model ever sees it.
    #[test]
    fn a_mocked_tool_result_is_substituted() {
        let mut req = request(vec![
            Message::new("user", "Book a flight"),
            assistant_call("call_1", "search_flights"),
            tool_result(
                "call_1",
                r#"{"flights":["whatever the real tool returned"]}"#,
            ),
        ]);
        let subs = apply(
            &mut req,
            &mocks(&[("search_flights", serde_json::json!({"flights": ["KQ311"]}))]),
        );
        assert_eq!(subs.tool_call_ids, ["call_1"]);
        assert_eq!(
            req.messages[2].content.as_deref(),
            Some(r#"{"flights":["KQ311"]}"#)
        );
    }

    /// Canonical JSON: object keys are sorted, so the same mock written in
    /// a different order records identical bytes and never drifts.
    #[test]
    fn substitution_is_canonical_regardless_of_key_order() {
        let mut req = request(vec![assistant_call("c", "book"), tool_result("c", "old")]);
        apply(
            &mut req,
            &mocks(&[("book", serde_json::json!({"b": 2, "a": 1}))]),
        );
        assert_eq!(req.messages[1].content.as_deref(), Some(r#"{"a":1,"b":2}"#));
    }

    /// A tool with no mock (a name-only `tools:` entry, or one the spec
    /// never listed) passes through verbatim. This is the documented
    /// replay-drift hazard, honored exactly rather than papered over.
    #[test]
    fn an_unmocked_tool_passes_through() {
        let mut req = request(vec![
            assistant_call("call_1", "search_flights"),
            tool_result("call_1", "REAL RESULT"),
            assistant_call("call_2", "get_time"),
            tool_result("call_2", "2026-07-23T00:00:00Z"),
        ]);
        let subs = apply(
            &mut req,
            &mocks(&[("search_flights", serde_json::json!({"ok": true}))]),
        );
        assert_eq!(subs.tool_call_ids, ["call_1"]);
        // search_flights was mocked...
        assert_eq!(req.messages[1].content.as_deref(), Some(r#"{"ok":true}"#));
        // ...get_time was not, so its volatile value is untouched.
        assert_eq!(
            req.messages[3].content.as_deref(),
            Some("2026-07-23T00:00:00Z")
        );
    }

    /// A tool message whose id no assistant turn issued is left alone, not
    /// guessed at.
    #[test]
    fn an_unknown_tool_call_id_is_not_substituted() {
        let mut req = request(vec![tool_result("orphan", "keep me")]);
        let subs = apply(&mut req, &mocks(&[("book", serde_json::json!({}))]));
        assert!(subs.is_empty());
        assert_eq!(req.messages[0].content.as_deref(), Some("keep me"));
    }

    /// Empty mocks is a no-op fast path: an agent flow may declare no
    /// mocks at all and still record.
    #[test]
    fn no_mocks_is_a_no_op() {
        let mut req = request(vec![assistant_call("c", "book"), tool_result("c", "real")]);
        let subs = apply(&mut req, &Mocks::new());
        assert!(subs.is_empty());
        assert_eq!(req.messages[1].content.as_deref(), Some("real"));
    }

    /// The same tool called twice gets the same mock both times - the v1
    /// fence (one static result per tool name).
    #[test]
    fn a_tool_called_twice_is_substituted_both_times() {
        let mut req = request(vec![
            assistant_call("c1", "lookup"),
            tool_result("c1", "first real"),
            assistant_call("c2", "lookup"),
            tool_result("c2", "second real"),
        ]);
        let subs = apply(&mut req, &mocks(&[("lookup", serde_json::json!({"v": 1}))]));
        assert_eq!(subs.tool_call_ids, ["c1", "c2"]);
        assert_eq!(req.messages[1].content.as_deref(), Some(r#"{"v":1}"#));
        assert_eq!(req.messages[3].content.as_deref(), Some(r#"{"v":1}"#));
    }
}
