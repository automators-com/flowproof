//! End-to-end proof that LLM authoring works on DESKTOP apps: the Windows
//! UIA driver describes its scene, a scripted OpenAI-compatible model picks
//! targets from it (native `id:`/`text:` tokens — no css anywhere), and the
//! authored trace replays deterministically against real Notepad.
//! Windows-only and opt-in via FLOWPROOF_E2E=1 — runs in windows CI.

#![cfg(windows)]

use flowproof_agent::{FlowSpec, SpecStep};
use flowproof_driver::UiaAppDriver;

/// Kill any notepad instance so each phase starts from a fresh, empty
/// document and unsaved-changes prompts never appear.
fn kill_notepad() {
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/IM", "notepad.exe"])
        .output();
    std::thread::sleep(std::time::Duration::from_millis(500));
}

/// Steps deliberately phrased so the rules resolver cannot parse them.
fn freeform_spec() -> FlowSpec {
    FlowSpec {
        name: "Notepad freeform".into(),
        app: "notepad".into(),
        url: None,
        redact: vec![],
        connection: None,
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
        steps: vec![
            SpecStep::Plain("Write hello from flowproof into the editor".into()),
            SpecStep::Assert {
                assert: "the document should now be showing hello from flowproof".into(),
            },
        ],
    }
}

/// Minimal OpenAI-compatible model: answers by which step intent appears in
/// the request body, and records the bodies so the test can prove the
/// prompts carried the UIA scene.
fn serve_scripted(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut bodies = Vec::new();
        while let Ok(mut request) = server.recv() {
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let reply = if body.contains("Write hello from flowproof") {
                // "id:15" is classic Notepad's editor automation id — the
                // scene must list it or grounding rejects this reply.
                r##"{"action":"type_text","target":"id:15","text":"hello from flowproof"}"##
            } else {
                r##"{"action":"assert_text","target":"surface","expected":"hello from flowproof","contains":true}"##
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
            if bodies.len() >= 2 {
                break;
            }
        }
        bodies
    })
}

#[test]
fn authors_against_real_notepad() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping notepad authoring E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-notepad-author-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("freeform.trace.jsonl");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let server_thread = serve_scripted(server);

    let config = flowproof_agent::BackendConfig {
        kind: flowproof_agent::BackendKind::OpenAiCompatible,
        base_url: Some(base_url),
        model: Some("fake-local-model".into()),
        api_key: None,
    };
    let mut client = flowproof_agent::HttpModelClient::new(config);

    kill_notepad();
    let record_result = (|| {
        let mut driver = UiaAppDriver::new()?;
        flowproof_agent::recorder::record_with_client(
            &freeform_spec(),
            &mut driver,
            &trace_path,
            flowproof_agent::Author::Auto,
            Some(&mut client),
        )
        .map_err(|e| flowproof_driver::DriverError::Uia(format!("record failed: {e}")))
    })();
    kill_notepad();
    record_result.expect("model authors the freeform flow");

    // The prompts carried the REAL UIA scene: the editor's native id token.
    let bodies = server_thread.join().expect("server thread");
    assert_eq!(bodies.len(), 2);
    assert!(
        bodies[0].contains("id:15"),
        "scene must list the editor's automation id token: {}",
        bodies[0]
    );

    // The trace records model authorship and native (non-css) selectors.
    let (header, steps) = flowproof_replay::load_trace(&trace_path).expect("trace loads");
    let agent = header.agent.expect("agent stamped in header");
    assert_eq!(agent.model.as_deref(), Some("fake-local-model"));
    assert_eq!(steps.len(), 2);

    // And it replays deterministically — zero model involvement.
    let replay_result = (|| {
        let mut driver = UiaAppDriver::new()?;
        flowproof_replay::run_trace(&trace_path, &mut driver)
            .map(|(report, _)| report)
            .map_err(|e| flowproof_driver::DriverError::Uia(format!("replay failed: {e}")))
    })();
    kill_notepad();
    let report = replay_result.expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "authored flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
