//! End-to-end proof of goal-based authoring: explores real headless
//! Chromium against examples/web/greeter.html, using a local fake
//! OpenAI-compatible HTTP server for the model — exercises the real HTTP
//! client, real scene extraction, real grounding, real driver actions,
//! real recording, with zero tokens. Gated on FLOWPROOF_E2E=1, matching
//! llm_author_e2e.rs's own gating (runs in ubuntu CI).
//!
//! Manually verified once against the real Anthropic API before this test
//! was written (see the feat/goal-based-authoring branch history): the
//! full loop — author-from-goal, then `record`, then `run` — passed with
//! zero LLM calls on replay. This test locks that same loop in with a
//! fake model so it runs in CI without a key.

use flowproof_agent::goal_author::{self, GoalAuthorOptions, GoalOutcome};
use flowproof_agent::FlowSpec;

const GREETER_HTML: &str = include_str!("../../../examples/web/greeter.html");

fn goal_spec(url: String) -> FlowSpec {
    FlowSpec::parse(&format!(
        "name: Greeter goal\napp: web\nurl: {url}\ngoal: the page greets the name typed with a message containing \"Hello\"\n"
    ))
    .expect("goal spec parses")
}

/// Serves requests by handing each body to `reply_for`, until the client
/// falls idle for `IDLE_TIMEOUT` — not a fixed call count. Neither
/// goal-authoring's own grounding retries nor `record`'s make a fixed
/// number of calls (a rejected reply costs an extra round trip), so a
/// server that stops after N requests risks the client's (N+1)th call
/// hanging against a socket nobody is listening on anymore; idling out
/// instead means an unexpected extra call is served correctly, not missed.
const IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn serve_until_idle(
    server: tiny_http::Server,
    reply_for: impl Fn(&str) -> &'static str + Send + 'static,
) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut bodies = Vec::new();
        loop {
            let request = match server.recv_timeout(IDLE_TIMEOUT) {
                Ok(Some(request)) => request,
                Ok(None) => break, // idle: the client has stopped calling
                Err(_) => break,
            };
            let mut request = request;
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let reply = reply_for(&body);
            let payload = serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": reply}}]
            });
            let response = tiny_http::Response::from_string(payload.to_string()).with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .expect("header"),
            );
            bodies.push(body);
            request.respond(response).ok();
        }
        bodies
    })
}

/// Dispatches on the request's own content, not call order — the
/// goal-check prompt is tagged "This is a CHECK" by author_checks itself
/// (author.rs), and a propose call's "Steps already performed" names
/// whatever was invoked so far.
fn serve_scripted(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    serve_until_idle(server, |body| {
        if body.contains("This is a CHECK") {
            // Whether this actually holds is decided live, against the
            // real page — the fake model's only job is to say what to
            // check, every time, the same way.
            r##"{"action":"assert_text","target":"surface","expected":"Hello"}"##
        } else if body.contains("Type Ada") {
            // The name has already been typed (it's in this call's own
            // "Steps already performed"): the only thing left is Greet.
            r##"{"action":"click","target":"css:#greet"}"##
        } else {
            r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##
        }
    })
}

/// The draft's action steps ("Type Ada into...", "Press the...") are plain
/// text, so `record`'s default `--author auto` grounds them against a
/// model exactly like any hand-authored flow's plain steps would — this
/// is the documented next step ("then `flowproof record`"), not a
/// rules-only shortcut, so a second fake server plays that model too.
fn serve_record_phase(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    serve_until_idle(server, |body| {
        // Anchored on "Current step to perform:", never a bare substring —
        // by the third step's call, "Steps already performed" already
        // names the first two, so a bare `body.contains("Type Ada into")`
        // matches every later call too, not just the one that should type.
        if body.contains("Current step to perform: Type Ada into") {
            r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##
        } else if body.contains("Current step to perform: Press the") {
            r##"{"action":"click","target":"css:#greet"}"##
        } else {
            r##"{"action":"assert_text","target":"surface","expected":"Hello"}"##
        }
    })
}

#[test]
fn explores_a_goal_then_records_and_replays() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping goal-author E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-goal-author-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let draft_path = dir.join("greeter.draft.flow.yaml");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let server_thread = serve_scripted(server);

    let spec = goal_spec(format!("file://{}", page.display()));
    let config = flowproof_agent::BackendConfig {
        kind: flowproof_agent::BackendKind::OpenAiCompatible,
        base_url: Some(base_url),
        model: Some("fake-local-model".into()),
        api_key: None,
        workspace_id: None,
    };
    let mut client = flowproof_agent::HttpModelClient::new(config);
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let opts = GoalAuthorOptions {
        out: draft_path.clone(),
        budget: 10,
        recording: flowproof_driver::RecordingOptions {
            detail: flowproof_driver::RecordingDetail::Off,
            video: false,
            highlight_cursor: false,
        },
    };
    let result = goal_author::author_from_goal(&spec, &mut driver, &mut client, &opts)
        .expect("goal exploration succeeds");
    drop(driver);

    assert!(
        matches!(result.outcome, GoalOutcome::Reached { .. }),
        "the goal is genuinely reachable in two actions: {:?}",
        result.outcome
    );

    let bodies = server_thread.join().expect("server thread");
    assert!(
        bodies.len() >= 3,
        "at least one check, one type, one click must have happened: {} calls",
        bodies.len()
    );

    // url: (the only reason exploration knew where to start) must survive
    // into the draft — this is the exact bug the extra_head fix prevents.
    let draft_yaml = std::fs::read_to_string(&draft_path).expect("draft readable");
    assert!(
        draft_yaml.contains("url:"),
        "draft must carry the source spec's url: forward:\n{draft_yaml}"
    );
    let draft_spec = FlowSpec::load(&draft_path).expect("draft parses as a real spec");
    assert_eq!(draft_spec.url.as_deref(), spec.url.as_deref());

    // The draft is an ordinary flow from here: record it for real, the
    // documented next step ("then `flowproof record`") — a second fake
    // model server for this phase, so the test never depends on whatever
    // real backend this machine happens to have configured.
    let record_server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let record_base_url = format!("http://{}", record_server.server_addr());
    let record_server_thread = serve_record_phase(record_server);
    let record_config = flowproof_agent::BackendConfig {
        kind: flowproof_agent::BackendKind::OpenAiCompatible,
        base_url: Some(record_base_url),
        model: Some("fake-local-model".into()),
        api_key: None,
        workspace_id: None,
    };
    let mut record_client = flowproof_agent::HttpModelClient::new(record_config);
    let trace_path = dir.join("greeter.draft.trace.jsonl");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::recorder::record_with_client(
        &draft_spec,
        &mut driver,
        &trace_path,
        flowproof_agent::Author::Auto,
        Some(&mut record_client),
    )
    .expect("draft records");
    drop(driver);
    assert!(
        record_server_thread.join().expect("server thread").len() >= 3,
        "two actions plus the goal assertion must each be grounded"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "the goal-authored draft must record and replay: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
