//! End-to-end: the full `record()` pipeline (not just the driver method in
//! isolation - see flowproof-adapters/tests/a11y_capture.rs for that) puts
//! an `a11y` selector first in the ladder for an ordinary web click.
//! Cross-platform, opt-in via FLOWPROOF_E2E=1, same fixture `web_e2e.rs`
//! uses.

use flowproof_agent::FlowSpec;

const GREETER_HTML: &str = include_str!("../../../examples/web/greeter.html");

#[test]
fn a_plain_click_records_an_a11y_selector_first() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping a11y selector E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-a11y-selector-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("web.trace.jsonl");

    let spec = FlowSpec {
        name: "Greet the user".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        connection: None,
        login: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        agent: None,
        tools: Vec::new(),
        mcp: Vec::new(),
        strict: false,
        control: None,
        exports: Default::default(),
        apps: Default::default(),
        steps: FlowSpec::parse(include_str!("../../../examples/web.flow.yaml"))
            .expect("example spec parses")
            .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let contents = std::fs::read_to_string(&trace_path).expect("trace readable");
    // Step 2, "Press the greet button": <button id="greet">Greet</button>
    // has an implicit role and an accessible name from its own text content
    // - no ARIA authored, which is the common case, not the labelled-only
    // one flowproof-adapters/tests/a11y_capture.rs already covers.
    let press_step: serde_json::Value = contents
        .lines()
        .map(|line| serde_json::from_str(line).expect("line is valid JSON"))
        .find(|line: &serde_json::Value| line["intent"].as_str() == Some("Press the greet button"))
        .expect("the press step is in the trace");

    let selectors = press_step["selectors"]
        .as_array()
        .expect("selectors is an array");
    assert!(!selectors.is_empty(), "expected at least one selector");
    assert_eq!(
        selectors[0]["tier"], "a11y",
        "the a11y tier must be tried first: {selectors:#?}"
    );
    assert_eq!(selectors[0]["payload"]["role"], "button");
    assert_eq!(selectors[0]["payload"]["name"], "Greet");

    std::fs::remove_dir_all(&dir).ok();
}

/// H1's mechanism, reproduced end to end rather than argued from source:
/// record against the real fixture, then - same URL, matching a real app
/// redeploy rather than a different environment - overwrite the page so the
/// button's id changes while its VISIBLE TEXT, and so its accessible name,
/// stays the same. `native_id` (css `#greet`) is dead on arrival; the
/// FULL replay must still pass, resolving that step via the `a11y` rung.
#[test]
fn a_renamed_native_id_still_replays_via_the_a11y_rung() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping a11y replay-resolution E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-a11y-replay-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("web.trace.jsonl");

    let spec = FlowSpec {
        name: "Greet the user".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        connection: None,
        login: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        agent: None,
        tools: Vec::new(),
        mcp: Vec::new(),
        strict: false,
        control: None,
        exports: Default::default(),
        apps: Default::default(),
        steps: FlowSpec::parse(include_str!("../../../examples/web.flow.yaml"))
            .expect("example spec parses")
            .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The "redeploy": same path the trace's url points at, new content.
    std::fs::write(
        &page,
        GREETER_HTML.replace(r#"id="greet""#, r#"id="greetBtnV2""#),
    )
    .expect("page rewritten in place");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) = flowproof_replay::run_trace(&trace_path, &mut driver)
        .expect("replay runs (does not itself error)");
    assert!(report.passed, "replay must still pass: {report:#?}");
    let press_step = report
        .steps
        .iter()
        .find(|s| s.intent == "Press the greet button")
        .expect("the press step is in the report");
    assert_eq!(
        press_step.selector_tier.as_deref(),
        Some("a11y"),
        "must resolve via the a11y rung, not degrade through a dead native_id: {press_step:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
