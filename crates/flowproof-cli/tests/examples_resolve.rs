//! Shipped examples must stay honest: every step parses and resolves
//! through the deterministic rules — no live system needed, no model
//! backend. This is the same role `documented_grammar_examples_all_
//! resolve` plays for docs/authoring.md.

use flowproof_agent::{FlowSpec, SuiteManifest};

const FIORI_SPEC: &str = include_str!("../../../examples/fiori/manage-info-records.flow.yaml");
const FIORI_SUITE: &str = include_str!("../../../examples/fiori/suite.yaml");
const CONN_TEST_SPEC: &str = include_str!("../../../examples/api/connection-test.flow.yaml");

#[test]
fn connection_test_example_resolves_with_body_and_headers() {
    let spec = FlowSpec::parse(CONN_TEST_SPEC).expect("example parses");
    assert_eq!(spec.app, "api");
    let actions =
        flowproof_agent::rules::resolve_step(&spec.app, &spec.steps[0]).expect("step resolves");
    let flowproof_agent::rules::ResolvedAction::AssertApi { headers, body, .. } = &actions[0]
    else {
        panic!("expected AssertApi");
    };
    // Raw refs in the resolved action — resolution is probe-time only.
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer ${DM_SESSION_TOKEN}")
    );
    assert_eq!(
        body.as_ref().expect("body present")["connectionString"],
        "${TEST_CONN_STRING}"
    );
}

#[test]
fn fiori_example_resolves_entirely_via_rules() {
    let spec = FlowSpec::parse(FIORI_SPEC).expect("example parses");
    assert_eq!(spec.app, "web", "Fiori is a browser app");
    assert!(
        spec.url
            .as_deref()
            .unwrap_or_default()
            .contains("${FIORI_BASE_URL}"),
        "launch URL stays a ${{VAR}} reference"
    );
    for step in &spec.steps {
        let actions = flowproof_agent::rules::resolve_step(&spec.app, step)
            .unwrap_or_else(|e| panic!("step '{}' must resolve via rules: {e}", step.intent()));
        assert!(
            !actions.is_empty(),
            "step '{}' yields actions",
            step.intent()
        );
    }
}

#[test]
fn fiori_suite_manifest_declares_the_data_leg() {
    let manifest: SuiteManifest = serde_yaml::from_str(FIORI_SUITE).expect("suite.yaml parses");
    let cmd = manifest.env_from.expect("env_from present");
    assert!(
        cmd.contains("datamaker"),
        "data comes from the DataMaker CLI"
    );
    assert!(manifest.env.contains_key("FIORI_BASE_URL"));
}
