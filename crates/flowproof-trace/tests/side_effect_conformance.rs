//! Keeps the `SideEffect` serde type, the side-effect JSON Schema, and the
//! committed falsifiability fixture in agreement: the fixture's lane must
//! validate, every record must parse into the typed model, and a serialize
//! round-trip must reproduce the record and still validate.

use flowproof_trace::side_effect::{capturable_kinds, SideEffect};

const SCHEMA: &str = include_str!("../schema/side-effect-v1.schema.json");
const FIXTURE: &str =
    include_str!("../../../tests/falsifiability/fixtures/side-effect-violation.trace.jsonl");

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is valid JSON");
    jsonschema::validator_for(&schema).expect("schema compiles")
}

#[test]
fn the_fixture_lane_validates_and_its_records_round_trip() {
    let validator = validator();
    let doc: serde_json::Value = serde_json::from_str(FIXTURE).expect("fixture is JSON");
    let lane = doc
        .get("side_effects")
        .expect("fixture carries a side_effects lane");
    assert!(
        validator.validate(lane).is_ok(),
        "fixture lane failed schema validation: {:?}",
        validator.iter_errors(lane).next()
    );

    let effects = lane["effects"].as_array().expect("lane carries effects");
    assert!(!effects.is_empty(), "the fixture is guilty by construction");
    for raw in effects {
        let parsed: SideEffect =
            serde_json::from_value(raw.clone()).expect("record parses into the typed model");
        // Only capturable kinds are ever emitted; the reserved kinds live in
        // the schema's enum, not in a committed lane.
        assert!(
            capturable_kinds().contains(&parsed.kind.as_str()),
            "fixture carries a non-capturable kind: {}",
            parsed.kind
        );
        // Round-trip: what we serialize must reproduce the record exactly
        // and still satisfy the schema.
        let reserialized = serde_json::to_value(&parsed).expect("typed model serializes");
        assert_eq!(reserialized, *raw, "round-trip reproduces the record");
        assert!(
            validator.validate(&reserialized).is_ok(),
            "round-tripped record failed schema validation: {:?}",
            validator.iter_errors(&reserialized).next()
        );
    }
    // The schema is an instrument, not documentation: a kind outside the
    // enum, or an unknown key, is refused.
    let bad_kind = serde_json::json!({"kind": "exec", "at_ms": 1});
    assert!(validator.validate(&bad_kind).is_err());
    let bad_key = serde_json::json!({"kind": "fs_write", "at_ms": 1, "resolved_path": "/etc"});
    assert!(validator.validate(&bad_key).is_err());
}
