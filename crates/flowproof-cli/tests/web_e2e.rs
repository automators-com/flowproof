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
    // ...and the ready-to-play whole-run GIF next to them.
    let gif = recording.gif.as_deref().expect("whole-run gif rendered");
    let gif_bytes = std::fs::read(run_dir.join(&recording.dir).join(gif)).expect("gif readable");
    assert!(gif_bytes.starts_with(b"GIF89a"));
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

/// Secret indirection against a real browser: a `${VAR}` password typed
/// into a live page resolves from the environment; neither the trace nor
/// the run artifacts ever contain the value.
#[test]
fn secret_reference_types_real_value_but_never_persists_it() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web secret E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    std::env::set_var("FLOWPROOF_E2E_PW", "s3cret-e2e-value");

    let dir = std::env::temp_dir().join("flowproof-web-secret-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("login.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <input id="pw" type="password" />
            <button id="go" onclick="document.getElementById('done').textContent =
                document.getElementById('pw').value.length >= 8 ? 'accepted' : 'rejected'">Go</button>
            <div id="done"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("login.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Password login".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: vec![
            flowproof_agent::SpecStep::Plain("Type ${FLOWPROOF_E2E_PW} into the pw field".into()),
            flowproof_agent::SpecStep::Plain("Press the go button".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows accepted".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // What was typed is proven by the trace-text assertions below plus the
    // replay's own resolution; the page's length check just gates the flow.
    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(persisted.contains("${FLOWPROOF_E2E_PW}"));
    assert!(
        !persisted.contains("s3cret-e2e-value"),
        "secret value must never reach the trace"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    drop(driver);
    assert!(report.passed, "report: {report:#?}");
    let result_path = report.write_into(&run_dir).expect("artifacts written");
    let artifacts = std::fs::read_to_string(&result_path).expect("result readable");
    assert!(!artifacts.contains("s3cret-e2e-value"));

    std::fs::remove_dir_all(&dir).ok();
    std::env::remove_var("FLOWPROOF_E2E_PW");
}

/// Auto-waiting against a real browser: the page's result text only appears
/// after an async delay — record and replay both wait it out, no sleeps in
/// the spec.
#[test]
fn assertions_wait_for_async_page_updates() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web auto-wait E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-autowait-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("slow.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <button id="start" onclick="
                document.getElementById('out').textContent = 'Generating…';
                setTimeout(() => {
                    document.getElementById('out').textContent = 'Generation complete: 100 rows';
                }, 3000);
            ">Start</button>
            <div id="out"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("slow.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Slow generation".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: vec![
            flowproof_agent::SpecStep::Plain("Press the start button".into()),
            flowproof_agent::SpecStep::Plain(
                "Wait until page shows Generation complete within 15s".into(),
            ),
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let started = std::time::Instant::now();
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording waits");
    assert!(
        started.elapsed() >= std::time::Duration::from_secs(3),
        "record must have actually waited for the async update"
    );
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Text-anchor targeting against a real browser: a page with NO ids at all
/// — elements addressed by placeholder and visible button text, the way
/// real-world apps (and Playwright suites) address them.
#[test]
fn idless_page_is_driven_by_placeholder_and_button_text() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web text-anchor E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-textanchor-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("noids.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <input placeholder="Template name" />
            <button onclick="
                const name = document.querySelector('input').value;
                const div = document.createElement('div');
                div.textContent = 'Created template: ' + name;
                document.body.appendChild(div);
            ">Create template</button>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("noids.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Id-less flow".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: vec![
            flowproof_agent::SpecStep::Plain(
                "Type Customers into the \"Template name\" field".into(),
            ),
            flowproof_agent::SpecStep::Plain("Press the \"Create template\" button".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Created template: Customers".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The trace records text anchors — reviewable exactly as written.
    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(persisted.contains("\"tier\":\"text_anchor\""));
    assert!(persisted.contains("Template name"));

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The assertion vocabulary against a real browser: field values, counts,
/// an element-scoped assert on a toast that only appears AFTER the assert
/// starts (resolution is part of the poll), a negative assert that waits
/// for a deletion to land, and visibility checks.
#[test]
fn assertion_forms_wait_and_verify_on_real_pages() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web assertions E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-assert-forms-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("asserts.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <input id="searchBox" value="prefilled" />
            <div>row one</div><div>row two</div><div>row three</div>
            <div id="conn-row">TestConnection</div>
            <button onclick="
                setTimeout(() => {
                    const t = document.createElement('div');
                    t.id = 'toast';
                    t.textContent = 'Copied to clipboard';
                    document.body.appendChild(t);
                }, 800);
            ">Show toast</button>
            <button onclick="
                setTimeout(() => document.getElementById('conn-row').remove(), 500);
            ">Delete connection</button>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("asserts.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Assertion forms".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "the searchBox field contains prefilled".into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: "page shows row 3 times".into(),
            },
            flowproof_agent::SpecStep::Plain("Press the \"Show toast\" button".into()),
            // #toast does not exist yet when this assert starts polling.
            flowproof_agent::SpecStep::Assert {
                assert: "the \"css:#toast\" shows Copied within 10s".into(),
            },
            flowproof_agent::SpecStep::Plain("Press the \"Delete connection\" button".into()),
            // The row is still on screen for ~500ms after the click.
            flowproof_agent::SpecStep::Assert {
                assert: "page does not show TestConnection within 10s".into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: "the \"css:#conn-row\" is not visible within 5s".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(
        persisted.contains("\"value_not_contains\""),
        "negative encoded"
    );
    assert!(persisted.contains("\"count\":3"), "count encoded");
    assert!(
        persisted.contains("\"element_present\":false"),
        "absence encoded"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The real-world action vocabulary against a real browser: clear-and-retype
/// (fill semantics on a framework-style input), Enter submission, focused
/// typing, a `css:` icon-button target, prefix-matched text anchors, and an
/// ordinal — the forms a Playwright migration leans on.
#[test]
fn keyboard_css_targets_and_ordinals_drive_real_pages() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web actions E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-actions-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("actions.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <form onsubmit="
                event.preventDefault();
                submitted.textContent = 'Submitted: ' + this.querySelector('input').value;
            "><input placeholder="Search box" value="stale text" /></form>
            <input placeholder="Row value" />
            <input placeholder="Row value"
                   oninput="second_row.textContent = 'Second row: ' + this.value" />
            <button data-test="icon-only" onclick="
                const focused = document.createElement('input');
                focused.oninput = () => { focus_sink.textContent = 'Focus got: ' + focused.value; };
                document.body.appendChild(focused);
                focused.focus();
            "></button>
            <button onclick="card.textContent = 'Card opened'"
                >Database — connect Postgres, MySQL and more</button>
            <div id="submitted"></div><div id="second_row"></div>
            <div id="focus_sink"></div><div id="card"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("actions.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Actions vocabulary".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        steps: vec![
            // Fill semantics: clear the prefilled value, retype, Enter.
            // "Submitted: fresh" (not "…stale textfresh") proves the clear.
            flowproof_agent::SpecStep::Plain("Clear the \"Search box\" field".into()),
            flowproof_agent::SpecStep::Plain("Type fresh into the \"Search box\" field".into()),
            flowproof_agent::SpecStep::Plain("Press Enter".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Submitted: fresh".into(),
            },
            // Ordinal targeting: two identical placeholders.
            flowproof_agent::SpecStep::Plain("Type second into the 2nd \"Row value\" field".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Second row: second".into(),
            },
            // css: target for a text-less icon button; it focuses a fresh
            // input — focused typing lands there.
            flowproof_agent::SpecStep::Plain("Click \"css:[data-test='icon-only']\"".into()),
            flowproof_agent::SpecStep::Plain("Type typed-into-focus".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Focus got: typed-into-focus".into(),
            },
            // Prefix match: the card's text goes on beyond "Database".
            flowproof_agent::SpecStep::Plain("Click \"Database\"".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Card opened".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(persisted.contains("\"replace\":true"), "clear encoded");
    assert!(persisted.contains("\"nth\":2"), "ordinal encoded");
    assert!(persisted.contains("\"press_key\""), "press_key encoded");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");
    assert!(!report.degraded, "report: {report:#?}");

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
