//! Keeps the serde types, the JSON Schema, and the fixture trace in
//! agreement: every fixture line must parse into the typed model, validate
//! against the schema, and survive a serialize round-trip that still
//! validates.

use flowproof_trace::TraceLine;

const SCHEMA: &str = include_str!("../schema/trace-v1.schema.json");
const FIXTURE: &str = include_str!("fixtures/sample.trace.jsonl");

fn validator() -> jsonschema::Validator {
    let schema: serde_json::Value = serde_json::from_str(SCHEMA).expect("schema is valid JSON");
    jsonschema::validator_for(&schema).expect("schema compiles")
}

#[test]
fn fixture_lines_parse_and_validate() {
    let validator = validator();
    let mut lines = FIXTURE.lines().filter(|l| !l.trim().is_empty());

    let header_line = lines.next().expect("fixture has a header line");
    let header = TraceLine::parse(header_line).expect("header parses");
    assert!(matches!(header, TraceLine::Header(_)));

    let mut steps = 0;
    for line in std::iter::once(header_line).chain(lines) {
        let raw: serde_json::Value = serde_json::from_str(line).expect("line is JSON");
        assert!(
            validator.validate(&raw).is_ok(),
            "fixture line failed schema validation: {:?}",
            validator.iter_errors(&raw).next()
        );

        let parsed = TraceLine::parse(line).expect("line parses into typed model");
        if matches!(parsed, TraceLine::Step(_)) {
            steps += 1;
        }

        // Round-trip: what we serialize must still satisfy the schema.
        let reserialized = serde_json::to_value(&parsed).expect("typed model serializes");
        assert!(
            validator.validate(&reserialized).is_ok(),
            "round-tripped line failed schema validation: {:?}",
            validator.iter_errors(&reserialized).next()
        );
        let reparsed: TraceLine =
            serde_json::from_value(reserialized).expect("round-trip reparses");
        assert_eq!(reparsed, parsed);
    }
    assert_eq!(steps, 2, "fixture should contain two steps");
}

#[test]
fn unsupported_version_is_rejected() {
    let bad = FIXTURE
        .lines()
        .next()
        .expect("header line")
        .replace("\"version\":1", "\"version\":99");
    assert!(TraceLine::parse(&bad).is_err());
}
