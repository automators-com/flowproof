//! End-to-end: records and replays a browser flow against real headless
//! Chromium. Cross-platform (this is the E2E that runs on ubuntu CI), opt-in
//! via FLOWPROOF_E2E=1; the Chromium binary comes from the CHROME env var or
//! auto-detection.

use flowproof_agent::FlowSpec;

const GREETER_HTML: &str = include_str!("../../../examples/web/greeter.html");

#[test]
fn records_and_replays_a_browser_flow() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("web.trace.jsonl");

    let spec = FlowSpec {
        name: "Greet the user".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        steps: FlowSpec::parse(include_str!("../../../examples/web.flow.yaml"))
            .expect("example spec parses")
            .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary =
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    assert_eq!(summary.steps, 3);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let report = flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "web flow must pass: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
