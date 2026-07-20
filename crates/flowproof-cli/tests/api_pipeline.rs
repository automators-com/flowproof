//! UI-less flows (`app: api`): out-of-band assertions with no browser and
//! no window — the ~21 "impossible" API-only tests the Playwright
//! evaluation flagged. Record and replay run against a real local HTTP
//! server through the production NoOpDriver path (no FLOWPROOF_E2E gate:
//! there's no browser to launch, so this runs everywhere on every push).

use flowproof_agent::FlowSpec;

/// A tiny HTTP server: `GET /health` → 200 `{"status":"ok"}`, everything
/// else → 404. Serves a fixed number of requests, then stops.
fn serve(server: tiny_http::Server, requests: usize) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok(request) = server.recv() else { break };
            let (code, body) = if request.url() == "/health" {
                (200, r#"{"status":"ok"}"#)
            } else {
                (404, r#"{"error":"not found"}"#)
            };
            let response = tiny_http::Response::from_string(body).with_status_code(code);
            request.respond(response).ok();
        }
    })
}

#[test]
fn records_and_replays_an_api_only_flow() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    // record makes 2 probes (both asserts), replay makes 2 more.
    let server_thread = serve(server, 4);
    // The base host travels via ${VAR} indirection, never into the trace.
    std::env::set_var("API_BASE", &base);

    let spec_yaml = "\
name: Health checks
app: api
steps:
  - assert_api:
      request: GET ${API_BASE}/health
      status: 200
      body_contains: \"\\\"status\\\":\\\"ok\\\"\"
  - assert_api:
      request: GET ${API_BASE}/missing
      status: 404
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-pipeline");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("health.trace.jsonl");

    // Record — no driver launch, no browser.
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("api flow records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    let header = trace.lines().next().expect("header");
    assert!(header.contains("\"adapter\":\"api\""), "header: {header}");
    // The base host resolved from ${API_BASE} must NOT be in the trace.
    assert!(
        !trace.contains(&base),
        "resolved host must not leak into the trace"
    );
    assert!(trace.contains("${API_BASE}"), "trace keeps the ref");

    // Replay — deterministic, still no browser.
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "api flow must replay: {report:#?}");

    server_thread.join().ok();
    std::env::remove_var("API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}
