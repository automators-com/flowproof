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

/// A foreach matrix records one real trace step per iteration — the
/// copy-paste class (the db-providers spec repeated one block five times)
/// collapses into a values list, with everything downstream unchanged.
#[test]
fn foreach_expands_to_real_trace_steps_and_replays() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    std::env::set_var("FE_API_BASE", &base);
    // 3 probes at record + 3 at replay.
    let server_thread = std::thread::spawn(move || {
        for _ in 0..6 {
            let Ok(mut request) = server.recv() else {
                break;
            };
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let known = matches!(parsed["type"].as_str(), Some("mysql" | "mssql" | "oracle"));
            let (code, text) = if known {
                (200, "connection accepted")
            } else {
                (400, "unknown provider")
            };
            request
                .respond(tiny_http::Response::from_string(text).with_status_code(code))
                .ok();
        }
    });

    let spec_yaml = "\
name: Providers matrix
app: api
steps:
  - foreach:
      values: [mysql, mssql, oracle]
      steps:
        - assert_api:
            request: POST ${FE_API_BASE}/connections/test
            body:
              type: \"${each}\"
            status: 200
            body_contains: connection accepted
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");
    assert_eq!(spec.steps.len(), 3, "expanded before anything records");

    let dir = std::env::temp_dir().join("flowproof-foreach-pipeline");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("matrix.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("matrix records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    // Each iteration is an ordinary dense-id trace step; the base URL ref
    // survives raw, and the substituted values are literal data.
    for id in ["s0001", "s0002", "s0003"] {
        assert!(trace.contains(&format!("\"id\":\"{id}\"")), "{id} present");
    }
    assert!(trace.contains("${FE_API_BASE}"), "ref kept raw");
    assert!(trace.contains("mssql"), "substituted value recorded");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "matrix replays: {report:#?}");
    assert_eq!(report.steps.len(), 3);

    server_thread.join().ok();
    std::env::remove_var("FE_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

/// The DataMaker-shaped case: an authenticated JSON POST. The server
/// returns 200 "Database not yet supported!" ONLY when it received the
/// exact Authorization header and JSON body — so the flow passing at
/// record AND replay proves both were sent, with the token and a
/// quote-bearing connection string travelling via ${VAR} and never
/// entering the trace.
#[test]
fn records_and_replays_an_authenticated_json_post() {
    // The secret deliberately contains a quote and a backslash: it must
    // land in the JSON body as data (leaf-walk resolution, not reparse).
    let token = "tok-p2831-secret";
    let conn = r#"postgres://u:pa"ss\w@db:5432/x"#;
    std::env::set_var("CONN_API_BASE", ""); // set below once the server binds
    std::env::set_var("CONN_SESSION_TOKEN", token);
    std::env::set_var("CONN_STRING", conn);

    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    std::env::set_var("CONN_API_BASE", &base);

    let expected_auth = format!("Bearer {token}");
    // record 1 probe + replay 1 probe.
    let server_thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok(mut request) = server.recv() else {
                break;
            };
            let auth_ok = request
                .headers()
                .iter()
                .any(|h| h.field.equiv("Authorization") && h.value.as_str() == expected_auth);
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let body_ok = parsed["type"] == "postgres"
                && parsed["connectionString"] == r#"postgres://u:pa"ss\w@db:5432/x"#;
            let json_ct = request.headers().iter().any(|h| {
                h.field.equiv("Content-Type") && h.value.as_str().contains("application/json")
            });
            // Mirrors the real DataMaker contract: an unsupported provider
            // answers 500 with this body — same shape as examples/api/.
            let (code, text) = if request.url() == "/connections/test" && auth_ok && body_ok {
                if json_ct {
                    (500, "Database not yet supported!")
                } else {
                    (415, "missing json content-type")
                }
            } else {
                (401, "unauthorized or wrong body")
            };
            let response = tiny_http::Response::from_string(text).with_status_code(code);
            request.respond(response).ok();
        }
    });

    let spec_yaml = "\
name: Test database providers
app: api
steps:
  - assert_api:
      request: POST ${CONN_API_BASE}/connections/test
      headers:
        Authorization: Bearer ${CONN_SESSION_TOKEN}
      body:
        type: postgres
        connectionString: ${CONN_STRING}
      status: 500
      body_contains: Database not yet supported!
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-auth-post");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("connections.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("authenticated POST records");

    // Redaction invariant: refs in the trace, secrets not.
    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(trace.contains("${CONN_SESSION_TOKEN}"), "header ref kept");
    assert!(trace.contains("${CONN_STRING}"), "body ref kept");
    assert!(!trace.contains(token), "token must not leak into the trace");
    assert!(
        !trace.contains("pa\\\"ss"),
        "connection string must not leak into the trace"
    );

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "authenticated POST must replay: {report:#?}");

    server_thread.join().ok();
    for var in ["CONN_API_BASE", "CONN_SESSION_TOKEN", "CONN_STRING"] {
        std::env::remove_var(var);
    }
    std::fs::remove_dir_all(&dir).ok();
}
