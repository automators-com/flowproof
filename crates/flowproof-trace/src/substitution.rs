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

use serde_json::Value;

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

/// Rewrite an OpenAI-compatible chat request body in place, replacing each
/// mocked tool result. Returns which `tool_call_id`s were touched.
///
/// Operates on the raw request JSON, not a parsed view, for one reason: at
/// record the substituted body is FORWARDED to the model, so every field
/// the agent sent (sampling knobs, full tool schemas) has to survive
/// untouched while only the tool-result contents change. A lossy
/// round-trip through a typed struct would drop them. Replay parses the
/// same substituted body afterwards for matching, so both phases share
/// this one transform.
///
/// A `role: tool` message is substituted when its `tool_call_id` names a
/// call whose tool has a mock. The id-to-name map is built from the
/// assistant `tool_calls` in this same request, so a tool message that
/// references an id no earlier assistant turn issued is left alone rather
/// than guessed at.
pub fn apply_json(body: &mut Value, mocks: &Mocks) -> Substitutions {
    if mocks.is_empty() {
        return Substitutions::default();
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Substitutions::default();
    };
    let id_to_name = tool_call_names(messages);

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return Substitutions::default();
    };
    let mut touched = Vec::new();
    for message in messages.iter_mut() {
        if message.get("role").and_then(Value::as_str) != Some("tool") {
            continue;
        }
        let Some(id) = message
            .get("tool_call_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            continue;
        };
        let Some(name) = id_to_name.get(&id) else {
            continue;
        };
        let Some(mock) = mocks.get(name.as_str()) else {
            continue;
        };
        if let Some(obj) = message.as_object_mut() {
            obj.insert("content".into(), Value::String(canonical(mock)));
            touched.push(id);
        }
    }
    Substitutions {
        tool_call_ids: touched,
    }
}

/// Rewrite an Anthropic Messages request body in place, replacing each
/// mocked tool result. Returns which `tool_use_id`s were touched.
///
/// The Anthropic sibling of [`apply_json`], with the SAME contract and the
/// same reason to operate on raw JSON: at record the substituted body is
/// forwarded to the model, so every field the agent sent has to survive
/// while only the mocked tool results change. The shapes differ from the
/// OpenAI wire: a tool result is a `{type:"tool_result", tool_use_id,
/// content}` block inside a user message's content array, and the id-to-name
/// map is built from the `{type:"tool_use", id, name}` blocks in the
/// assistant messages of this same request.
///
/// The overwritten `content` is the canonical JSON STRING of the mock,
/// byte-for-byte what [`apply_json`] writes. That choice is load-bearing for
/// determinism: the neutral parser reads a string tool-result content
/// verbatim, so record (which substitutes then captures) and replay (which
/// substitutes then matches) produce identical neutral bytes, and a volatile
/// real tool result cannot fail replay.
pub fn apply_anthropic_json(body: &mut Value, mocks: &Mocks) -> Substitutions {
    if mocks.is_empty() {
        return Substitutions::default();
    }
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Substitutions::default();
    };
    let id_to_name = tool_use_names(messages);

    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return Substitutions::default();
    };
    let mut touched = Vec::new();
    for message in messages.iter_mut() {
        let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in blocks.iter_mut() {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                continue;
            };
            let Some(name) = id_to_name.get(&id) else {
                continue;
            };
            let Some(mock) = mocks.get(name.as_str()) else {
                continue;
            };
            if let Some(obj) = block.as_object_mut() {
                obj.insert("content".into(), Value::String(canonical(mock)));
                touched.push(id);
            }
        }
    }
    Substitutions {
        tool_call_ids: touched,
    }
}

/// Map every `tool_use_id` an assistant turn issued to the tool it named,
/// reading the raw Anthropic content blocks.
fn tool_use_names(messages: &[Value]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for message in messages {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let id = block.get("id").and_then(Value::as_str);
            let name = block.get("name").and_then(Value::as_str);
            if let (Some(id), Some(name)) = (id, name) {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

/// Map every `tool_call_id` an assistant turn issued to the tool it named,
/// reading the raw messages array.
fn tool_call_names(messages: &[Value]) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for message in messages {
        let Some(calls) = message.get("tool_calls").and_then(Value::as_array) else {
            continue;
        };
        for call in calls {
            let id = call.get("id").and_then(Value::as_str);
            let name = call
                .get("function")
                .and_then(|f| f.get("name"))
                .and_then(Value::as_str);
            if let (Some(id), Some(name)) = (id, name) {
                map.insert(id.to_string(), name.to_string());
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mocks(pairs: &[(&str, Value)]) -> Mocks {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    /// An OpenAI-shaped request body: a user turn, an assistant tool_call,
    /// and the tool result the agent computed.
    fn body(tool_content: &str) -> Value {
        serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "user", "content": "Book a flight"},
                {"role": "assistant", "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "search_flights", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": tool_content},
            ],
            "temperature": 0.7,
        })
    }

    fn content(body: &Value, i: usize) -> Option<&str> {
        body["messages"][i]["content"].as_str()
    }

    /// The headline: the agent's real tool result is replaced with the
    /// spec's mock before the model ever sees it.
    #[test]
    fn a_mocked_tool_result_is_substituted() {
        let mut b = body(r#"{"flights":["whatever the real tool returned"]}"#);
        let subs = apply_json(
            &mut b,
            &mocks(&[("search_flights", serde_json::json!({"flights": ["KQ311"]}))]),
        );
        assert_eq!(subs.tool_call_ids, ["call_1"]);
        assert_eq!(content(&b, 2), Some(r#"{"flights":["KQ311"]}"#));
        // Everything else the agent sent survives untouched, so the body
        // can be forwarded to the model verbatim.
        assert_eq!(b["temperature"], serde_json::json!(0.7));
        assert_eq!(content(&b, 0), Some("Book a flight"));
    }

    /// Canonical JSON: object keys are sorted, so the same mock written in
    /// a different order records identical bytes and never drifts.
    #[test]
    fn substitution_is_canonical_regardless_of_key_order() {
        let mut b = body("old");
        apply_json(
            &mut b,
            &mocks(&[("search_flights", serde_json::json!({"b": 2, "a": 1}))]),
        );
        assert_eq!(content(&b, 2), Some(r#"{"a":1,"b":2}"#));
    }

    /// A tool with no mock passes through verbatim - the documented
    /// replay-drift hazard, honored exactly rather than papered over.
    #[test]
    fn an_unmocked_tool_passes_through() {
        let mut b = serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                {"role": "assistant", "tool_calls": [
                    {"id": "c1", "type": "function",
                     "function": {"name": "search_flights", "arguments": "{}"}},
                    {"id": "c2", "type": "function",
                     "function": {"name": "get_time", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "REAL"},
                {"role": "tool", "tool_call_id": "c2", "content": "2026-07-23T00:00:00Z"},
            ],
        });
        let subs = apply_json(
            &mut b,
            &mocks(&[("search_flights", serde_json::json!({"ok": true}))]),
        );
        assert_eq!(subs.tool_call_ids, ["c1"]);
        assert_eq!(content(&b, 1), Some(r#"{"ok":true}"#));
        assert_eq!(content(&b, 2), Some("2026-07-23T00:00:00Z"));
    }

    /// A tool message whose id no assistant turn issued is left alone.
    #[test]
    fn an_unknown_tool_call_id_is_not_substituted() {
        let mut b = serde_json::json!({
            "messages": [{"role": "tool", "tool_call_id": "orphan", "content": "keep me"}],
        });
        let subs = apply_json(&mut b, &mocks(&[("book", serde_json::json!({}))]));
        assert!(subs.is_empty());
        assert_eq!(content(&b, 0), Some("keep me"));
    }

    /// Empty mocks is a no-op fast path.
    #[test]
    fn no_mocks_is_a_no_op() {
        let mut b = body("real");
        let subs = apply_json(&mut b, &Mocks::new());
        assert!(subs.is_empty());
        assert_eq!(content(&b, 2), Some("real"));
    }

    /// An Anthropic Messages request body: a user turn, an assistant
    /// `tool_use` block, and the `tool_result` the agent computed, carried
    /// in the next user message's content array.
    fn anthropic_body(tool_result: Value) -> Value {
        serde_json::json!({
            "model": "claude-sonnet-4-5",
            "messages": [
                {"role": "user", "content": "Book a flight"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1",
                     "name": "search_flights", "input": {}}]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1",
                     "content": tool_result}]},
            ],
            "max_tokens": 1024,
        })
    }

    fn block_content(body: &Value) -> &Value {
        &body["messages"][2]["content"][0]["content"]
    }

    /// The Anthropic headline: the agent's real tool_result is replaced with
    /// the spec's mock, as the canonical JSON string, before the model sees
    /// it - and everything else the agent sent survives for forwarding.
    #[test]
    fn an_anthropic_tool_result_is_substituted() {
        let mut b = anthropic_body(serde_json::json!("whatever the real tool returned"));
        let subs = apply_anthropic_json(
            &mut b,
            &mocks(&[("search_flights", serde_json::json!({"flights": ["KQ311"]}))]),
        );
        assert_eq!(subs.tool_call_ids, ["toolu_1"]);
        assert_eq!(
            block_content(&b),
            &serde_json::json!(r#"{"flights":["KQ311"]}"#)
        );
        assert_eq!(b["max_tokens"], serde_json::json!(1024));
        assert_eq!(
            b["messages"][0]["content"],
            serde_json::json!("Book a flight")
        );
    }

    /// A volatile result arriving as a `[{type:text,text}]` block array is
    /// still overwritten to the same canonical string, so record and replay
    /// produce identical bytes whatever shape the agent used.
    #[test]
    fn an_anthropic_text_block_result_is_substituted_to_the_canonical_string() {
        let mut b = anthropic_body(serde_json::json!([
            {"type": "text", "text": "2026-07-23T09:41:07.123456Z"}
        ]));
        apply_anthropic_json(
            &mut b,
            &mocks(&[("search_flights", serde_json::json!({"b": 2, "a": 1}))]),
        );
        // Canonical: keys sorted, and a plain JSON string - the neutral
        // parser reads it verbatim, so it matches at replay.
        assert_eq!(block_content(&b), &serde_json::json!(r#"{"a":1,"b":2}"#));
    }

    /// A tool with no mock passes through verbatim, the same replay-drift
    /// hazard the OpenAI path honors.
    #[test]
    fn an_unmocked_anthropic_tool_passes_through() {
        let mut b = anthropic_body(serde_json::json!("REAL"));
        let subs = apply_anthropic_json(&mut b, &mocks(&[("other", serde_json::json!({}))]));
        assert!(subs.is_empty());
        assert_eq!(block_content(&b), &serde_json::json!("REAL"));
    }

    /// A `tool_result` whose id no assistant `tool_use` issued is left alone.
    #[test]
    fn an_unknown_anthropic_tool_use_id_is_not_substituted() {
        let mut b = serde_json::json!({
            "messages": [{"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "orphan", "content": "keep me"}]}],
        });
        let subs = apply_anthropic_json(&mut b, &mocks(&[("book", serde_json::json!({}))]));
        assert!(subs.is_empty());
        assert_eq!(
            b["messages"][0]["content"][0]["content"],
            serde_json::json!("keep me")
        );
    }

    /// The same tool called twice gets the same mock both times.
    #[test]
    fn a_tool_called_twice_is_substituted_both_times() {
        let mut b = serde_json::json!({
            "messages": [
                {"role": "assistant", "tool_calls": [
                    {"id": "c1", "type": "function",
                     "function": {"name": "lookup", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "first real"},
                {"role": "assistant", "tool_calls": [
                    {"id": "c2", "type": "function",
                     "function": {"name": "lookup", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c2", "content": "second real"},
            ],
        });
        let subs = apply_json(&mut b, &mocks(&[("lookup", serde_json::json!({"v": 1}))]));
        assert_eq!(subs.tool_call_ids, ["c1", "c2"]);
        assert_eq!(content(&b, 1), Some(r#"{"v":1}"#));
        assert_eq!(content(&b, 3), Some(r#"{"v":1}"#));
    }
}
