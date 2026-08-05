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
    // s0003 carries an api request body + headers with raw ${VAR} refs;
    // s0002 (headerless GET) proves the backward-compat default path.
    // s0004/s0005 are the text and COUNTED capture readings and s0006 is
    // `set_checked` - the three action types the schema did not list, which
    // is exactly why this fixture has to carry one of each: the enum was
    // wrong for as long as nothing here exercised them.
    assert_eq!(steps, 6, "fixture should contain six steps");
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

const MULTI_FIXTURE: &str = include_str!("fixtures/multi.trace.jsonl");

/// The multi-surface trace shape: a `multi` sentinel header whose `apps`
/// map carries the real surfaces, and steps attributed to a surface by
/// name. The `legacy` surface exercises `command` + `geometry`, which the
/// schema's app object refused before app_info was factored out — a
/// `windows` flow's header failed validation for as long as nothing here
/// carried one. The final step carries NO surface (out-of-band asserts
/// need no UI), proving the field is optional per step, not per trace.
#[test]
fn multi_surface_fixture_lines_parse_and_validate() {
    let validator = validator();
    let mut surfaces = Vec::new();
    for (i, line) in MULTI_FIXTURE
        .lines()
        .filter(|l| !l.trim().is_empty())
        .enumerate()
    {
        let raw: serde_json::Value = serde_json::from_str(line).expect("line is JSON");
        assert!(
            validator.validate(&raw).is_ok(),
            "multi fixture line {} failed schema validation: {:?}",
            i + 1,
            validator.iter_errors(&raw).next()
        );
        let parsed = TraceLine::parse(line).expect("line parses into typed model");
        match &parsed {
            TraceLine::Header(header) => {
                assert_eq!(header.app.name, "multi");
                assert_eq!(header.apps.len(), 3, "gui, portal, legacy");
                let legacy = &header.apps["legacy"];
                assert!(legacy.command.is_some() && legacy.geometry.is_some());
            }
            TraceLine::Step(step) => surfaces.push(step.surface.clone()),
        }
        // Round-trip: what we serialize must still satisfy the schema and
        // reparse identically — surface attribution intact.
        let reserialized = serde_json::to_value(&parsed).expect("typed model serializes");
        assert!(
            validator.validate(&reserialized).is_ok(),
            "round-tripped multi line failed schema validation: {:?}",
            validator.iter_errors(&reserialized).next()
        );
        let reparsed: TraceLine =
            serde_json::from_value(reserialized).expect("round-trip reparses");
        assert_eq!(reparsed, parsed);
    }
    assert_eq!(
        surfaces,
        vec![
            Some("gui".to_string()),
            Some("gui".to_string()),
            Some("portal".to_string()),
            None
        ],
        "steps carry their surface; the out-of-band assert carries none"
    );
}

/// A single-surface trace serializes byte-identically to before `apps`
/// and `surface` existed: the additive fields leave no key behind when
/// unset, so nothing rewrites what correct means for existing cassettes.
#[test]
fn single_surface_traces_serialize_without_the_new_keys() {
    for line in FIXTURE.lines().filter(|l| !l.trim().is_empty()) {
        let parsed = TraceLine::parse(line).expect("line parses");
        let reserialized = serde_json::to_string(&parsed).expect("typed model serializes");
        assert!(
            !reserialized.contains("\"apps\"") && !reserialized.contains("\"surface\""),
            "unset additive fields must leave no key: {reserialized}"
        );
    }
}
