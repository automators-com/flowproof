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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        steps: FlowSpec::parse(include_str!("../../../examples/web.flow.yaml"))
            .expect("example spec parses")
            .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The app moved on: the recorded selector no longer matches the page.
    let contents = std::fs::read_to_string(&trace_path).expect("trace readable");
    std::fs::write(
        &trace_path,
        contents.replace(
            "\"automation_id\":\"greet\"",
            "\"automation_id\":\"old-greet\"",
        ),
    )
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
    assert!(html.contains("old-greet"), "shows the stale selector");

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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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

/// Session seeding + mid-flow navigation against a real browser: the page
/// boots ALREADY seeded (localStorage is set before any page script runs),
/// `Go to` moves between pages, `Reload` re-renders. Cookies use the same
/// staging path (proven on the mock; file:// pages cannot carry cookies).
#[test]
fn session_seeding_and_navigation_drive_real_pages() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web session E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    std::env::set_var("FLOWPROOF_E2E_PROJECT", "proj-e2e-42");

    let dir = std::env::temp_dir().join("flowproof-web-session-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Page 1 renders the seeded localStorage value AT LOAD TIME — this only
    // works if seeding ran before the page script.
    std::fs::write(
        dir.join("home.html"),
        r#"<!doctype html><html><body><div id="who"></div><script>
            document.getElementById('who').textContent =
                'project: ' + (localStorage.getItem('projectId') || 'MISSING');
        </script></body></html>"#,
    )
    .expect("page 1 written");
    // Page 2 counts its own loads via sessionStorage — reload observable.
    std::fs::write(
        dir.join("settings.html"),
        r#"<!doctype html><html><body><div id="loads"></div><script>
            const n = Number(sessionStorage.getItem('loads') || 0) + 1;
            sessionStorage.setItem('loads', n);
            document.getElementById('loads').textContent =
                'Settings page, load ' + n + ', project ' + (localStorage.getItem('projectId') || 'MISSING');
        </script></body></html>"#,
    )
    .expect("page 2 written");
    let trace_path = dir.join("session.trace.jsonl");

    let mut local_storage = std::collections::BTreeMap::new();
    local_storage.insert(
        "projectId".to_string(),
        "${FLOWPROOF_E2E_PROJECT}".to_string(),
    );
    let spec = flowproof_agent::FlowSpec {
        name: "Seeded session".into(),
        app: "web".into(),
        url: Some(format!("file://{}/home.html", dir.display())),
        redact: vec![],
        connection: None,
        window: None,
        session: Some(flowproof_trace::format::SessionSetup {
            cookies: vec![],
            local_storage,
        }),
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows project: ${FLOWPROOF_E2E_PROJECT}".into(),
            },
            flowproof_agent::SpecStep::Plain(format!(
                "Go to file://{}/settings.html",
                dir.display()
            )),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Settings page, load 1, project ${FLOWPROOF_E2E_PROJECT}".into(),
            },
            flowproof_agent::SpecStep::Plain("Reload the page".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows load 2".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The trace stores the reference, not the resolved project id.
    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(persisted.contains("${FLOWPROOF_E2E_PROJECT}"));
    assert!(!persisted.contains("proj-e2e-42"));

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");

    std::env::remove_var("FLOWPROOF_E2E_PROJECT");
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
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
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

/// Suite mode: `flowproof run <dir>` replays every recorded flow under the
/// directory, keeps going past failures, merges ONE junit.xml, and exits
/// non-zero when any flow failed.
#[test]
fn suite_run_aggregates_flows_and_merges_junit() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web suite E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-suite-e2e");
    std::fs::remove_dir_all(&dir).ok();
    let specs_dir = dir.join("specs");
    std::fs::create_dir_all(specs_dir.join("nested")).expect("temp dirs");

    // Two tiny flows, one nested — recorded through the normal pipeline so
    // their traces sit next to their specs (the suite pairing contract).
    for (rel, name, marker) in [
        ("a-first.flow.yaml", "First flow", "alpha"),
        ("nested/b-second.flow.yaml", "Second flow", "beta"),
    ] {
        let page = dir.join(format!("{marker}.html"));
        std::fs::write(
            &page,
            format!(r#"<!doctype html><html><body><div>{marker} ready</div></body></html>"#),
        )
        .expect("page written");
        let spec_yaml = format!(
            "name: {name}\napp: web\nurl: file://{}\nsteps:\n  - assert: page shows {marker} ready\n",
            page.display()
        );
        let spec_path = specs_dir.join(rel);
        std::fs::write(&spec_path, &spec_yaml).expect("spec written");
        let spec = flowproof_agent::FlowSpec::parse(&spec_yaml).expect("spec parses");
        let trace_path = flowproof_cli::default_trace_path(&spec_path);
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    }

    // Green suite: both flows pass, exit 0, one junit with two testsuites.
    let code = flowproof_cli::run_suite(&specs_dir, false, 0, flowproof_cli::MissingTrace::Error)
        .expect("suite runs");
    assert_eq!(code, flowproof_cli::EXIT_PASS);
    let junit_path = specs_dir.join(".flowproof").join("suite-junit.xml");
    let junit = std::fs::read_to_string(&junit_path).expect("suite junit written");
    assert_eq!(junit.matches("<testsuite name=").count(), 2);
    assert!(junit.contains("failures=\"0\""));

    // Break the SECOND flow's trace: the suite must still run the first,
    // report the failure, and exit non-zero.
    let broken = specs_dir.join("nested").join("b-second.trace.jsonl");
    let contents = std::fs::read_to_string(&broken).expect("trace readable");
    std::fs::write(&broken, contents.replace("beta ready", "beta NEVER")).expect("trace broken");
    let code = flowproof_cli::run_suite(&specs_dir, false, 0, flowproof_cli::MissingTrace::Error)
        .expect("suite runs");
    assert_eq!(code, flowproof_cli::EXIT_FAIL);
    let junit = std::fs::read_to_string(&junit_path).expect("suite junit rewritten");
    assert!(junit.contains("<failure"), "failure recorded: {junit}");
    assert_eq!(
        junit.matches("<testsuite name=").count(),
        2,
        "both flows still ran"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The Playwright-evaluation fixes, against real Chromium: a native
/// <select> commits through React-style change listeners, text anchors
/// resolve an element by its OWN text (a sibling avatar's initials must
/// not fuse with the label), `is disabled`/`is enabled` assert real
/// element state, and `Replace … with …` is one step.
#[test]
fn select_own_text_anchors_and_state_asserts_work() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web eval-fixes E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    const PAGE: &str = r##"<!DOCTYPE html>
<html><body>
  <div class="switcher">
    <span class="avatar">ET</span><button id="team">E2E Test Runner's Team</button>
  </div>
  <label>Role
    <select id="role">
      <option value="">choose...</option>
      <option value="member">Member</option>
      <option value="admin">Administrator</option>
    </select>
  </label>
  <input id="task" value="old name" />
  <button id="save" disabled>Save</button>
  <div id="log"></div>
  <script>
    // React-style: state only changes via the change EVENT, never by
    // direct value writes.
    document.getElementById('role').addEventListener('change', (e) => {
      document.getElementById('log').textContent = 'role committed: ' + e.target.value;
      document.getElementById('save').removeAttribute('disabled');
    });
    document.getElementById('team').addEventListener('click', () => {
      document.getElementById('log').textContent += ' | team switched';
    });
  </script>
</body></html>"##;

    let dir = std::env::temp_dir().join("flowproof-web-evalfix-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("panel.html");
    std::fs::write(&page, PAGE).expect("page written");
    let trace_path = dir.join("panel.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Eval fixes".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        steps: FlowSpec::parse(
            "name: x\napp: web\nurl: x\nsteps:\n\
             - assert: the \"Save\" is disabled\n\
             - Select Administrator from the \"css:#role\" dropdown\n\
             - assert: \"page shows role committed: admin\"\n\
             - assert: the \"Save\" is enabled\n\
             - Click \"E2E Test Runner's Team\"\n\
             - assert: page shows team switched\n\
             - Replace the task field with new name\n\
             - assert: the task field contains new name\n",
        )
        .expect("spec parses")
        .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The team switcher was clicked via its OWN text — the avatar's "ET"
    // must not have fused into the recorded anchor.
    let trace = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(
        trace.contains(r#""text":"E2E Test Runner's Team""#),
        "anchor text recorded without avatar fusion"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "eval-fix flow must pass: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Network mocking against real Chromium: the page fetches an absolute URL
/// on a host that does not exist — only CDP interception can answer it.
/// The mocked body renders into the DOM at record AND replay, proving the
/// rules apply identically on both executions.
#[test]
fn mock_rules_intercept_real_requests_at_record_and_replay() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web mock E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-mock-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("rates.html");
    // No such host resolves: without interception this page shows "offline".
    std::fs::write(
        &page,
        r#"<!doctype html><title>Rates</title><div id="out">loading</div>
<script>
fetch('https://rates.invalid.flowproof.test/api/rates')
  .then(r => r.json())
  .then(d => { document.getElementById('out').textContent = 'rate ' + d.rate + ' via ' + d.source; })
  .catch(() => { document.getElementById('out').textContent = 'offline'; });
</script>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Mocked rates\napp: web\nurl: file://{}\nmock:\n  - url_contains: /api/rates\n    body:\n      rate: 1.23\n      source: mocked\nsteps:\n  - Wait until page shows rate 1.23 via mocked within 10s\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("rates.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("mocked recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "mocked flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Visual assertion v1 against real Chromium: record mints a baseline of
/// the page with the volatile clock masked; replay (a different moment,
/// so the clock text differs) still matches because the same mask is
/// applied. The browser-config viewport keeps capture dimensions stable.
#[test]
fn masked_screenshot_baseline_survives_a_volatile_clock() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web visual E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-visual-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("home.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Home</title>
<main>
  <h1>Stable heading</h1>
  <div id="clock"></div>
  <p>Stable body text under the volatile clock.</p>
  <script>document.getElementById('clock').textContent = 'now ' + Date.now();</script>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Home looks right\napp: web\nurl: file://{}\nbrowser:\n  viewport:\n    width: 800\n    height: 600\nsteps:\n  - assert_screenshot:\n      name: home\n      mask: [\"css:#clock\"]\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("home.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);
    assert!(
        dir.join("home.baselines/home.png").is_file(),
        "baseline minted next to the trace"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "masked visual flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Round-2 browser config against real Chromium: the page boots into an
/// emulated phone viewport (innerWidth 390), sees the overridden
/// user-agent, and — because extra Chrome flags force a private browser —
/// a `--lang=fr-FR` flag reaches `navigator.language`. Record and replay
/// both run the same shape (the config travels in the trace header).
#[test]
fn viewport_user_agent_and_chrome_args_shape_the_real_browser() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web browser-config E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-browser-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("device.html");
    // The meta viewport matters: with mobile emulation and no meta tag,
    // Chrome (like a real phone) lays the page out at its 980px default.
    std::fs::write(
        &page,
        r#"<!doctype html><title>Device</title>
<meta name="viewport" content="width=device-width">
<div id="out"></div>
<script>
  const ua = navigator.userAgent.includes('flowproof-probe') ? 'probe-ua'
    : navigator.userAgent.includes('flag-ua') ? 'flag-ua' : 'default-ua';
  document.getElementById('out').textContent =
    'width ' + window.innerWidth + ', ' + ua + ', touch ' + navigator.maxTouchPoints;
</script>"#,
    )
    .expect("page written");

    // Flow 1: viewport/mobile/touch emulation + tab-level UA override.
    let spec = FlowSpec::parse(&format!(
        "name: Emulated device\napp: web\nurl: file://{}\nbrowser:\n  viewport:\n    width: 390\n    height: 844\n    mobile: true\n    touch: true\n  user_agent: flowproof-probe\nsteps:\n  - assert: page shows width 390, probe-ua, touch 1\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("device.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "device flow must replay: {report:#?}");

    // Flow 2: extra Chrome flags reach the process — the exact shim case
    // from the field report (`--user-agent=playwright` via env wrapper),
    // now first-class. Flags force a private browser for the flow.
    let spec = FlowSpec::parse(&format!(
        "name: Flagged browser\napp: web\nurl: file://{}\nbrowser:\n  args: [\"--user-agent=flowproof flag-ua\"]\nsteps:\n  - assert: page shows flag-ua\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("flagged.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("flagged recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "flagged flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Round-2 input capabilities against real Chromium: a hidden file input
/// behind a wrapping label receives a real file (DOM.setFileInputFiles),
/// a right-click fires the page's contextmenu handler, and a portable
/// `Mod+K` chord lands as Ctrl+K on this OS.
#[test]
fn upload_right_click_and_portable_chord_work_on_a_real_page() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web input E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-input-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let upload_src = dir.join("import.qif");
    std::fs::write(&upload_src, "!Type:Bank\n").expect("upload fixture written");
    let page = dir.join("import.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Import</title>
<main>
  <label>Import file <input type="file" style="display:none"
    onchange="document.getElementById('status').textContent = 'file ' + this.files[0].name"/></label>
  <button oncontextmenu="event.preventDefault();
    document.getElementById('status').textContent = 'menu open'; return false;">Accounts</button>
  <div id="status">waiting</div>
  <script>
    document.addEventListener('keydown', e => {
      if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
        document.getElementById('status').textContent = 'palette';
      }
    });
  </script>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Import a file\napp: web\nurl: file://{}\nsteps:\n  \
         - Upload {} into the \"Import file\" field\n  \
         - assert: page shows file import.qif\n  \
         - Right-click \"Accounts\"\n  \
         - assert: page shows menu open\n  \
         - Press Mod+K\n  \
         - assert: page shows palette\n",
        page.display(),
        upload_src.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("import.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "input flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Round-2 selector fixes against real Chromium, all three in one flow:
/// a wrapping `<label>Name: <input/></label>` resolves as a label query,
/// `Click "Close Account"` lands on a button whose DOM text is
/// "Close account" (case-insensitive fallback rung), and a `page shows`
/// wait sees an icon-only button that exists solely as an aria-label.
#[test]
fn label_association_case_fold_and_aria_names_resolve() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web selector E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-selectors-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("account.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Account</title>
<main>
  <h1>Account settings</h1>
  <label>Name: <input/></label>
  <button aria-label="Open command palette">&#9776;</button>
  <button onclick="document.getElementById('status').textContent =
      'closed for ' + document.querySelector('label input').value">Close account</button>
  <div id="status"></div>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Close the account\napp: web\nurl: file://{}\nsteps:\n  \
         - Type Casey into the \"Name\" field\n  \
         - Click \"Close Account\"\n  \
         - Wait until page shows Open command palette within 5s\n  \
         - assert: page shows closed for Casey\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("account.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "selector flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Field regression (cypress-realworld-app, round 3): EVERY flow that logged
/// in recorded fine and then failed to replay with "Unable to make method
/// calls because underlying connection is closed". The mechanism is a
/// self-inflicted transport death, and it needs two ingredients this test
/// reproduces in order:
///
/// 1. more than 30 seconds of page-level work with no BROWSER-level event,
///    which lets headless_chrome's default `idle_browser_timeout` reap the
///    browser-event listener thread;
/// 2. a real navigation afterwards, which fires `TargetInfoChanged` - a
///    browser-level event the transport can no longer deliver, so it treats
///    it as fatal and shuts the whole connection down permanently.
///
/// A login redirect is exactly that shape, which is why the field suite hit
/// it on every authenticated flow and never on the others. Slow by nature
/// (it must out-wait the idle reaper); that is the bug.
#[test]
fn a_navigation_after_a_long_idle_does_not_kill_the_connection() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web idle-then-navigate E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-idle-nav-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Page two is a separate document, so reaching it is a real navigation
    // rather than a same-document update.
    std::fs::write(
        dir.join("two.html"),
        r#"<!doctype html><html><body><h1>Welcome back</h1></body></html>"#,
    )
    .expect("page two written");
    let page = dir.join("one.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <div id="out">waiting</div>
            <button id="go" onclick="window.location.href = 'two.html'">Continue</button>
            <script>
              // Page-level churn only: no browser-level CDP events at all,
              // so the idle reaper is free to fire while the flow works.
              setTimeout(() => {
                document.getElementById('out').textContent = 'ready to continue';
              }, 35000);
            </script>
        </body></html>"#,
    )
    .expect("page one written");
    let trace_path = dir.join("idle-nav.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Navigate after a long idle".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        steps: vec![
            // Out-waits the 30s default idle timeout.
            flowproof_agent::SpecStep::Plain(
                "Wait until page shows ready to continue within 60s".into(),
            ),
            flowproof_agent::SpecStep::Plain("Press the \"Continue\" button".into()),
            // The read that used to die: first page-level call after the
            // navigation's TargetInfoChanged.
            flowproof_agent::SpecStep::Plain(
                "Wait until page shows Welcome back within 20s".into(),
            ),
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording survives the idle");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "a navigation after an idle period must not kill the transport: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Field regression (cypress-realworld-app, round 3): a settings form whose
/// first field sat below the fold was untestable. The actionability gate
/// hit-tests `elementFromPoint` at the element's centre, but ran BEFORE any
/// scrolling - and outside the viewport that returns null, so the gate
/// reported "obscured (another element would receive the click)" and
/// blocked a click that would have worked. headless_chrome's own
/// `Element::click` starts with `scroll_into_view`, so the gate was asking
/// about a position the click never uses. Cypress and Playwright both
/// scroll before acting; now so does the gate.
#[test]
fn an_element_below_the_fold_is_scrolled_to_rather_than_called_obscured() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web below-the-fold E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-below-fold-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("tall.html");
    // A tall spacer puts the field far below a pinned 500x300 viewport, so
    // "below the fold" is a property of the test, not of the runner.
    std::fs::write(
        &page,
        r#"<!doctype html><html><body style="margin:0">
            <div style="height:1200px">scroll down</div>
            <input id="name" placeholder="Full name" />
            <button id="save" onclick="
              document.getElementById('out').textContent = 'Saved ' + document.getElementById('name').value;
            ">Save</button>
            <div id="out"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("tall.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Field below the fold".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
        redact: vec![],
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: Some(flowproof_trace::format::BrowserSetup {
            viewport: Some(flowproof_trace::format::ViewportSetup {
                width: 500,
                height: 300,
                device_scale_factor: None,
                mobile: None,
                touch: None,
            }),
            user_agent: None,
            args: Vec::new(),
        }),
        steps: vec![
            flowproof_agent::SpecStep::Plain("Type Ada into the name field".into()),
            flowproof_agent::SpecStep::Plain("Press the \"Save\" button".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Saved Ada".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect("recording reaches a field below the fold");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "below-the-fold element must be scrolled to, not called obscured: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
