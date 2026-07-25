//! `page title is|contains`, end to end in a real browser. The interesting
//! case is not the static title: it is the SPA that sets `document.title`
//! AFTER the route commits, which is why the assertion auto-waits like its
//! `page url` sibling. Gated on FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

/// The title is set late, on a timer, exactly as a router would set it after
/// a data load. A non-waiting assertion would read "Loading" and fail.
const LATE_TITLE_HTML: &str = r#"<!doctype html>
<html>
  <head><title>Loading</title></head>
  <body>
    <h1>Orders</h1>
    <script>
      setTimeout(function () { document.title = 'Orders - Acme Admin'; }, 900);
    </script>
  </body>
</html>
"#;

const TITLE_SPEC: &str = r#"
name: page title assertions
app: web
url: __URL__
steps:
  # Auto-wait: the title is still "Loading" when this step begins.
  - assert: page title is Orders - Acme Admin
  - assert: page title contains Acme
"#;

/// A title that never arrives must fail naming what was actually there.
const WRONG_TITLE_SPEC: &str = r#"
name: wrong page title
app: web
url: __URL__
steps:
  - assert: page title is Invoices
"#;

fn skip() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping page title E2E: set FLOWPROOF_E2E=1 to run it");
        return true;
    }
    false
}

fn spec_for(yaml: &str, page: &std::path::Path) -> FlowSpec {
    FlowSpec::parse(&yaml.replace("__URL__", &format!("file://{}", page.display())))
        .expect("spec parses")
}

fn write_page(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("flowproof-page-title-e2e-{name}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("orders.html");
    std::fs::write(&page, LATE_TITLE_HTML).expect("page written");
    (dir, page)
}

#[test]
fn a_title_set_after_load_is_waited_for_and_replays() {
    if skip() {
        return;
    }
    let (dir, page) = write_page("late");
    let trace = dir.join("title.trace.jsonl");
    let spec = spec_for(TITLE_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 2);
    drop(driver);

    // The expectation travels as its own key, not as surface text: a title
    // is a different reading of the surface, like the url.
    let recorded = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        recorded.contains("\"title_equals\""),
        "the title expectation must travel in the trace: {recorded}"
    );
    assert!(recorded.contains("\"title_contains\""), "{recorded}");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "title assertions must replay: {report:#?}");
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_wrong_title_fails_naming_what_the_page_actually_had() {
    if skip() {
        return;
    }
    let (dir, page) = write_page("wrong");
    let trace = dir.join("wrong.trace.jsonl");
    let spec = spec_for(WRONG_TITLE_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace)
        .expect_err("a title that never arrives must fail");
    let message = err.to_string();
    assert!(
        message.contains("Invoices"),
        "names what was wanted: {message}"
    );
    assert!(
        message.contains("Orders - Acme Admin") || message.contains("Loading"),
        "and what was actually there: {message}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}
