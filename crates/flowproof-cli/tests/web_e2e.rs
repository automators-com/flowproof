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
    let summary =
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    assert_eq!(summary.steps, 3);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    // GIF assembly is opt-in since it became a `--video` flag; the default is
    // keyframes only. This test asserts the whole-run GIF renders, so it has
    // to ask for one — running with defaults made it expect an artifact
    // nothing had been told to produce.
    let (report, run_dir) = flowproof_replay::run_trace_with_options(
        &trace_path,
        &mut driver,
        flowproof_driver::RecordingOptions {
            video: true,
            ..Default::default()
        },
    )
    .expect("replay runs");
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
    // Both pages come from ONE http origin, not from `file://`. The
    // sessionStorage load counter below has to survive a reload, and
    // file documents are opaque origins in Chrome, so their storage is not
    // reliably carried across one - which made this test fail
    // intermittently on CI for a reason unrelated to what it asserts.
    const HOME: &str = r#"<!doctype html><html><body><div id="who"></div><script>
            document.getElementById('who').textContent =
                'project: ' + (localStorage.getItem('projectId') || 'MISSING');
        </script></body></html>"#;
    const SETTINGS: &str = r#"<!doctype html><html><body><div id="loads"></div><script>
            const n = Number(sessionStorage.getItem('loads') || 0) + 1;
            sessionStorage.setItem('loads', n);
            document.getElementById('loads').textContent =
                'Settings page, load ' + n + ', project ' + (localStorage.getItem('projectId') || 'MISSING');
        </script></body></html>"#;
    let base = serve_site(&[("/home", HOME), ("/settings", SETTINGS)]);
    let trace_path = dir.join("session.trace.jsonl");

    let mut local_storage = std::collections::BTreeMap::new();
    local_storage.insert(
        "projectId".to_string(),
        "${FLOWPROOF_E2E_PROJECT}".to_string(),
    );
    let spec = flowproof_agent::FlowSpec {
        name: "Seeded session".into(),
        app: "web".into(),
        url: Some(format!("{base}/home")),
        redact: vec![],
        connection: None,
        login: None,
        window: None,
        session: Some(flowproof_agent::SessionRef::Inline(
            flowproof_trace::format::SessionSetup {
                cookies: vec![],
                local_storage,
            },
        )),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows project: ${FLOWPROOF_E2E_PROJECT}".into(),
            },
            flowproof_agent::SpecStep::Plain(format!("Go to {base}/settings")),
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

/// Targeted typing means FILL: the field ends up reading the text exactly,
/// whatever it held. Focused typing stays raw keystrokes and appends.
///
/// The distinction was learned the expensive way. A correction typed 800 -
/// the right value - into a payload field still holding the refused 9000,
/// and the page saw 9000800. The value was right and the field was wrong,
/// which is a bad way to lose a recording. The page echoes the field's
/// value in brackets, so the assertions are exact: under append the log
/// would read `[draftreplacement]`, and `page shows notes: [replacement]`
/// fails.
#[test]
fn targeted_typing_fills_and_focused_typing_appends() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web fill-semantics E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    const PAGE: &str = r##"<!DOCTYPE html>
<html><body>
  <input id="notes" value="draft" />
  <div id="log">notes: [draft]</div>
  <script>
    document.getElementById('notes').addEventListener('input', (e) => {
      document.getElementById('log').textContent = 'notes: [' + e.target.value + ']';
    });
  </script>
</body></html>"##;

    let dir = std::env::temp_dir().join("flowproof-web-fill-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("fill.html");
    std::fs::write(&page, PAGE).expect("page written");
    let trace_path = dir.join("fill.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Fill semantics".into(),
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
        steps: FlowSpec::parse(
            "name: x\napp: web\nurl: x\nsteps:\n\
             - Type replacement into the notes field\n\
             - assert: \"page shows notes: [replacement]\"\n\
             - Type !\n\
             - assert: \"page shows notes: [replacement!]\"\n",
        )
        .expect("spec parses")
        .steps,
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "fill-semantics flow must pass: {report:#?}");

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

/// A real `dblclick` against real Chromium: the driver's CDP double-click
/// (two press/release pairs, the second at click_count 2) makes the page
/// emit a DOM `dblclick`, which flips a status marker the flow then asserts.
/// A plain click must NOT trigger it, which is what makes this a genuine
/// double-click and not a click in disguise.
#[test]
fn double_click_fires_a_real_dblclick_on_a_real_page() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web double-click E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-dblclick-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("dblclick.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Double click</title>
<main>
  <button id="target"
    onclick="document.getElementById('status').textContent = 'single'"
    ondblclick="document.getElementById('status').textContent = 'opened'">Open</button>
  <div id="status">waiting</div>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Double click a button\napp: web\nurl: file://{}\nsteps:\n  \
         - Double-click \"Open\"\n  \
         - assert: page shows opened\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("dblclick.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "double-click flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A real hover against real Chromium reveals a CSS `:hover` menu. The menu
/// is `display:none` by default and shown only by `.trigger:hover + .menu`,
/// so its text is absent from the surface until the driver's single CDP
/// `mouseMoved` parks the pointer on the trigger. The test proves BOTH
/// directions: without the hover step the revealed text is absent (a control
/// flow asserts `page does not show`), and with it the revealed text appears.
/// A no-op or a click-in-disguise (which moves the pointer off after
/// releasing) would leave `:hover` inactive and the menu hidden, failing.
#[test]
fn hover_reveals_a_css_hover_menu_on_a_real_page() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web hover E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-hover-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("hover.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Hover menu</title>
<style>
  .menu { display: none; }
  .trigger:hover + .menu { display: block; }
</style>
<main>
  <button class="trigger">Menu</button>
  <div class="menu">Reports</div>
</main>"#,
    )
    .expect("page written");

    // Control: WITHOUT any hover, the CSS `:hover` menu stays hidden, so its
    // text never reaches the surface. This is what proves the hover (not the
    // page load) is what reveals it.
    let control = FlowSpec::parse(&format!(
        "name: Menu hidden by default\napp: web\nurl: file://{}\nsteps:\n  \
         - assert: page does not show Reports\n",
        page.display()
    ))
    .expect("control spec parses");
    let control_trace = dir.join("control.trace.jsonl");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&control, &mut driver, &control_trace).expect("control records");
    drop(driver);
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (control_report, _run_dir) =
        flowproof_replay::run_trace(&control_trace, &mut driver).expect("control replays");
    assert!(
        control_report.passed,
        "without a hover the revealed text must be absent: {control_report:#?}"
    );

    // With the hover, the menu opens and its text shows.
    let spec = FlowSpec::parse(&format!(
        "name: Hover reveals a menu\napp: web\nurl: file://{}\nsteps:\n  \
         - Hover over \"Menu\"\n  \
         - assert: page shows Reports\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("hover.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "hover flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Hover-then-click against real Chromium: hovering the menu reveals a
/// submenu button (shown on `mouseover`, hidden again by a `mouseout` that
/// leaves the menu), and the NEXT step clicks the revealed button. This
/// pins the persistence semantic: the engine synthesizes no pointer
/// movement between steps, so any engine-injected move would fire the
/// mouseout, collapse the submenu, and fail the click.
#[test]
fn hover_reveals_a_submenu_the_next_step_can_click() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web hover-submenu E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-hover-submenu-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("submenu.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Hover submenu</title>
<main>
  <div id="menu">
    <button id="trigger">Menu</button>
    <button id="submenu" style="display:none"
      onclick="document.getElementById('status').textContent = 'submenu clicked'">Reports</button>
  </div>
  <div id="status">waiting</div>
  <script>
    const menu = document.getElementById('menu');
    const submenu = document.getElementById('submenu');
    menu.addEventListener('mouseover', () => {
      submenu.style.display = 'inline-block';
    });
    menu.addEventListener('mouseout', (e) => {
      // The standard menu pattern: only a move that LEAVES the menu
      // (including its submenu) collapses it.
      if (!menu.contains(e.relatedTarget)) {
        submenu.style.display = 'none';
      }
    });
  </script>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Open a hover submenu\napp: web\nurl: file://{}\nsteps:\n  \
         - Hover over \"Menu\"\n  \
         - Click \"Reports\"\n  \
         - assert: page shows submenu clicked\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("submenu.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "hover-submenu flow must replay: {report:#?}");

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
        login: None,
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
            clock: None,
            random: None,
        }),
        agent: None,
        tools: Vec::new(),
        mcp: Vec::new(),
        strict: false,
        control: None,
        exports: Default::default(),
        apps: Default::default(),
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

/// The web-only assertion + action family (GAP-E sibling): DOM attributes,
/// computed style, and Scroll. Records and replays against real Chrome so the
/// adapter's `getAttribute` / `getComputedStyle` / scroll paths are exercised
/// end to end. Gated behind FLOWPROOF_E2E like every browser test here.
#[test]
fn attribute_style_and_scroll_record_and_replay() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web attribute/style/scroll E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-attr-style-scroll-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("checks.html");
    // A download link (present-empty attribute), a crimson amount, and a
    // scrollable list whose last row starts below the fold.
    std::fs::write(
        &page,
        r#"<!doctype html><html><body style="margin:0">
            <a id="dl" href="/report.csv" download>Export</a>
            <span id="amount" style="color: crimson">-100.00</span>
            <div id="list" style="height:120px;overflow:auto">
              <div style="height:600px">rows</div>
              <div id="last">Last row</div>
            </div>
            <div style="height:1500px">page spacer</div>
            <div id="footer">Footer</div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("checks.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "Attribute, style, and scroll".into(),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: r#"the "css:#dl" has attribute download"#.into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: r#"the "css:#dl" attribute href is /report.csv"#.into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: r#"the "css:#dl" does not have attribute hidden"#.into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: r#"the "css:#amount" style color is rgb(220, 20, 60)"#.into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: r#"the "css:#amount" style color is not green"#.into(),
            },
            flowproof_agent::SpecStep::Plain(r#"Scroll the "css:#list" to the bottom"#.into()),
            flowproof_agent::SpecStep::Plain(r#"Scroll "css:#last" into view"#.into()),
            flowproof_agent::SpecStep::Plain("Scroll to the bottom".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Footer".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect("recording the attribute/style/scroll flow succeeds");
    assert_eq!(summary.steps, 9);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "attribute/style/scroll flow must replay green: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// `assert_no_secret_leak` on a real browser flow: a page whose SURFACE TEXT
/// contains the resolved `${SECRET}` fails the record. The store-guard scans
/// the corpus (surface text at each step boundary) before minting, so the
/// leaked value never reaches disk and NO trace is written. The failure names
/// the variable and the step, never the value.
#[test]
fn a_secret_in_web_surface_text_fails_the_record_and_mints_no_trace() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web secret-leak E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let secret = "s3cret-portal-dsn-value";
    std::env::set_var("FLOWPROOF_E2E_LEAK_PW", secret);

    let dir = std::env::temp_dir().join("flowproof-web-secret-leak-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("leaky.html");
    // The page renders the secret straight into visible surface text: the
    // exact leak this control catches.
    std::fs::write(
        &page,
        format!(
            r#"<!doctype html><html><body>
                <h1>Welcome</h1>
                <pre id="err">connection failed: {secret}</pre>
            </body></html>"#
        ),
    )
    .expect("page written");
    let trace_path = dir.join("leaky.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "DB password must not surface".into(),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Welcome".into(),
            },
            flowproof_agent::SpecStep::AssertNoSecretLeak {
                assert_no_secret_leak: vec!["${FLOWPROOF_E2E_LEAK_PW}".into()],
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect_err("a leaked secret must fail the record");
    drop(driver);
    let message = err.to_string();

    assert!(
        message.contains("${FLOWPROOF_E2E_LEAK_PW}"),
        "names the var: {message}"
    );
    assert!(message.contains("step 2"), "names the step: {message}");
    assert!(
        message.contains("surface text at a step boundary"),
        "names the corpus element: {message}"
    );
    assert!(
        !message.contains(secret),
        "message must not leak the value: {message}"
    );
    assert!(
        !trace_path.exists(),
        "a leak must mint no trace; {} exists",
        trace_path.display()
    );

    std::fs::remove_dir_all(&dir).ok();
    std::env::remove_var("FLOWPROOF_E2E_LEAK_PW");
}

/// The clean counterpart: the same secret is declared but never appears in the
/// page's surface text, so record mints a trace whose bytes never contain the
/// value, and replay passes deterministically re-scanning the absent secret.
#[test]
fn a_clean_web_flow_records_and_replays_with_the_secret_absent() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web clean-secret E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let secret = "s3cret-portal-dsn-value";
    std::env::set_var("FLOWPROOF_E2E_CLEAN_PW", secret);

    let dir = std::env::temp_dir().join("flowproof-web-secret-clean-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("clean.html");
    // No secret anywhere in the surface text.
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <h1>Welcome</h1>
            <pre id="err">connection healthy</pre>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("clean.trace.jsonl");

    let spec = flowproof_agent::FlowSpec {
        name: "DB password stays contained".into(),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Welcome".into(),
            },
            flowproof_agent::SpecStep::AssertNoSecretLeak {
                assert_no_secret_leak: vec!["${FLOWPROOF_E2E_CLEAN_PW}".into()],
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("clean flow records");
    drop(driver);

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(
        !trace.contains(secret),
        "the value must never reach the trace"
    );

    // Replay through the scanning path: the secret is re-scanned and absent,
    // so the flow passes deterministically on an unchanged page.
    let scan = flowproof_replay::SecretScan {
        assertions: spec.secret_leak_assertions(),
    };
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, run_dir) =
        flowproof_replay::run_trace_with_secret_scan(&trace_path, &mut driver, &scan)
            .expect("replay runs");
    drop(driver);
    assert!(report.passed, "clean web flow must replay: {report:#?}");
    let result_path = report.write_into(&run_dir).expect("artifacts written");
    let artifacts = std::fs::read_to_string(&result_path).expect("result readable");
    assert!(
        !artifacts.contains(secret),
        "the value must never reach the run artifacts"
    );

    std::fs::remove_dir_all(&dir).ok();
    std::env::remove_var("FLOWPROOF_E2E_CLEAN_PW");
}

/// Native dialogs (GAP): a folded-in dialog suffix arms a one-shot handler
/// BEFORE the trigger, answers the dialog on the listener thread, and
/// verifies it fired as declared. Accept lets the page proceed, dismiss does
/// not, and a prompt reply reaches the page - all against real headless
/// Chromium, recorded and replayed.
#[test]
fn native_dialogs_arm_and_verify() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web dialog E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-dialog-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("dialogs.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <button onclick="del.textContent = window.confirm('Delete this?') ? 'Deleted' : 'still-here'">Delete</button>
            <button onclick="keep.textContent = window.confirm('Discard changes?') ? 'discarded' : 'Kept'">Keep</button>
            <button onclick="ren.textContent = 'Renamed: ' + (window.prompt('New name?') || 'none')">Rename</button>
            <div id="del"></div><div id="keep"></div><div id="ren"></div>
        </body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("dialogs.trace.jsonl");

    let spec = FlowSpec {
        name: "Native dialogs".into(),
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
        steps: vec![
            // Accept a confirm: the page proceeds (marker shows "Deleted").
            flowproof_agent::SpecStep::Plain(
                "Click \"Delete\", accepting the \"Delete this?\" dialog".into(),
            ),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Deleted".into(),
            },
            // Dismiss a confirm: the page does NOT proceed (marker "Kept",
            // never "discarded").
            flowproof_agent::SpecStep::Plain("Click \"Keep\", dismissing the dialog".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Kept".into(),
            },
            flowproof_agent::SpecStep::Assert {
                assert: "page does not show discarded".into(),
            },
            // Answer a prompt: the supplied reply reaches the page.
            flowproof_agent::SpecStep::Plain(
                "Press the \"Rename\" button, answering the prompt with \"Fable\"".into(),
            ),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows Renamed: Fable".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // The dialog folds into the trigger action's params, matched `contains`.
    let persisted = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(persisted.contains("\"dialog\""), "dialog encoded in trace");
    assert!(
        persisted.contains("\"disposition\":\"accept\""),
        "accept encoded"
    );
    assert!(
        persisted.contains("\"disposition\":\"dismiss\""),
        "dismiss encoded"
    );
    assert!(
        persisted.contains("\"match\":\"contains\""),
        "match mode encoded"
    );
    // Value-free: the prompt reply travels as authored input, and here it is
    // a literal, so it appears; a ${VAR} would appear only as the reference.
    assert!(persisted.contains("\"reply\":\"Fable\""), "reply encoded");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "report: {report:#?}");
    assert!(!report.degraded, "report: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The safety net (GAP): a step that triggers an UNDECLARED dialog must FAIL
/// deterministically with "an unexpected dialog opened", NOT hang on the
/// unanswered dialog. A watchdog thread enforces a hard timeout, so a
/// regression that reintroduces the hang fails this test instead of blocking
/// the suite forever.
#[test]
fn undeclared_dialog_fails_and_does_not_hang() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web undeclared-dialog E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let dir = std::env::temp_dir().join("flowproof-web-undeclared-dialog-e2e");
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        let page = dir.join("undeclared.html");
        // The click fires a confirm the step never declares.
        std::fs::write(
            &page,
            r#"<!doctype html><html><body>
                <button onclick="window.confirm('Surprise!'); done.textContent = 'proceeded'">Danger</button>
                <div id="done"></div>
            </body></html>"#,
        )
        .expect("page written");
        let trace_path = dir.join("undeclared.trace.jsonl");

        let spec = FlowSpec {
            name: "Undeclared dialog".into(),
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
            // A PLAIN click, no dialog suffix: the dialog is undeclared.
            steps: vec![flowproof_agent::SpecStep::Plain("Click \"Danger\"".into())],
        };

        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        let result = flowproof_agent::record(&spec, &mut driver, &trace_path);
        std::fs::remove_dir_all(&dir).ok();
        let _ = tx.send(result.map_err(|e| e.to_string()));
    });

    // Hard timeout: the dialog must be dismissed and the step failed well
    // within this bound. A hang would leave the recv waiting.
    match rx.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(Ok(_)) => panic!("recording should FAIL on an undeclared dialog, but it succeeded"),
        Ok(Err(msg)) => {
            assert!(
                msg.contains("an unexpected dialog opened"),
                "expected the unexpected-dialog message, got: {msg}"
            );
        }
        Err(_) => panic!("undeclared dialog HUNG the step: no failure within the timeout"),
    }
}

/// A classic form submit button is a VOID element: `<input type="submit"
/// value="Login">` has no text node and no aria-label, so its accessible
/// name is the `value` attribute (HTML-AAM). Before the value-anchor rung
/// this element was unreachable by text and recording failed with
/// ElementNotFound; now `Press the "Login" button` resolves it, really
/// presses it against real Chromium, and the page's onsubmit proves the
/// submit fired.
#[test]
fn submit_input_is_pressed_by_its_value_on_a_real_form() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web submit-input E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-submit-input-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("login.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Login</title>
<main>
  <form onsubmit="document.getElementById('status').textContent = 'submitted'; return false">
    <input type="submit" value="Login">
  </form>
  <div id="status">waiting</div>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Submit a classic form\napp: web\nurl: file://{}\nsteps:\n  \
         - Press the \"Login\" button\n  \
         - assert: page shows submitted\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("login.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "submit-input flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// `appears 0 times` asserts ABSENCE: zero elements match, and that is the
/// pass condition. The recorder's up-front element_exists probe used to
/// fire ElementNotFound before the count logic ran; AssertCount now sits
/// in the assertions-do-their-own-waiting gate, so record and replay both
/// pass on a real page where the selector matches nothing.
#[test]
fn appears_zero_times_passes_on_a_real_page() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web zero-count E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-zero-count-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("login.html");
    std::fs::write(
        &page,
        r#"<!doctype html><title>Login</title>
<main>
  <form onsubmit="document.getElementById('status').textContent = 'submitted'; return false">
    <input type="submit" value="Login">
  </form>
  <div id="status">waiting</div>
</main>"#,
    )
    .expect("page written");

    let spec = FlowSpec::parse(&format!(
        "name: Nothing matches\napp: web\nurl: file://{}\nsteps:\n  \
         - assert: the \"css:.gone\" appears 0 times\n  \
         - assert: the \"css:form input\" appears 1 time\n",
        page.display()
    ))
    .expect("spec parses");
    let trace_path = dir.join("zero.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary =
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    assert_eq!(summary.steps, 2);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "zero-count flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Seeding is a one-time fixture, not an invariant: a flow that seeds a
/// cart, mutates it through the UI, then navigates must KEEP the mutation.
/// The init script reruns on every document (CDP semantics), so without
/// the sessionStorage seed-once sentinel the navigation re-seeds and
/// silently resets the cart to "[4]" - failing the final assert.
/// Serve several pages from ONE loopback port, which is ONE origin, so
/// localStorage and sessionStorage are shared across them exactly as they
/// are for a real site. Routes are matched on the request path.
///
/// This exists because `file://` is not a usable substrate for storage
/// tests: Chrome treats file documents as opaque origins, so sessionStorage
/// is not reliably carried across a reload and localStorage is not reliably
/// shared between two files. A test written on file:// can therefore fail
/// for a reason that has nothing to do with the behaviour it asserts.
fn serve_site(routes: &'static [(&'static str, &'static str)]) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(64) {
            let Ok(mut stream) = stream else { continue };
            use std::io::{BufRead, BufReader, Write};
            let requested = {
                let mut line = String::new();
                let mut reader = BufReader::new(&mut stream);
                let _ = reader.read_line(&mut line);
                line.split_whitespace().nth(1).unwrap_or("/").to_string()
            };
            let body = routes
                .iter()
                .find(|(path, _)| *path == requested)
                .map(|(_, html)| *html)
                .unwrap_or("<!doctype html><html><body>404</body></html>");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                 Cache-Control: no-store\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// Serve one page on its own loopback PORT, which is its own ORIGIN.
/// Returns the base url; the thread dies with the test.
fn serve_page(html: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

/// The seed must run ONCE, and "once" cannot be enforced from inside the
/// page: it used to be guarded by a `sessionStorage` sentinel, which is
/// per ORIGIN, so any navigation crossing an origin could not see it and
/// re-seeded - silently overwriting whatever the flow had changed.
///
/// Two loopback PORTS are two origins, so this fails on every platform if
/// the guard lives in page storage, and passes only when the seed script is
/// dropped after the first document. The second origin must show MISSING:
/// the seed belongs to the first document alone.
#[test]
fn the_seed_does_not_follow_a_cross_origin_navigation() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping cross-origin seed E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    const PAGE: &str = r#"<!doctype html><html><body><div id="cart"></div><script>
        document.getElementById('cart').textContent =
            'cart: ' + (localStorage.getItem('cart-contents') || 'MISSING');
    </script></body></html>"#;
    let origin_a = serve_page(PAGE);
    let origin_b = serve_page(PAGE);

    let dir = std::env::temp_dir().join("flowproof-web-seed-cross-origin-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("cross-origin.trace.jsonl");

    let mut local_storage = std::collections::BTreeMap::new();
    local_storage.insert("cart-contents".to_string(), "[4]".to_string());
    let spec = flowproof_agent::FlowSpec {
        name: "Seed does not cross an origin".into(),
        app: "web".into(),
        url: Some(origin_a.clone()),
        redact: vec![],
        connection: None,
        login: None,
        window: None,
        session: Some(flowproof_agent::SessionRef::Inline(
            flowproof_trace::format::SessionSetup {
                cookies: vec![],
                local_storage,
            },
        )),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows cart: [4]".into(),
            },
            flowproof_agent::SpecStep::Plain(format!("Go to {origin_b}")),
            // The seed is a fixture for the flow's FIRST document, not a
            // rule the browser carries everywhere.
            flowproof_agent::SpecStep::Assert {
                assert: "page shows cart: MISSING".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "the seed must not re-run on a second origin: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn seeded_fixture_mutation_survives_navigation() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web seed-once E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    // Served from ONE loopback port, which is ONE origin, so localStorage is
    // shared across both pages exactly as it is on a real site.
    //
    // This test used to write the two pages to disk and navigate between
    // `file://` urls, which made it flaky on CI: Chrome does not reliably
    // share localStorage between two file documents, so the mutation this
    // asserts about could vanish for a reason that has nothing to do with
    // seeding. `serve_site` exists for exactly that, and this test predates
    // its adoption — the failure was `cart: MISSING` on the second page,
    // after the first three steps had passed.
    const SHOP: &str = r#"<!doctype html><html><body>
            <div id="cart"></div>
            <button onclick="
                const c = JSON.parse(localStorage.getItem('cart-contents') || '[]');
                c.push(5);
                localStorage.setItem('cart-contents', JSON.stringify(c));
                render();
            ">Add item</button>
            <script>
                function render() {
                    document.getElementById('cart').textContent =
                        'cart: ' + (localStorage.getItem('cart-contents') || 'MISSING');
                }
                render();
            </script></body></html>"#;
    // Page 2 renders the cart at load time - AFTER the init script reran.
    const CART: &str = r#"<!doctype html><html><body><div id="cart"></div><script>
            document.getElementById('cart').textContent =
                'cart: ' + (localStorage.getItem('cart-contents') || 'MISSING');
        </script></body></html>"#;
    let base = serve_site(&[("/shop.html", SHOP), ("/cart.html", CART)]);

    let dir = std::env::temp_dir().join("flowproof-web-seed-once-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("seed-once.trace.jsonl");

    let mut local_storage = std::collections::BTreeMap::new();
    local_storage.insert("cart-contents".to_string(), "[4]".to_string());
    let spec = flowproof_agent::FlowSpec {
        name: "Seeded cart mutation survives navigation".into(),
        app: "web".into(),
        url: Some(format!("{base}/shop.html")),
        redact: vec![],
        connection: None,
        login: None,
        window: None,
        session: Some(flowproof_agent::SessionRef::Inline(
            flowproof_trace::format::SessionSetup {
                cookies: vec![],
                local_storage,
            },
        )),
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
        steps: vec![
            flowproof_agent::SpecStep::Assert {
                assert: "page shows cart: [4]".into(),
            },
            flowproof_agent::SpecStep::Plain("Click \"Add item\"".into()),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows cart: [4,5]".into(),
            },
            flowproof_agent::SpecStep::Plain(format!("Go to {base}/cart.html")),
            flowproof_agent::SpecStep::Assert {
                assert: "page shows cart: [4,5]".into(),
            },
        ],
    };

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "mutation must survive navigation: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn late_rendered_assert_targets_are_waited_for() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web late-assert E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-late-assert-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("late.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <div id="total">42</div>
            <div id="slot"></div>
            <script>
                setTimeout(function () {
                    document.getElementById('slot').innerHTML =
                        '<label for="tos">Terms</label>' +
                        '<input id="tos" type="checkbox" checked>' +
                        '<div id="echo">42</div>';
                }, 7000);
            </script></body></html>"#,
    )
    .expect("page written");
    let trace_path = dir.join("late.trace.jsonl");

    let spec = flowproof_agent::FlowSpec::parse(&format!(
        "name: Late assert targets\napp: web\nurl: file://{}\nsteps:\n  \
         - Remember the \"css:#total\" as total\n  \
         - assert: the \"Terms\" checkbox is checked\n  \
         - assert: the \"css:#echo\" shows ${{captured.total}}\n",
        page.display()
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "late assert targets must pass: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// `repeat:` with a `when:` recovery nested inside it, against a page that
/// faults on its way to the goal — the shape a real obstacle had, reduced to
/// a fixture and made deterministic (the faults happen on known presses, so
/// nothing here depends on a seed).
///
/// What it proves is what the trace holds: the passes that ACTUALLY ran, as
/// ordinary steps. The recovery presses are in there because the page faulted,
/// not because anyone wrote them.
#[test]
fn a_repeat_with_a_nested_when_records_the_passes_that_ran() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping control-flow E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    // #go advances a counter; on presses 3 and 6 it faults, replacing itself
    // with a fault button that #fix clears. Ten clean advances finish it.
    const PAGE: &str = r#"<!doctype html><html><body>
        <div id="status">working</div>
        <button id="go">Advance</button>
        <div id="slot"></div>
        <script>
            var n = 0, presses = 0;
            function render() {
                document.getElementById('status').textContent =
                    n >= 10 ? 'FINISHED' : ('at ' + n);
            }
            function bind() {
                document.getElementById('go').onclick = function () {
                    presses++;
                    if (presses === 3 || presses === 6) {
                        this.remove();
                        document.getElementById('slot').innerHTML =
                            '<button id="fix">Clear fault</button>';
                        document.getElementById('fix').onclick = function () {
                            document.getElementById('slot').innerHTML = '';
                            document.body.insertAdjacentHTML('beforeend',
                                '<button id="go">Advance</button>');
                            bind();
                        };
                        return;
                    }
                    n++; render();
                };
            }
            bind(); render();
        </script></body></html>"#;
    let base = serve_page(PAGE);

    let dir = std::env::temp_dir().join("flowproof-web-repeat-when-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("repeat-when.trace.jsonl");

    let spec = flowproof_agent::FlowSpec::parse(&format!(
        "name: Advance until finished\napp: web\nurl: {base}\nsteps:\n  \
         - repeat:\n      until: page shows FINISHED\n      max: 30\n      steps:\n        \
         - when: the \"id:go\" is not visible\n          steps:\n            \
         - Press the \"id:fix\" button\n        \
         - Press the \"id:go\" button\n  \
         - assert: page shows FINISHED\n"
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording");
    drop(driver);

    // 10 advances + 2 faulted presses + 2 recoveries + the assert. The exact
    // number matters: a loop that had been written into the trace, or one
    // that ran to its bound, would not land here.
    assert_eq!(summary.steps, 15, "the trace holds the passes that ran");
    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(
        !trace.contains("repeat"),
        "no control flow survives into the trace"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "the recorded passes must replay: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The numeric comparison, against a page where the TEXTUAL answer is the
/// wrong one. `"9"` sorts after `"10"` as a string and before it as a number,
/// so a comparison that read the two as text would swap a pair that was
/// already in order, and the flow would run to its bound and fail. Passing
/// this test is the assertion that it does not.
#[test]
fn a_comparison_condition_orders_numerically_not_textually() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping comparison E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    const PAGE: &str = r#"<!doctype html><html><body>
        <div id="a">9</div><div id="b">10</div>
        <div id="status">?</div>
        <button id="swap">Swap</button>
        <button id="check">Check</button>
        <script>
            var a = document.getElementById('a'), b = document.getElementById('b');
            document.getElementById('swap').onclick = function () {
                var t = a.textContent; a.textContent = b.textContent; b.textContent = t;
            };
            document.getElementById('check').onclick = function () {
                document.getElementById('status').textContent =
                    Number(a.textContent) < Number(b.textContent) ? 'SORTED' : 'UNSORTED';
            };
        </script></body></html>"#;
    let base = serve_page(PAGE);

    let dir = std::env::temp_dir().join("flowproof-web-compare-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("compare.trace.jsonl");

    let spec = flowproof_agent::FlowSpec::parse(&format!(
        "name: Order a pair\napp: web\nurl: {base}\nsteps:\n  \
         - repeat:\n      until: page shows SORTED\n      max: 4\n      steps:\n        \
         - when: the \"id:a\" is greater than the \"id:b\"\n          steps:\n            \
         - Press the \"id:swap\" button\n        \
         - Press the \"id:check\" button\n  \
         - assert: page shows SORTED\n"
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording");
    drop(driver);

    // One check and the assert. A textual comparison would have swapped
    // first, and 10/9 never sorts by swapping again.
    assert_eq!(summary.steps, 2, "9 is not greater than 10");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "the pair was already in order: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// `Drag` end to end: the grammar, the recording, and the replay of what was
/// recorded — against a mouse-family sortable in miniature.
///
/// The driver's own measurement (`flowproof-adapters`, `drag_spike`) proves
/// the DISPATCH lands. This proves the rest of the path: that a spec saying
/// `Drag … onto …` reaches it, that both ends survive into the trace as
/// selector ladders, and that replaying the trace performs the drop again.
///
/// The fixture ABORTS a drag on a move reporting no button held, exactly as
/// jQuery UI does, so a dispatch regression fails here too rather than
/// quietly dropping nothing.
#[test]
fn a_drag_records_and_replays_against_a_mouse_sortable() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping drag E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    const PAGE: &str = r#"<!doctype html><html><body>
        <div id="src" style="width:120px;height:40px;background:#ccc">drag me</div>
        <div style="height:200px"></div>
        <div id="dst" style="width:200px;height:80px;background:#eee">empty</div>
        <script>
          var down = null, started = false;
          document.getElementById('src').addEventListener('mousedown', function (e) {
            down = {x: e.clientX, y: e.clientY}; started = false;
          });
          document.addEventListener('mousemove', function (e) {
            if (!down) { return; }
            if (!e.buttons) { down = null; return; }   // the button came up
            if (!started &&
                Math.abs(e.clientX - down.x) + Math.abs(e.clientY - down.y) > 4) {
              started = true;
            }
          });
          document.addEventListener('mouseup', function (e) {
            if (!down || !started) { down = null; return; }
            var r = document.getElementById('dst').getBoundingClientRect();
            if (e.clientX >= r.left && e.clientX <= r.right &&
                e.clientY >= r.top && e.clientY <= r.bottom) {
              document.getElementById('dst').textContent = 'landed';
            }
            down = null; started = false;
          });
        </script></body></html>"#;
    let base = serve_page(PAGE);

    let dir = std::env::temp_dir().join("flowproof-web-drag-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("drag.trace.jsonl");

    let spec = flowproof_agent::FlowSpec::parse(&format!(
        "name: Drag onto the target\napp: web\nurl: {base}\n\
         browser:\n  viewport:\n    width: 1280\n    height: 900\nsteps:\n  \
         - Drag the \"css:#src\" onto the \"css:#dst\"\n  \
         - assert: the \"css:#dst\" shows landed\n"
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);

    // Both ends are in the trace, and the drop target as its own ladder -
    // recorded as a bare string it could not survive the drift the source
    // is protected from.
    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(trace.contains("\"onto\""), "the drop target is recorded");
    assert!(trace.contains("#dst"), "including how to find it again");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "the recorded drag must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// The cross-technology handoff: a value one flow MINTS (captured off its
/// surface, typed nowhere) reaches the next flow as an environment
/// variable. Flow A remembers an order number and exports it; flow B types
/// `${HANDOFF_ORDER_NO}` — a variable nothing in the environment sets. If
/// the suite runner did not thread A's export to B, B's replay fails
/// naming the unset variable; a pass proves the value crossed flows, and
/// crossed them at REPLAY time (the var is scrubbed before the run, so a
/// stale record-time value cannot satisfy it).
#[test]
fn suite_run_hands_a_passing_flows_exports_to_the_flows_after_it() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web suite E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-e2e-exports");
    std::fs::remove_dir_all(&dir).ok();
    let specs_dir = dir.join("specs");
    std::fs::create_dir_all(&specs_dir).expect("temp dirs");

    // Flow A's page carries the order number; A captures and exports it.
    let mint_page = dir.join("mint.html");
    std::fs::write(
        &mint_page,
        r#"<!doctype html><html><body><div id="order">4711</div></body></html>"#,
    )
    .expect("page written");
    let mint_yaml = format!(
        "name: Mint order\napp: web\nurl: file://{}\nsteps:\n  - Remember the \"css:#order\" as order\nexports:\n  HANDOFF_ORDER_NO: ${{captured.order}}\n",
        mint_page.display()
    );
    let mint_path = specs_dir.join("a-mint.flow.yaml");
    std::fs::write(&mint_path, &mint_yaml).expect("spec written");

    // Flow B's page greets whatever is typed; B types the exported value.
    let spend_page = dir.join("spend.html");
    std::fs::write(&spend_page, GREETER_HTML).expect("page written");
    let spend_yaml = format!(
        "name: Spend order\napp: web\nurl: file://{}\nsteps:\n  - Type ${{HANDOFF_ORDER_NO}} into the name field\n  - Press the greet button\n  - assert: page shows Hello, 4711\n",
        spend_page.display()
    );
    let spend_path = specs_dir.join("b-spend.flow.yaml");
    std::fs::write(&spend_path, &spend_yaml).expect("spec written");

    // Record both through the normal pipeline. B's recording needs the
    // variable, exactly as it would in a suite-mode `record` (where A's
    // verification replay has already exported it).
    std::env::set_var("HANDOFF_ORDER_NO", "4711");
    for (path, yaml) in [(&mint_path, &mint_yaml), (&spend_path, &spend_yaml)] {
        let spec = flowproof_agent::FlowSpec::parse(yaml).expect("spec parses");
        let trace_path = flowproof_cli::default_trace_path(path);
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    }

    // Scrub the variable: from here on, only flow A's replay can set it.
    std::env::remove_var("HANDOFF_ORDER_NO");
    let code = flowproof_cli::run_suite(&specs_dir, false, 0, flowproof_cli::MissingTrace::Error)
        .expect("suite runs");
    assert_eq!(
        code,
        flowproof_cli::EXIT_PASS,
        "flow B resolves the variable flow A's replay exported"
    );
    assert_eq!(
        std::env::var("HANDOFF_ORDER_NO").as_deref(),
        Ok("4711"),
        "the export was set from A's replay-time capture"
    );
    std::env::remove_var("HANDOFF_ORDER_NO");
    std::fs::remove_dir_all(&dir).ok();
}

/// Three controls the page has drawn for itself, all hit-tested at a point
/// that lands on something else inside their own `<label>`.
///
/// `#agree` is the ordinary custom checkbox: a span painted over an input
/// the stylesheet made invisible. `#news` is the same shape with a LINK as
/// the face. `#pair` wraps two inputs, so the label labels `#first` and
/// nothing else.
const LABEL_FORWARDING_HTML: &str = r##"<!doctype html>
<html><head><meta charset="utf-8"><title>Styled controls</title>
<style>
  label { position: relative; display: block; width: 160px; height: 28px;
          margin: 24px; }
  label input { position: absolute; left: 0; top: 0;
                width: 160px; height: 28px; margin: 0; opacity: 0; }
  .face { position: absolute; left: 0; top: 0; width: 160px; height: 28px;
          line-height: 28px; background: #cfe; }
</style></head>
<body>
  <label id="styled">
    <input type="checkbox" id="agree">
    <span class="face">I agree</span>
  </label>

  <label id="linked">
    <input type="checkbox" id="news">
    <a class="face" href="#read">Read the terms</a>
  </label>

  <label id="pair">
    <input type="checkbox" id="first">
    <input type="checkbox" id="second">
    <span class="face">Both</span>
  </label>

  <label id="mapped">
    <input type="checkbox" id="mapbox">
    <img class="face" usemap="#m"
      src="data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7">
    <map name="m"><area shape="rect" coords="0,0,160,28" href="#gone"></map>
  </label>

  <label id="off">
    <input type="checkbox" id="disabled-box" disabled>
    <span class="face">Out of stock</span>
  </label>
</body></html>
"##;

/// Label forwarding, both ways round — the half that must keep working and
/// the half that never worked at all.
///
/// A styled control has to stay recordable: the click lands on the span the
/// page painted over the input, the browser forwards it, and refusing that
/// would make every custom checkbox on the web unrecordable.
///
/// But the browser forwards on its own terms, and two shapes it refuses were
/// being recorded as clean clicks. A link inside the label keeps the
/// activation for itself — the click follows the href and the box never
/// ticks. A label wrapping two controls labels the FIRST one, so the second
/// was borrowing an area no click of its own can reach. Both minted a trace
/// asserting a page state nothing had produced, which is the false green
/// this gate exists to prevent.
#[test]
fn a_label_forwards_a_click_only_where_the_browser_does() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping label-forwarding E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-label-forwarding-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("styled-controls.html");
    std::fs::write(&page, LABEL_FORWARDING_HTML).expect("page written");
    let url = format!("file://{}", page.display());

    let spec = |target: &str| {
        flowproof_agent::FlowSpec::parse(&format!(
            "name: Styled controls\napp: web\nurl: {url}\nsteps:\n  \
             - Click the \"css:{target}\"\n"
        ))
        .expect("spec parses")
    };

    // The label really does forward these: the input is the control the
    // label names, and the face over it is inert decoration.
    for target in ["#agree", "#first"] {
        let trace = dir.join(format!("{}.trace.jsonl", target.trim_start_matches('#')));
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        flowproof_agent::record(&spec(target), &mut driver, &trace).unwrap_or_else(|e| {
            panic!("a page-styled control must stay recordable ({target}): {e}")
        });
        drop(driver);
    }

    // ...and does not forward these. `#news` is covered by a link, `#second`
    // by a face belonging to the control beside it.
    // `#mapbox` is faced by an image map. The hit is the `<area>`, which
    // extends HTMLAnchorElement and keeps the activation for itself — and
    // `img[usemap]` never catches it, because the image is a SIBLING of the
    // area, never on its ancestor chain.
    for target in ["#news", "#second", "#mapbox"] {
        let trace = dir.join("refused.trace.jsonl");
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        let err = flowproof_agent::record(&spec(target), &mut driver, &trace)
            .expect_err(&format!("the browser forwards no click to {target}"));
        let message = err.to_string();
        assert!(
            message.contains("another element would receive it"),
            "the refusal must name the occlusion for {target}: {message}"
        );
        drop(driver);
        assert!(
            !trace.exists(),
            "a click the page never received must not reach the trace ({target})"
        );
    }

    std::fs::remove_dir_all(&dir).ok();
}
