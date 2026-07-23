//! End-to-end proof of the LLM authoring loop: steps the rules CANNOT parse
//! get authored against real headless Chromium, producing a standard trace
//! that then replays deterministically.
//!
//! Two variants:
//! - `authors_via_openai_compatible_server`: the model is a LOCAL fake HTTP
//!   server speaking `/chat/completions` — exercises the real HTTP client,
//!   real scene extraction, real grounding, real browser actions, with zero
//!   tokens. Gated on FLOWPROOF_E2E=1 (runs in ubuntu CI).
//! - `authors_via_live_anthropic`: the real Anthropic API. Gated on
//!   FLOWPROOF_E2E_LLM=1 plus a key — for maintainers to run locally.

use flowproof_agent::{FlowSpec, SpecStep};

const GREETER_HTML: &str = include_str!("../../../examples/web/greeter.html");

/// Steps deliberately phrased so the rules resolver cannot parse them.
fn freeform_spec(url: String) -> FlowSpec {
    FlowSpec {
        name: "Greet freeform".into(),
        app: "web".into(),
        url: Some(url),
        redact: vec![],
        connection: None,
        window: None,
        session: None,
        skip_unless_env: Vec::new(),
        mock: Vec::new(),
        browser: None,
        agent: None,
        tools: Vec::new(),
        strict: false,
        steps: vec![
            SpecStep::Plain("Put Ada into the box labelled with the name".into()),
            SpecStep::Plain("Smash the greeting button".into()),
            SpecStep::Assert {
                assert: "the page should now be greeting Ada".into(),
            },
        ],
    }
}

/// Minimal OpenAI-compatible model: answers based on which step intent
/// appears in the request body, and records the bodies so the test can prove
/// the prompts carried the live page's scene graph.
fn serve_scripted(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut bodies = Vec::new();
        while let Ok(mut request) = server.recv() {
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let reply = if body.contains("Put Ada into the box") {
                r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##
            } else if body.contains("Smash the greeting button") {
                r##"{"action":"click","target":"css:#greet"}"##
            } else if body.contains("greeting Ada") {
                r##"{"action":"assert_text","target":"css:#greeting","expected":"Hello, Ada","contains":true}"##
            } else {
                r##"{"action":"click","target":"css:#nonsense"}"##
            };
            let payload = serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": reply}}]
            });
            let response = tiny_http::Response::from_string(payload.to_string()).with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .expect("header"),
            );
            bodies.push(body);
            request.respond(response).ok();
            if bodies.len() >= 3 {
                break;
            }
        }
        bodies
    })
}

#[test]
fn authors_via_openai_compatible_server() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping LLM-author E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-llm-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("freeform.trace.jsonl");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let server_thread = serve_scripted(server);

    let spec = freeform_spec(format!("file://{}", page.display()));
    let config = flowproof_agent::BackendConfig {
        kind: flowproof_agent::BackendKind::OpenAiCompatible,
        base_url: Some(base_url),
        model: Some("fake-local-model".into()),
        api_key: None,
    };
    let mut client = flowproof_agent::HttpModelClient::new(config);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::recorder::record_with_client(
        &spec,
        &mut driver,
        &trace_path,
        flowproof_agent::Author::Auto,
        Some(&mut client),
    )
    .expect("model authors the freeform flow");
    drop(driver);

    // The prompts carried the REAL scene from the live page.
    let bodies = server_thread.join().expect("server thread");
    assert_eq!(bodies.len(), 3);
    for body in &bodies {
        assert!(
            body.contains("css:#name") && body.contains("css:#greet"),
            "scene target tokens in prompt"
        );
    }

    // The trace records model authorship and standard css selectors.
    let (header, steps) = flowproof_replay::load_trace(&trace_path).expect("trace loads");
    let agent = header.agent.expect("agent stamped in header");
    assert!(agent.model.as_deref() == Some("fake-local-model"));
    assert_eq!(steps.len(), 3);

    // And it replays deterministically — zero model involvement.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "authored flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn authors_via_live_anthropic() {
    if std::env::var("FLOWPROOF_E2E_LLM").as_deref() != Ok("1") {
        eprintln!("skipping live LLM E2E: set FLOWPROOF_E2E_LLM=1 (and an API key) to run it");
        return;
    }
    let Some(mut client) = flowproof_agent::HttpModelClient::from_env() else {
        panic!("FLOWPROOF_E2E_LLM=1 but no usable model backend configured");
    };

    let dir = std::env::temp_dir().join("flowproof-llm-live-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let trace_path = dir.join("freeform.trace.jsonl");
    let spec = freeform_spec(format!("file://{}", page.display()));

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::recorder::record_with_client(
        &spec,
        &mut driver,
        &trace_path,
        flowproof_agent::Author::Auto,
        Some(&mut client),
    )
    .expect("live model authors the freeform flow");
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "authored flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
