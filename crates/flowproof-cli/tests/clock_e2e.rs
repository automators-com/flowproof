//! Pinned clock (GAP-P), end to end. A page that renders `new Date()` is
//! recorded with the clock pinned to a fixed past date; replay - which
//! happens on a different real day - must still see the pinned date and
//! timezone. Gated on FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

const TODAY_HTML: &str = include_str!("../../../examples/web/today.html");

const SPEC: &str = r#"
name: Pinned clock
app: web
url: __URL__
browser:
  clock:
    at: "2019-03-14T12:00:00Z"
    timezone: "Europe/Berlin"
steps:
  - assert: page shows 2019-03-14
  - assert: page shows Europe/Berlin
"#;

#[test]
fn a_pinned_clock_is_deterministic_across_the_real_calendar() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping clock E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-clock-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("today.html");
    std::fs::write(&page, TODAY_HTML).expect("page written");
    let trace = dir.join("clock.trace.jsonl");

    let spec = FlowSpec::parse(&SPEC.replace("__URL__", &format!("file://{}", page.display())))
        .expect("spec parses");

    // Record: recording asserts, so this only succeeds if the page really
    // read 2019-03-14 - i.e. the shim pinned the clock at record time.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 2);
    drop(driver);

    // Replay: on whatever real day CI runs, the pinned date and zone must
    // still hold - the clock config travels in the trace header.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "the pinned clock must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
