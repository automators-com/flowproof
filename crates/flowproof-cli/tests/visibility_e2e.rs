//! `is [not] visible` against a real browser: resolving is only half of it.
//!
//! A hidden element is still in the DOM and still answers every selector, so
//! a presence-only check reported it visible and `is visible` became an
//! assertion that could not fail. These pin both directions - the hidden
//! forms must FAIL, the rendered one must PASS - because a fix that only
//! made the negative form work would leave the false green in place.
//! Gated on FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Visibility</title></head><body>
  <input id="shown" value="here">
  <input id="display-none" style="display:none">
  <input id="visibility-hidden" style="visibility:hidden">
  <div id="clipped" style="display:none"><input id="hidden-parent"></div>
  <span id="zero-size" style="display:inline-block;width:0;height:0;overflow:hidden"></span>
</body></html>
"#;

fn page_url(dir: &std::path::Path) -> String {
    let page = dir.join("visibility.html");
    std::fs::write(&page, PAGE).expect("page written");
    format!("file://{}", page.display())
}

fn spec(url: &str, step: &str) -> FlowSpec {
    FlowSpec::parse(&format!(
        "name: Visibility\napp: web\nurl: {url}\nsteps:\n  - assert: {step}\n"
    ))
    .expect("spec parses")
}

/// The bug: every one of these elements resolves, so `is visible` passed on
/// all of them. Each must now fail, and the error must say the element was
/// found-but-not-rendered rather than sending the reader after a selector
/// that is perfectly correct.
#[test]
fn is_visible_fails_on_an_element_that_resolves_but_is_not_rendered() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping visibility E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-visibility-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let url = page_url(&dir);

    // `#zero-size` is deliberately absent: an element the browser considers
    // rendered but which occupies no box is a different judgement, and this
    // fix takes the browser's own definition rather than inventing one.
    for target in [
        "css:#display-none",
        "css:#visibility-hidden",
        "css:#hidden-parent",
    ] {
        let trace = dir.join("hidden.trace.jsonl");
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        let err = flowproof_agent::record(
            &spec(&url, &format!("the \"{target}\" is visible within 2s")),
            &mut driver,
            &trace,
        )
        .expect_err(&format!("{target} is hidden - `is visible` must fail"));
        let message = err.to_string();
        assert!(
            message.contains("not rendered"),
            "the failure must distinguish hidden from absent for {target}: {message}"
        );
        drop(driver);
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The other direction, so the fix cannot be "call everything invisible":
/// a rendered element still passes `is visible`, a hidden one satisfies
/// `is not visible` (it IS gone as far as the user is concerned), and both
/// survive a replay.
#[test]
fn a_rendered_element_is_visible_and_a_hidden_one_is_not() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping visibility E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-visibility-ok-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let url = page_url(&dir);
    let trace = dir.join("visible.trace.jsonl");

    let spec = FlowSpec::parse(&format!(
        "name: Visibility\napp: web\nurl: {url}\nsteps:\n  \
         - assert: the \"css:#shown\" is visible\n  \
         - assert: the \"css:#display-none\" is not visible\n  \
         - assert: the \"css:#never-existed\" is not visible\n"
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 3);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "visibility must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
