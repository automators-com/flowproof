//! Keeps the `Cassette` serde types and the cassette JSON Schema in
//! agreement. The schema exists because `delivery_index`/`deliveries` are
//! the on-disk shape of multi-turn `conversation:` flows, and a field that
//! only a Rust struct describes is undocumented in the form a consumer can
//! actually check against.
//!
//! The load-bearing property is the one the multi-turn work rests on: the
//! new fields are ABSENT at their defaults, so every cassette recorded
//! before they existed still validates.

use flowproof_trace::cassette::{
    default_protocol, Cassette, DeliveryMeta, Message, ToolCall, Turn, TurnRequest, TurnResponse,
};

const SCHEMA: &str = include_str!("../schema/cassette-v1.schema.json");

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is valid JSON");
    jsonschema::validator_for(&schema).expect("schema compiles")
}

fn turn(user: &str, reply: &str, delivery_index: usize) -> Turn {
    Turn {
        protocol: default_protocol(),
        request: TurnRequest {
            model: "gpt-4o".into(),
            messages: vec![Message::new("user", user)],
            tools: Vec::new(),
        },
        response: TurnResponse {
            message: Message::new("assistant", reply),
            stop_reason: None,
        },
        delivery_index,
    }
}

fn assert_validates(validator: &jsonschema::Validator, value: &serde_json::Value) {
    assert!(
        validator.validate(value).is_ok(),
        "failed schema validation: {:?}",
        validator.iter_errors(value).next()
    );
}

/// A single-delivery cassette - the shape every pre-`conversation:` trace
/// has - validates, and carries neither new field.
#[test]
fn a_single_delivery_cassette_validates_without_the_conversation_fields() {
    let validator = validator();
    let cassette = Cassette {
        turns: vec![turn("What is the weather?", "Sunny.", 0)],
        deliveries: Vec::new(),
    };
    let json = serde_json::to_value(&cassette).expect("cassette serializes");
    assert_validates(&validator, &json);

    let text = serde_json::to_string(&cassette).expect("cassette serializes");
    assert!(!text.contains("delivery_index"), "{text}");
    assert!(!text.contains("deliveries"), "{text}");
}

/// A multi-delivery cassette validates with both fields present, and
/// round-trips through the typed model without drifting from the schema.
#[test]
fn a_multi_delivery_cassette_validates_and_round_trips() {
    let validator = validator();
    let cassette = Cassette {
        turns: vec![
            turn("Cancel order A-4471.", "Are you sure?", 0),
            turn("Yes, go ahead.", "Cancelled.", 1),
        ],
        deliveries: vec![
            DeliveryMeta {
                user: "Cancel order A-4471.".into(),
                turn_count: 1,
            },
            DeliveryMeta {
                user: "Yes, go ahead.".into(),
                turn_count: 1,
            },
        ],
    };
    let json = serde_json::to_value(&cassette).expect("cassette serializes");
    assert_validates(&validator, &json);

    let parsed: Cassette = serde_json::from_value(json.clone()).expect("cassette parses");
    assert_eq!(parsed, cassette, "round-trip reproduces the cassette");
    let reserialized = serde_json::to_value(&parsed).expect("cassette serializes");
    assert_eq!(reserialized, json, "round-trip reproduces the bytes");
    assert_validates(&validator, &reserialized);
}

/// Tool calls and the Anthropic dialect are part of the recorded shape, so
/// the schema has to accept them rather than only the happy text path.
#[test]
fn a_tool_calling_anthropic_turn_validates() {
    let validator = validator();
    let mut t = turn("Cancel it.", "", 1);
    t.protocol = "anthropic".into();
    t.request.tools = vec!["cancel_order".into()];
    t.response.stop_reason = Some("tool_use".into());
    t.response.message = Message {
        role: "assistant".into(),
        content: None,
        tool_calls: vec![ToolCall {
            id: "call_1".into(),
            name: "cancel_order".into(),
            arguments: "{\"id\":\"A-4471\"}".into(),
        }],
        tool_call_id: None,
    };
    let cassette = Cassette {
        turns: vec![t],
        deliveries: vec![DeliveryMeta {
            user: "Cancel it.".into(),
            turn_count: 1,
        }],
    };
    let json = serde_json::to_value(&cassette).expect("cassette serializes");
    assert_validates(&validator, &json);
}

/// The schema is an instrument, not documentation: it has to REFUSE the
/// shapes that would mean a broken recording.
#[test]
fn the_schema_refuses_malformed_cassettes() {
    let validator = validator();

    // A turn missing its response is a half-recorded exchange.
    let no_response = serde_json::json!({
        "turns": [{"request": {"model": "gpt-4o", "messages": []}}]
    });
    assert!(validator.validate(&no_response).is_err());

    // delivery_index counts deliveries; a negative one is meaningless.
    let negative_index = serde_json::json!({
        "turns": [{
            "request": {"model": "gpt-4o", "messages": []},
            "response": {"message": {"role": "assistant"}},
            "delivery_index": -1
        }]
    });
    assert!(validator.validate(&negative_index).is_err());

    // A dialect nothing can replay must not validate.
    let unknown_protocol = serde_json::json!({
        "turns": [{
            "protocol": "cohere",
            "request": {"model": "gpt-4o", "messages": []},
            "response": {"message": {"role": "assistant"}}
        }]
    });
    assert!(validator.validate(&unknown_protocol).is_err());

    // DeliveryMeta without its turn_count cannot describe a window.
    let partial_delivery = serde_json::json!({
        "turns": [],
        "deliveries": [{"user": "hi"}]
    });
    assert!(validator.validate(&partial_delivery).is_err());

    // An unknown key is a typo or a field this schema has not caught up
    // with; either way it should be reported, not silently accepted.
    let unknown_key = serde_json::json!({
        "turns": [],
        "delivery": []
    });
    assert!(validator.validate(&unknown_key).is_err());
}
