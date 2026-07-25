//! iframe-scoped assertions, end to end in a real browser. The fixture puts
//! the SAME text on the page and inside the frame, so every test here is
//! really one question: does the frame scope FENCE the read, or does it leak
//! to the main document and pass on the wrong element? Gated on
//! FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

const FRAMES_HTML: &str = include_str!("../../../examples/web/frames.html");

/// Reads inside the frame, by title and by `css:`, plus the fence: the page
/// says "Total 10.00" and the frame says "Total 42.00", so a scoped read of
/// 42.00 and an unscoped read of 10.00 can only both pass if the scope is
/// real.
const FRAMES_SPEC: &str = r#"
name: iframe scoped assertions
app: web
url: __URL__
steps:
  - assert: the "css:#total" in the iframe "checkout" shows Total 42.00
  - assert: the "css:#total" shows Total 10.00
  - assert: the "css:.status" inside the iframe "checkout" shows Card accepted
  - assert: the "css:.status" in the iframe "receipt" shows Receipt pending
  - assert: the "css:#total" in the iframe "css:iframe[title=checkout]" shows Total 42.00
  # An input inside the frame exposes its VALUE, like anywhere else.
  - assert: the "coupon" in the iframe "checkout" shows SAVE20
"#;

/// The fence, stated as its own failing assertion: `.status` exists in BOTH
/// frames, so reading the receipt frame must not see the checkout frame's
/// text.
const WRONG_FRAME_SPEC: &str = r#"
name: iframe fence
app: web
url: __URL__
steps:
  - assert: the "css:.status" in the iframe "receipt" shows Card accepted
"#;

/// A frame that is not on the page must say so, and name what IS there.
const MISSING_FRAME_SPEC: &str = r#"
name: missing iframe
app: web
url: __URL__
steps:
  - assert: the "css:.status" in the iframe "invoice" shows anything
"#;

fn skip() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping iframe E2E: set FLOWPROOF_E2E=1 to run it");
        return true;
    }
    false
}

fn spec_for(yaml: &str, page: &std::path::Path) -> FlowSpec {
    FlowSpec::parse(&yaml.replace("__URL__", &format!("file://{}", page.display())))
        .expect("spec parses")
}

fn write_page(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("flowproof-iframe-e2e-{name}"));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("frames.html");
    std::fs::write(&page, FRAMES_HTML).expect("page written");
    (dir, page)
}

#[test]
fn framed_assertions_read_inside_the_frame_and_replay() {
    if skip() {
        return;
    }
    let (dir, page) = write_page("happy");
    let trace = dir.join("frames.trace.jsonl");
    let spec = spec_for(FRAMES_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 6);
    drop(driver);

    // The frame travels as written, and the inner keys are PREFIXED so an
    // engine without this rung sees an empty selector and fails loudly
    // rather than resolving the inner target against the main document.
    let recorded = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        recorded.contains("\"kind\":\"framed\""),
        "the framed rung must be recorded: {recorded}"
    );
    assert!(
        recorded.contains("\"frame\":\"checkout\""),
        "the frame travels as written: {recorded}"
    );
    assert!(
        recorded.contains("\"inner_css\"") || recorded.contains("\"inner_text\""),
        "inner keys must be prefixed: {recorded}"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "framed assertions must replay: {report:#?}");
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_frame_scope_does_not_leak_to_the_other_frame() {
    if skip() {
        return;
    }
    let (dir, page) = write_page("fence");
    let trace = dir.join("fence.trace.jsonl");
    let spec = spec_for(WRONG_FRAME_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace)
        .expect_err("reading the wrong frame must fail");
    let message = err.to_string();
    // The receipt frame really was read: its own text is what came back,
    // NOT the checkout frame's and NOT the page's. A fence that leaked
    // would have reported a pass instead of this mismatch.
    assert!(
        message.contains("Receipt pending"),
        "the failure must show what the RECEIPT frame actually said: {message}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_frame_names_the_frames_that_are_there() {
    if skip() {
        return;
    }
    let (dir, page) = write_page("missing");
    let trace = dir.join("missing.trace.jsonl");
    let spec = spec_for(MISSING_FRAME_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err =
        flowproof_agent::record(&spec, &mut driver, &trace).expect_err("a missing frame must fail");
    let message = err.to_string();
    assert!(
        message.contains("invoice"),
        "the failure names the frame that was asked for: {message}"
    );
    assert!(
        message.contains("checkout") || message.contains("receipt"),
        "and names the frames that ARE there: {message}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}
