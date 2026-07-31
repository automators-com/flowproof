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

/// `assert_no_secret_leak` on an `app: api` flow: a secret echoed into an
/// `assert_api` response body is caught by the record-time store-guard, which
/// fails the run and mints NO trace, so the leaked value never reaches disk.
/// The failure names the variable and the step, never the value.
#[test]
fn a_secret_in_an_api_response_body_fails_the_record_and_mints_no_trace() {
    // The server echoes the resolved secret into the JSON body.
    let secret = "s3cr3t-connection-string-value";
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let body = format!("{{\"dsn\":\"{secret}\"}}");
    let server_thread = std::thread::spawn(move || {
        // Record probes once, then the run fails at the store-guard: no
        // replay, so a single request is served.
        if let Ok(request) = server.recv() {
            let response = tiny_http::Response::from_string(body).with_status_code(200);
            request.respond(response).ok();
        }
    });
    std::env::set_var("LEAK_API_BASE", &base);
    std::env::set_var("LEAK_DB_DSN", secret);

    let spec_yaml = "\
name: DSN must not surface
app: api
steps:
  - assert_api:
      request: GET ${LEAK_API_BASE}/config
      status: 200
  - assert_no_secret_leak: ${LEAK_DB_DSN}
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");
    let dir = std::env::temp_dir().join("flowproof-api-secret-leak");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("dsn.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect_err("a leaked secret must fail the record");
    let message = err.to_string();

    // Names the variable and the asserting step...
    assert!(
        message.contains("${LEAK_DB_DSN}"),
        "names the var: {message}"
    );
    assert!(message.contains("step 2"), "names the step: {message}");
    assert!(
        message.contains("assert_api response body"),
        "names the corpus element: {message}"
    );
    // ...but NEVER the value.
    assert!(
        !message.contains(secret),
        "message must not leak the value: {message}"
    );
    // And the store-guard minted NO trace.
    assert!(
        !trace_path.exists(),
        "a leak must mint no trace; {} exists",
        trace_path.display()
    );

    server_thread.join().ok();
    std::env::remove_var("LEAK_API_BASE");
    std::env::remove_var("LEAK_DB_DSN");
    std::fs::remove_dir_all(&dir).ok();
}

/// The clean counterpart: the same secret is declared but never appears in the
/// response body, so record mints a trace whose bytes never contain the value,
/// and replay passes deterministically re-scanning the absent secret.
#[test]
fn a_clean_api_flow_records_without_the_secret_and_replays_deterministically() {
    let secret = "s3cr3t-connection-string-value";
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    // Body carries NO secret. Record probes once, replay probes once.
    let server_thread = std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok(request) = server.recv() else { break };
            let response =
                tiny_http::Response::from_string(r#"{"status":"ok"}"#).with_status_code(200);
            request.respond(response).ok();
        }
    });
    std::env::set_var("CLEAN_API_BASE", &base);
    std::env::set_var("CLEAN_DB_DSN", secret);

    let spec_yaml = "\
name: DSN stays contained
app: api
steps:
  - assert_api:
      request: GET ${CLEAN_API_BASE}/health
      status: 200
  - assert_no_secret_leak: ${CLEAN_DB_DSN}
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");
    let dir = std::env::temp_dir().join("flowproof-api-secret-clean");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("clean.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("clean flow records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(
        !trace.contains(secret),
        "the value must never reach the trace"
    );
    // assert_no_secret_leak is additive: it mints no trace step, so the
    // secret's `${VAR}` never appears in the trace. Ordinary refs still do.
    assert!(
        !trace.contains("${CLEAN_DB_DSN}"),
        "the secret-leak selector is not persisted"
    );
    assert!(trace.contains("${CLEAN_API_BASE}"), "ordinary refs stay");

    // Replay through the scanning path: the secret is re-scanned and absent.
    let scan = flowproof_replay::SecretScan {
        assertions: spec.secret_leak_assertions(),
    };
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace_with_secret_scan(&trace_path, &mut driver, &scan)
            .expect("replay runs");
    assert!(report.passed, "clean api flow must replay: {report:#?}");

    server_thread.join().ok();
    std::env::remove_var("CLEAN_API_BASE");
    std::env::remove_var("CLEAN_DB_DSN");
    std::fs::remove_dir_all(&dir).ok();
}

/// The RWA-shaped case: a `GET /testData/users` returns a users array whose
/// first element carries a numeric `balance`. `body_json: results.0.balance`
/// with `equals:` the REAL value records and replays; a WRONG `equals` fails
/// the record (the two-tier compare catches the mismatch). The extracted
/// value lives only inside the probe: the trace keeps the request and the
/// raw expectation, never the plucked response value.
fn serve_users(server: tiny_http::Server, balance: i64) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let body = format!(
            "{{\"results\":[{{\"id\":\"uBmeaz5pX\",\"balance\":{balance}}},\
             {{\"id\":\"GjWovtg2hr\",\"balance\":42}}]}}"
        );
        // Serve until idle: the passing flow makes 2 requests (record + replay),
        // the failing flow polls a handful of times before the record errors.
        // Serve until idle: `recv_timeout` yields `Ok(None)` after the poll
        // stops requesting, so the `while let` exits on its own.
        while let Ok(Some(request)) = server.recv_timeout(std::time::Duration::from_millis(500)) {
            let (code, text): (u16, &str) = if request.url() == "/testData/users" {
                (200, body.as_str())
            } else {
                (404, "{\"error\":\"not found\"}")
            };
            let response = tiny_http::Response::from_string(text).with_status_code(code);
            request.respond(response).ok();
        }
    })
}

#[test]
fn body_json_equals_the_real_first_user_balance_records_and_replays() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_users(server, 150953);
    std::env::set_var("BAL_API_BASE", &base);

    let spec_yaml = "\
name: Balance assertion
app: api
steps:
  - assert_api:
      request: GET ${BAL_API_BASE}/testData/users
      status: 200
      body_json: results.0.balance
      equals: 150953
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-body-json-pass");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("balance.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("body_json flow records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    // The path and the raw expectation travel; the host ref stays a ref.
    assert!(
        trace.contains("\"body_json\":\"results.0.balance\""),
        "trace carries the path: {trace}"
    );
    assert!(trace.contains("\"equals\":150953"), "trace carries equals");
    assert!(
        trace.contains("${BAL_API_BASE}"),
        "trace keeps the host ref"
    );
    assert!(
        !trace.contains(&base),
        "resolved host must not leak into the trace"
    );

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "body_json flow must replay: {report:#?}");

    server_thread.join().ok();
    std::env::remove_var("BAL_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

/// The RWA-shaped header case: `GET /testData/users` answers with an explicit
/// `Content-Type: application/json`. Serves until idle so both the passing
/// (record + replay) and failing (a few polls) flows can share one helper.
fn serve_users_json(server: tiny_http::Server) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let body = r#"{"results":[{"id":"uBmeaz5pX","balance":150953}]}"#;
        let ctype = tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("valid header");
        while let Ok(Some(request)) = server.recv_timeout(std::time::Duration::from_millis(500)) {
            let response = if request.url() == "/testData/users" {
                tiny_http::Response::from_string(body)
                    .with_status_code(200)
                    .with_header(ctype.clone())
            } else {
                tiny_http::Response::from_string(r#"{"error":"not found"}"#).with_status_code(404)
            };
            request.respond(response).ok();
        }
    })
}

#[test]
fn header_contains_the_content_type_records_and_replays() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_users_json(server);
    std::env::set_var("HDR_API_BASE", &base);

    let spec_yaml = "\
name: Content type assertion
app: api
steps:
  - assert_api:
      request: GET ${HDR_API_BASE}/testData/users
      status: 200
      header: Content-Type
      header_contains: json
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-header-pass");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("ctype.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("header flow records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    // The header name and predicate travel; the live value never does.
    assert!(
        trace.contains("\"header\":\"Content-Type\""),
        "trace carries the header name: {trace}"
    );
    assert!(
        trace.contains("\"header_contains\":\"json\""),
        "trace carries the predicate: {trace}"
    );
    assert!(
        trace.contains("${HDR_API_BASE}"),
        "trace keeps the host ref"
    );

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "header flow must replay: {report:#?}");

    server_thread.join().ok();
    std::env::remove_var("HDR_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn header_equals_a_wrong_value_fails_the_record() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_users_json(server);
    std::env::set_var("BADHDR_API_BASE", &base);

    // The response is application/json; the spec asserts text/html.
    let spec_yaml = "\
name: Wrong content type
app: api
steps:
  - assert_api:
      request: GET ${BADHDR_API_BASE}/testData/users
      status: 200
      header: Content-Type
      header_equals: text/html
      timeout_seconds: 1
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-header-fail");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("wrong-ctype.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect_err("a wrong header_equals must fail the record");
    let message = err.to_string();
    // The mismatch message names the header, the actual value, and the want.
    assert!(
        message.contains("Content-Type")
            && message.contains("application/json")
            && message.contains("text/html"),
        "failure names header, actual, and expected: {message}"
    );

    server_thread.join().ok();
    std::env::remove_var("BADHDR_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn header_existence_of_an_absent_header_fails_the_record() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_users_json(server);
    std::env::set_var("ABSHDR_API_BASE", &base);

    let spec_yaml = "\
name: Missing header
app: api
steps:
  - assert_api:
      request: GET ${ABSHDR_API_BASE}/testData/users
      status: 200
      header: X-Does-Not-Exist
      timeout_seconds: 1
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-header-absent");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("absent-header.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect_err("an absent header must fail the record");
    let message = err.to_string();
    assert!(
        message.contains("no 'X-Does-Not-Exist' header"),
        "failure names the absent header: {message}"
    );

    server_thread.join().ok();
    std::env::remove_var("ABSHDR_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn body_json_equals_a_wrong_value_fails_the_record() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    // The server returns the REAL balance; the spec asserts the wrong one.
    let server_thread = serve_users(server, 150953);
    std::env::set_var("BADBAL_API_BASE", &base);

    let spec_yaml = "\
name: Wrong balance
app: api
steps:
  - assert_api:
      request: GET ${BADBAL_API_BASE}/testData/users
      status: 200
      body_json: results.0.balance
      equals: 999
      timeout_seconds: 1
";
    let spec = FlowSpec::parse(spec_yaml).expect("spec parses");

    let dir = std::env::temp_dir().join("flowproof-api-body-json-fail");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("wrong.trace.jsonl");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&spec, &mut driver, &trace_path)
        .expect_err("a wrong equals must fail the record");
    let message = err.to_string();
    // The failure carries the two-tier compare's text ("expected '999', got
    // '150953'"): a wrong scalar is caught, not silently passed.
    assert!(
        message.contains("999") && message.contains("150953"),
        "failure names expected and actual: {message}"
    );

    server_thread.join().ok();
    std::env::remove_var("BADBAL_API_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

/// A counting server that always answers 200, so an assertion demanding 201
/// can never hold. Returns the handle plus the shared counter.
fn serve_counting(server: tiny_http::Server) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = seen.clone();
    std::thread::spawn(move || {
        while let Ok(request) = server.recv() {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let response = tiny_http::Response::from_string(r#"{"ok":true}"#).with_status_code(200);
            request.respond(response).ok();
        }
    });
    seen
}

/// A failing assertion auto-waits by RE-SENDING its probe, which is right
/// for a read and catastrophic for a write: the probe IS the mutation, so a
/// failing `POST` used to deliver ~40 duplicate writes (measured 41 against
/// a counting server over the 10s bound) - and only ever when a test FAILS,
/// when the state is hardest to reason about. Writes are now sent once.
#[test]
fn a_failing_write_assertion_is_sent_exactly_once() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let seen = serve_counting(server);
    std::env::set_var("RETRY_BASE", &base);

    let spec = FlowSpec::parse(
        "\
name: Failing write
app: api
steps:
  - assert_api:
      request: POST ${RETRY_BASE}/orders
      body:
        item: widget
      status: 201
",
    )
    .expect("spec parses");

    let dir = std::env::temp_dir().join(format!("flowproof-api-retry-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&spec, &mut driver, &dir.join("w.trace.jsonl"))
        .expect_err("the assertion cannot hold");

    assert_eq!(
        seen.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "a failing write must be delivered exactly once, not re-sent while auto-waiting"
    );
    // The failure teaches the migration, so a flow that relied on polling a
    // write self-diagnoses the first time it fails.
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("retry: true"),
        "the failure must name the opt-in, got: {rendered}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The read side is unchanged: a failing GET still polls, because re-asking
/// a question is free and "the API is converging" is the whole point.
#[test]
fn a_failing_read_assertion_still_polls() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let seen = serve_counting(server);
    std::env::set_var("RETRY_READ_BASE", &base);

    let spec = FlowSpec::parse(
        "\
name: Failing read
app: api
steps:
  - assert_api:
      request: GET ${RETRY_READ_BASE}/orders
      status: 201
      timeout_seconds: 2
",
    )
    .expect("spec parses");

    let dir = std::env::temp_dir().join(format!("flowproof-api-retry-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &dir.join("r.trace.jsonl"))
        .expect_err("the assertion cannot hold");

    assert!(
        seen.load(std::sync::atomic::Ordering::SeqCst) > 1,
        "a failing read must keep polling within its bound"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// A server whose `/users` returns a 3-element array, the shape the field
/// suite asserts against (`expect(body.results).length.to.be.greaterThan(1)`).
fn serve_collection(server: tiny_http::Server) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        while let Ok(request) = server.recv() {
            let body = r#"{"results":[{"id":"a"},{"id":"b"},{"id":"c"}],"page":{"total":3}}"#;
            let response = tiny_http::Response::from_string(body).with_status_code(200);
            request.respond(response).ok();
        }
    })
}

/// `count` / `count_at_least` on the array at `body_json`. Before these, the
/// only way to ask "how many rows came back" was to assert that some index
/// exists (`results.1.id`), which cannot express "exactly N" and forces you
/// to name a leaf key that element happens to carry. 11 of ~30 assertions in
/// the migrated field suite are of this shape.
#[test]
fn an_api_flow_asserts_array_counts() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    serve_collection(server);
    std::env::set_var("COUNT_BASE", &base);

    let spec = FlowSpec::parse(
        "\
name: Counts
app: api
steps:
  - assert_api:
      request: GET ${COUNT_BASE}/users
      status: 200
      body_json: results
      count: 3
  - assert_api:
      request: GET ${COUNT_BASE}/users
      status: 200
      body_json: results
      count_at_least: 2
",
    )
    .expect("spec parses");

    let dir = std::env::temp_dir().join(format!("flowproof-api-count-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let trace = dir.join("c.trace.jsonl");
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    flowproof_agent::record(&spec, &mut driver, &trace).expect("counts record");

    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let (report, _run_dir) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "counts must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A wrong count fails, and a count against a non-array says which kind it
/// actually found rather than a bare "does not hold".
#[test]
fn a_wrong_count_and_a_non_array_fail_with_the_actual_shape() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    serve_collection(server);
    std::env::set_var("COUNT_FAIL_BASE", &base);

    let dir = std::env::temp_dir().join(format!("flowproof-api-count-bad-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");

    let wrong = FlowSpec::parse(
        "\
name: Wrong count
app: api
steps:
  - assert_api:
      request: GET ${COUNT_FAIL_BASE}/users
      status: 200
      body_json: results
      count: 9
      timeout_seconds: 1
",
    )
    .expect("spec parses");
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&wrong, &mut driver, &dir.join("w.trace.jsonl"))
        .expect_err("3 elements is not 9");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("has 3 elements") && rendered.contains("exactly 9"),
        "the failure must name found and wanted, got: {rendered}"
    );

    // `page` is an object, not an array.
    let not_array = FlowSpec::parse(
        "\
name: Not an array
app: api
steps:
  - assert_api:
      request: GET ${COUNT_FAIL_BASE}/users
      status: 200
      body_json: page
      count: 1
      timeout_seconds: 1
",
    )
    .expect("spec parses");
    let mut driver = flowproof_cli::driver_for("api").expect("api driver");
    let err = flowproof_agent::record(&not_array, &mut driver, &dir.join("n.trace.jsonl"))
        .expect_err("an object has no element count");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("is an object") && rendered.contains("count requires an array"),
        "the failure must name the actual kind, got: {rendered}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// `run` names the HUMAN artifact, not the machine one.
///
/// Every run writes `report.html` beside `result.json` — the step table,
/// the per-step frames and the recording — and for a long time said nothing
/// about it. On macOS and Linux the only UI-driving adapter is `web`, which
/// is headless, so a first run shows no window and points at a JSON file:
/// the reasonable conclusion is that nothing visual was captured. It was,
/// in a dot-directory Finder hides by default.
///
/// Driven through the real binary, because the defect was in what the CLI
/// printed — a library-level assertion could not have caught it.
#[test]
fn run_output_names_the_html_report_and_it_exists() {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    // record probes once, replay probes once.
    let server_thread = serve(server, 2);

    let dir = std::env::temp_dir().join("flowproof-run-names-report");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let spec_path = dir.join("health.flow.yaml");
    std::fs::write(
        &spec_path,
        "\
name: Health checks
app: api
steps:
  - assert_api:
      request: GET ${API_BASE}/health
      status: 200
",
    )
    .expect("spec written");

    let record = std::process::Command::new(FLOWPROOF_BIN)
        .args(["record", spec_path.to_str().expect("utf-8 path")])
        .env("API_BASE", &base)
        .output()
        .expect("record runs");
    assert!(
        record.status.success(),
        "record failed: {}",
        String::from_utf8_lossy(&record.stderr)
    );

    let run = std::process::Command::new(FLOWPROOF_BIN)
        .args(["run", spec_path.to_str().expect("utf-8 path")])
        .env("API_BASE", &base)
        .output()
        .expect("run runs");
    assert!(
        run.status.success(),
        "run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let verdict = stdout
        .lines()
        .find(|l| l.starts_with("PASS: "))
        .unwrap_or_else(|| panic!("no verdict line in:\n{stdout}"));
    assert!(
        verdict.ends_with("report.html"),
        "the verdict line must point a human at the HTML report, got: {verdict}"
    );

    // A path in the output that does not exist would be worse than none.
    let named = verdict
        .rsplit(" -> ")
        .next()
        .expect("verdict line has a path");
    assert!(
        std::path::Path::new(named).is_file(),
        "the named report must exist on disk: {named}"
    );

    server_thread.join().ok();
    std::fs::remove_dir_all(&dir).ok();
}
