//! Shipped examples must stay honest: every step parses and resolves
//! through the deterministic rules — no live system needed, no model
//! backend. This is the same role `documented_grammar_examples_all_
//! resolve` plays for docs/authoring.md.

use flowproof_agent::{FlowSpec, SuiteManifest};

const FIORI_SPEC: &str = include_str!("../../../examples/fiori/manage-info-records.flow.yaml");
const PURCHASE_INFO_RECORDS_SPEC: &str =
    include_str!("../../../examples/fiori/purchase-info-records-report.flow.yaml");
const FIORI_SUITE: &str = include_str!("../../../examples/fiori/suite.yaml");
const CONN_TEST_SPEC: &str = include_str!("../../../examples/api/connection-test.flow.yaml");
/// The npm-path agent quickstart. It is the first example a reader coming
/// from `npx flowproof` runs, so it has to keep parsing.
const AGENT_NODE_SPEC: &str = include_str!("../../../examples/agent-demo/weather-node.flow.yaml");
const AGENT_PY_SPEC: &str = include_str!("../../../examples/agent-demo/weather.flow.yaml");
const DEMO_SPEC: &str = include_str!("../../../scripts/demo/order-status.flow.yaml");

#[test]
fn connection_test_example_resolves_with_body_and_headers() {
    let spec = FlowSpec::parse(CONN_TEST_SPEC).expect("example parses");
    assert_eq!(spec.app.id(), "api");
    let actions =
        flowproof_agent::rules::resolve_step(spec.app.id(), &spec.steps[0]).expect("step resolves");
    let flowproof_agent::rules::ResolvedAction::AssertApi {
        headers,
        body,
        status,
        ..
    } = &actions[0]
    else {
        panic!("expected AssertApi");
    };
    // Raw refs in the resolved action — resolution is probe-time only.
    assert_eq!(
        headers.get("Authorization").map(String::as_str),
        Some("Bearer ${DM_SESSION_TOKEN}")
    );
    let body = body.as_ref().expect("body present");
    assert_eq!(body["connectionString"], "${TEST_CONN_STRING}");
    // Pinned to the real DataMaker /connections/test contract the example
    // mirrors (the api_pipeline mock speaks the same shape): the field is
    // `type`, and an unsupported provider answers 500. Drift fails here.
    assert_eq!(body["type"], "postgres");
    assert_eq!(*status, Some(500));
}

#[test]
fn fiori_example_resolves_entirely_via_rules() {
    let spec = FlowSpec::parse(FIORI_SPEC).expect("example parses");
    assert_eq!(spec.app.id(), "web", "Fiori is a browser app");
    assert!(
        spec.url
            .as_deref()
            .unwrap_or_default()
            .contains("${FIORI_BASE_URL}"),
        "launch URL stays a ${{VAR}} reference"
    );
    for step in &spec.steps {
        let actions = flowproof_agent::rules::resolve_step(spec.app.id(), step)
            .unwrap_or_else(|e| panic!("step '{}' must resolve via rules: {e}", step.intent()));
        assert!(
            !actions.is_empty(),
            "step '{}' yields actions",
            step.intent()
        );
    }
}

/// Unlike `fiori_example_resolves_entirely_via_rules`, this example's header
/// deliberately promises two flagged TODOs left unresolved for `flowproof
/// record` to fill in against the live screen, not guessed here — so this
/// test only proves the file parses, and does not run every step through
/// `rules::resolve_step` (the two TODO steps are plain prose by design and
/// are not expected to resolve).
#[test]
fn purchase_info_records_example_parses_as_a_multi_surface_flow() {
    let spec = FlowSpec::parse(PURCHASE_INFO_RECORDS_SPEC).expect("example parses");
    assert!(
        spec.apps.contains_key("fiori") && spec.apps.contains_key("excel"),
        "flow declares both the fiori and excel surfaces"
    );
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

/// The quickstart's two agent demos must stay runnable and stay TWINS: the
/// docs present them as the same flow in two languages, so a change to one
/// that is not mirrored in the other makes the documentation lie.
#[test]
fn both_agent_demos_resolve_and_assert_the_same_thing() {
    let node = FlowSpec::parse(AGENT_NODE_SPEC).expect("node example parses");
    let py = FlowSpec::parse(AGENT_PY_SPEC).expect("python example parses");

    for spec in [&node, &py] {
        assert_eq!(spec.app.id(), "agent");
        // A mocked tool is what makes replay deterministic here; without a
        // result the demo would depend on a live timestamp.
        let mocked = spec
            .tools
            .iter()
            .find(|t| t.name == "get_weather")
            .expect("get_weather declared");
        assert!(!mocked.result.is_null(), "get_weather must carry a result:");
    }

    // Same trajectory, same assertions - only the runtime differs.
    assert_eq!(
        node.steps.len(),
        py.steps.len(),
        "the two demos must stay the same flow"
    );

    // The Node demo must not need Python, which is the entire reason it
    // exists: the npm install path has to stand on its own.
    let command = node
        .agent
        .as_ref()
        .and_then(|a| a.command.clone())
        .expect("node demo has a command");
    assert!(
        command.starts_with("node "),
        "the npm-path demo must run under node, got: {command}"
    );
    assert!(
        !command.contains("python"),
        "the npm-path demo must not need Python: {command}"
    );
}

/// The two quickstarts are the most-read prose we have and they are now the
/// front door for the npm audience, so their YAML must not drift from the file
/// they claim to quote. A reader who copies a block that no longer parses is
/// the worst possible first experience. Both the README and
/// docs/getting-started.md open on the same shipped example, so both are held
/// to it here.
#[test]
fn the_quickstart_quotes_the_shipped_agent_example_verbatim() {
    const README: &str = include_str!("../../../README.md");
    const DOC: &str = include_str!("../../../docs/getting-started.md");

    // The shipped file, minus its comment header.
    let shipped: String = AGENT_NODE_SPEC
        .lines()
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    // Normalise before comparing. `.gitattributes` now checks everything out
    // with LF, but a test asserting that prose quotes a file should not also be
    // asserting how the checkout was configured - and this one failed on Windows
    // for a day with the message "README.md quotes the node example", because
    // the marker holds "\n" and a CRLF README holds "\r\n", so the find never
    // matched. Belt and braces, and the cheaper half.
    let readme = README.replace("\r\n", "\n");
    let doc = DOC.replace("\r\n", "\n");

    for (name, prose) in [
        ("README.md", readme.as_str()),
        ("docs/getting-started.md", doc.as_str()),
    ] {
        // Pull the fenced block that names the example file.
        let marker = "```yaml\n# examples/agent-demo/weather-node.flow.yaml\n";
        let start = prose
            .find(marker)
            .unwrap_or_else(|| panic!("{name} quotes the node example"))
            + marker.len();
        let block = &prose[start..];
        let block = &block[..block.find("```").expect("fence closes")];

        assert_eq!(
            block.trim(),
            shipped.trim(),
            "{name} has drifted from examples/agent-demo/weather-node.flow.yaml"
        );

        // And it must still parse, so the block a reader copies actually runs.
        FlowSpec::parse(block).expect("the quoted quickstart block parses");
    }
}

/// The flow behind the README's demo GIF. The GIF is a capture of this spec
/// running, so a grammar change that breaks it would silently make the README's
/// hero image a picture of something that no longer works.
#[test]
fn readme_demo_spec_resolves() {
    let spec = FlowSpec::parse(DEMO_SPEC).expect("demo spec parses");
    assert_eq!(spec.app.id(), "agent");

    // Deliberately unmocked: the demo's tool returns a deterministic result, so
    // replay needs no substitution and the run earns no unprotected-tool
    // warning. That is what keeps the captured output to five lines.
    let tool = spec
        .tools
        .iter()
        .find(|t| t.name == "lookup_order")
        .expect("lookup_order declared");
    assert!(
        tool.result.is_null(),
        "the demo tool must stay declaration-only"
    );

    // Paths are relative to the repository root, because that is where the GIF
    // runs its commands and therefore what a reader copies.
    let command = spec
        .agent
        .as_ref()
        .and_then(|a| a.command.clone())
        .expect("demo has a command");
    assert_eq!(command, "python3 scripts/demo/support_agent.py");
}
