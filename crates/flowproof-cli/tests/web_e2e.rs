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
        redact: vec![],
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
    let (report, run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "web flow must pass: {report:#?}");

    // The replay run carries its recording: per-step ranges + real frames.
    let recording = report.recording.as_ref().expect("run is recorded");
    assert_eq!(recording.steps.len(), report.steps.len());
    for frame in &recording.frames {
        assert!(run_dir.join(&recording.dir).join(&frame.file).exists());
    }
    // The authoring trace references its own recording bundle.
    let (header, steps) = flowproof_replay::load_trace(&trace_path).expect("trace loads");
    let trace_rec = header.recording.expect("trace records its authoring run");
    assert!(dir.join(&trace_rec.dir).is_dir());
    assert!(steps.iter().all(|s| s.artifacts.recording.is_some()));

    std::fs::remove_dir_all(&dir).ok();
}

/// Heal review page against a real browser: an outdated trace produces a
/// before/after page whose frames come from BOTH executions' bundles.
#[test]
fn heal_writes_a_review_page_with_frames_from_both_runs() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web heal-review E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-heal-review-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("web.trace.jsonl");

    let spec = FlowSpec {
        name: "Greet the user".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: FlowSpec::parse(include_str!("../../../examples/web.flow.yaml"))
            .expect("example spec parses")
            .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The app moved on: the recorded selector no longer matches the page.
    let contents = std::fs::read_to_string(&trace_path).expect("trace readable");
    std::fs::write(&trace_path, contents.replace("#greet\"", "#old-greet\""))
        .expect("trace rewritten");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let report = flowproof_agent::heal(&spec, &mut driver, &trace_path).expect("heal runs");
    drop(driver);
    assert!(report.changed, "corrupted selector must be flagged");

    let page_path = report.diff_html.expect("review page written");
    assert_eq!(page_path, dir.join("web.heal.html"));
    let html = std::fs::read_to_string(&page_path).expect("review page readable");
    assert!(html.contains("Before (recorded)"));
    assert!(html.contains("After (proposed)"));
    assert!(html.contains("#old-greet"), "shows the stale selector");

    // Both executions were recorded; the page embeds frames from each
    // bundle, and every referenced frame file really exists next to it.
    let (old_header, _) = flowproof_replay::load_trace(&trace_path).expect("trace loads");
    let (new_header, _) =
        flowproof_replay::load_trace(report.proposed_path.as_ref().expect("proposal written"))
            .expect("proposal loads");
    let old_dir = old_header.recording.expect("original run recorded").dir;
    let new_dir = new_header.recording.expect("proposal run recorded").dir;
    assert_ne!(old_dir, new_dir, "each execution has its own bundle");
    for bundle in [&old_dir, &new_dir] {
        assert!(
            html.contains(&format!("<img src=\"{bundle}/frame-")),
            "page must embed frames from bundle {bundle}"
        );
    }
    for src in html.split("<img src=\"").skip(1) {
        let file = src.split('"').next().expect("img src attr");
        assert!(dir.join(file).is_file(), "referenced frame missing: {file}");
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// Redaction proof against a real browser: a page with a password field and
/// a css-masked region — the PERSISTED frames must show both as solid black.
#[test]
fn persisted_frames_never_contain_masked_data() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web redaction E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-redact-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("login.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body style="background:#fff">
            <input id="user" value="alice" />
            <input id="pw" type="password" value="hunter2" />
            <div id="ssn" style="background:#f00;width:120px;height:40px">123-45-6789</div>
            <button id="go" onclick="document.getElementById('done').textContent='ok'">Go</button>
            <div id="done"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("login.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Login-ish".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![flowproof_driver::RedactionRule::css("#ssn")],
        steps: vec![
            flowproof_agent::SpecStep::Plain("Type bob into the user field".into()),
            flowproof_agent::SpecStep::Plain("Press the go button".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows ok".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");

    // Ground truth: where the masked elements actually are on this page.
    let ssn_rect = driver
        .element_rect(&flowproof_driver::UiaSelector::css("#ssn"))
        .expect("rect query")
        .expect("#ssn on screen");
    let pw_rect = driver.password_rects().expect("password rects")[0];
    drop(driver);

    let (header, _) = flowproof_replay::load_trace(&trace_path).expect("trace loads");
    let bundle = dir.join(header.recording.expect("recorded").dir);
    let mut checked = 0;
    for entry in std::fs::read_dir(&bundle).expect("bundle dir") {
        let path = entry.expect("entry").path();
        let frame = image::open(&path).expect("frame decodes").to_rgba8();
        for &(x, y, w, h) in &[ssn_rect, pw_rect] {
            // Sample the rect interior: every pixel must be the mask fill.
            for (px, py) in [
                (x + 2, y + 2),
                (x + w as i32 / 2, y + h as i32 / 2),
                (x + w as i32 - 3, y + h as i32 - 3),
            ] {
                assert_eq!(
                    *frame.get_pixel(px as u32, py as u32),
                    image::Rgba([0, 0, 0, 255]),
                    "unmasked pixel at {px},{py} in {path:?}"
                );
            }
        }
        checked += 1;
    }
    assert!(checked > 0, "frames were persisted");

    std::fs::remove_dir_all(&dir).ok();
}
