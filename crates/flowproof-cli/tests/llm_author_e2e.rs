//! End-to-end proof of the LLM authoring loop: natural UI steps get authored
//! against real headless Chromium, while structured assertions stay
//! deterministic. The resulting standard trace then replays without a model.
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

const SCOPED_CAPTURE_HTML: &str = r##"<!doctype html>
<html><body>
  <div id="rows">
    <div class="row propertyGrid">
      <div class="col-md-4 bg-info border">aria-busy</div>
      <div class="col-md-4 border">false</div>
    </div>
    <div class="row propertyGrid">
      <div class="col-md-4 bg-info border">role</div>
      <div class="col-md-4 border">button</div>
    </div>
  </div>
  <a id="generate" href="#">Generate order ID</a>
  <input id="offerId" placeholder="Get order id from table and enter it here!">
  <p id="status"></p>
  <script>
    let currentOrderId = '';
    document.getElementById('generate').addEventListener('click', event => {
      event.preventDefault();
      currentOrderId = String(Date.now()).slice(-7);
      const row = document.createElement('div');
      row.className = 'row propertyGrid';
      row.innerHTML = '<div class="col-md-4 bg-info border">order id</div>' +
        '<div class="col-md-4 border">' + currentOrderId + '</div>';
      const rows = document.getElementById('rows');
      rows.insertBefore(row, rows.children[1]);
    });
    document.getElementById('offerId').addEventListener('input', event => {
      document.getElementById('status').textContent = event.target.value === currentOrderId
        ? 'You solved this automation problem' : '';
    });
  </script>
</body></html>"##;

const SCOPED_ORDER_TOKEN: &str = r#"scoped:css:div.row.propertyGrid containing "order id" > css:div.col-md-4.border:not(.bg-info)"#;

const HUMAN_PRIMITIVES_HTML: &str = r##"<!doctype html>
<html><body>
  <div id="task1" draggable="true">task 1</div><div id="todo">todo drop area</div>
  <button id="half" style="width:200px">Click into my right half</button>
  <table id="rows"><tr><th>row</th></tr><tr><td>A</td></tr><tr><td>B</td></tr><tr><td>C</td></tr></table>
  <input id="rowCount" placeholder="row-count field">
  <select id="methods" multiple>
    <option>Functional testing</option><option>End2End testing</option>
    <option>GUI testing</option><option>Exploratory testing</option>
  </select>
  <iframe id="container" style="height:100px" srcdoc='<body style="height:500px;overflow:auto"><input id="textfield" style="margin-top:300px" placeholder="embedded text field"><script>document.querySelector("#textfield").addEventListener("input",e=>{if(e.target.value==="Tosca"){parent.mark("scroll");parent.mark("frame")}})</script></body>'></iframe>
  <input id="first" placeholder="first field"><input id="second" placeholder="next field">
  <p id="status"></p>
  <script>
    const flags = {drag:false, half:false, count:false, select:false, scroll:false, frame:false, tab:false};
    function done() {
      const output = document.getElementById('status');
      output.textContent = Object.entries(flags).filter(([, value]) => value).map(([name]) => name + ' ok').join(' ');
      if (Object.values(flags).every(Boolean)) output.textContent += ' You solved this automation problem';
    }
    window.mark = name => { flags[name] = true; done(); };
    task1.addEventListener('dragstart', e => e.dataTransfer.setData('text/plain', 'task1'));
    todo.addEventListener('dragover', e => e.preventDefault());
    todo.addEventListener('drop', e => { e.preventDefault(); todo.appendChild(task1); mark('drag'); });
    half.addEventListener('click', e => { if (e.offsetX > half.clientWidth / 2) mark('half'); });
    rowCount.addEventListener('input', e => { if (e.target.value === '4') mark('count'); });
    methods.addEventListener('change', () => {
      if (Array.from(methods.selectedOptions).length === 4) mark('select');
    });
    second.addEventListener('focus', () => mark('tab'));
  </script>
</body></html>"##;

const FRAME_BODY_TOKEN: &str = r#"framed:"container" > css:body"#;
const FRAME_FIELD_TOKEN: &str = r#"framed:"container" > css:#textfield"#;

/// Natural UI steps for the model, followed by a deterministic assertion.
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
        mcp: Vec::new(),
        strict: false,
        control: None,
        steps: vec![
            SpecStep::Plain("Put Ada into the box labelled with the name".into()),
            SpecStep::Plain("Smash the greeting button".into()),
            SpecStep::Assert {
                assert: "page shows Hello, Ada".into(),
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
            let reply = if body.contains("Current step to perform: Put Ada into the box") {
                r##"{"action":"type_text","target":"css:#name","text":"Ada"}"##
            } else if body.contains("Current step to perform: Smash the greeting button") {
                r##"{"action":"click","target":"css:#greet"}"##
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
            if bodies.len() >= 2 {
                break;
            }
        }
        bodies
    })
}

fn serve_scoped_capture(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut bodies = Vec::new();
        while let Ok(mut request) = server.recv() {
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let parsed: serde_json::Value = serde_json::from_str(&body).expect("request is JSON");
            let prompt = parsed["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .expect("request carries the user prompt");
            let reply = if prompt
                .contains("Current step to perform: Click the generate order ID control")
            {
                serde_json::json!({"action":"click", "target":"css:#generate"})
            } else if prompt.contains(
                "Current step to perform: Remember the value beside \"order id\" as the order ID",
            ) {
                serde_json::json!({
                    "action":"capture_text",
                    "target":SCOPED_ORDER_TOKEN,
                    "name":"order_id"
                })
            } else if prompt
                .contains("Current step to perform: Enter the order ID in the destination field")
            {
                serde_json::json!({
                    "action":"type_captured",
                    "target":"css:#offerId",
                    "capture":"order_id"
                })
            } else {
                serde_json::json!({"action":"click", "target":"css:#nonsense"})
            };
            let payload = serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": reply.to_string()}}]
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

fn serve_human_primitives(server: tiny_http::Server) -> std::thread::JoinHandle<Vec<String>> {
    std::thread::spawn(move || {
        let mut bodies = Vec::new();
        while let Ok(mut request) = server.recv() {
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            let parsed: serde_json::Value = serde_json::from_str(&body).expect("request is JSON");
            let prompt = parsed["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .expect("request carries the user prompt");
            let reply = if prompt.contains("Current step to perform: Drag task 1") {
                serde_json::json!({"action":"drag","target":"css:#task1","onto":"css:#todo"})
            } else if prompt.contains("Current step to perform: Click the right half") {
                serde_json::json!({"action":"click_at","target":"css:#half","x_pct":75,"y_pct":50})
            } else if prompt.contains("Current step to perform: Remember the number") {
                serde_json::json!({"action":"capture_count","target":"css:#rows tr","name":"row_count"})
            } else if prompt.contains("Current step to perform: Enter the remembered row count") {
                serde_json::json!({"action":"type_captured","target":"css:#rowCount","capture":"row_count"})
            } else if prompt.contains("Current step to perform: Select Functional") {
                serde_json::json!({"action":"select_options","target":"css:#methods","values":["Functional testing","End2End testing","GUI testing","Exploratory testing"]})
            } else if prompt.contains("Current step to perform: Scroll the embedded") {
                serde_json::json!({"action":"scroll","target":FRAME_BODY_TOKEN,"to_px":147})
            } else if prompt.contains("Current step to perform: Enter Tosca") {
                serde_json::json!({"action":"type_text","target":FRAME_FIELD_TOKEN,"text":"Tosca"})
            } else if prompt.contains("Current step to perform: Click the first field") {
                serde_json::json!({"action":"click","target":"css:#first"})
            } else if prompt.contains("Current step to perform: Move focus") {
                serde_json::json!({"action":"press_key","key":"Tab"})
            } else {
                serde_json::json!({"action":"click","target":"css:#nonsense"})
            };
            let payload = serde_json::json!({
                "choices": [{"message": {"role": "assistant", "content": reply.to_string()}}]
            });
            let response = tiny_http::Response::from_string(payload.to_string()).with_header(
                tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                    .expect("header"),
            );
            bodies.push(body);
            request.respond(response).ok();
            if bodies.len() >= 9 {
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
    assert_eq!(bodies.len(), 2);
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
fn authors_scoped_capture_from_human_language() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping scoped-capture E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-llm-scoped-capture-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("scoped-capture.html");
    std::fs::write(&page, SCOPED_CAPTURE_HTML).expect("page written");
    let trace_path = dir.join("scoped-capture.trace.jsonl");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let server_thread = serve_scoped_capture(server);
    let spec = FlowSpec {
        name: "Scoped capture freeform".into(),
        app: "web".into(),
        url: Some(format!("file://{}", page.display())),
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
            SpecStep::Plain("Click the generate order ID control".into()),
            SpecStep::Plain("Remember the value beside \"order id\" as the order ID".into()),
            SpecStep::Plain("Enter the order ID in the destination field".into()),
            SpecStep::Assert {
                assert: "page shows You solved this automation problem".into(),
            },
        ],
    };
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
    .expect("human-language scoped capture records");
    drop(driver);

    let bodies = server_thread.join().expect("server thread");
    assert_eq!(bodies.len(), 3);
    assert!(
        bodies[1].contains("scoped:css:div.row.propertyGrid")
            && bodies[1].contains("order id")
            && bodies[1].contains("css:div.col-md-4.border:not(.bg-info)"),
        "the generated value must be listed as a scoped model target"
    );
    let trace = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(trace.contains("scoped"), "trace persists scoped resolution");
    assert!(
        !trace.contains(SCOPED_ORDER_TOKEN),
        "synthetic authoring token must not enter the trace"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "scoped capture must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn human_language_primitives_record_and_replay_without_rule_inputs() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping human-primitives E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-human-primitives-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("human-primitives.html");
    std::fs::write(&page, HUMAN_PRIMITIVES_HTML).expect("page written");
    let trace_path = dir.join("human-primitives.trace.jsonl");
    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let server_thread = serve_human_primitives(server);
    let plain = [
        "Drag task 1 into the todo drop area",
        "Click the right half of \"Click into my right half\"",
        "Remember the number of displayed table rows as the row count",
        "Enter the remembered row count in the row-count field",
        "Select Functional, End2End, GUI, and Exploratory testing together",
        "Scroll the embedded challenge to 147 pixels",
        "Enter Tosca in the text field inside the embedded challenge",
        "Click the first field",
        "Move focus to the next field",
    ];
    assert!(plain.iter().all(|step| {
        !step.contains("rules:") && !step.contains("css:") && !step.contains("id:")
    }));
    let mut spec = freeform_spec(format!("file://{}", page.display()));
    spec.name = "Human primitive matrix".into();
    let assertion = |text: &str| SpecStep::Assert {
        assert: format!("page shows {text}"),
    };
    spec.steps = vec![
        SpecStep::Plain(plain[0].into()),
        assertion("drag ok"),
        SpecStep::Plain(plain[1].into()),
        assertion("half ok"),
        SpecStep::Plain(plain[2].into()),
        SpecStep::Plain(plain[3].into()),
        assertion("count ok"),
        SpecStep::Plain(plain[4].into()),
        assertion("select ok"),
        SpecStep::Plain(plain[5].into()),
        SpecStep::Plain(plain[6].into()),
        assertion("frame ok"),
        SpecStep::Plain(plain[7].into()),
        SpecStep::Plain(plain[8].into()),
        assertion("tab ok"),
        assertion("You solved this automation problem"),
    ];
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
    .expect("plain human primitives record");
    drop(driver);

    let bodies = server_thread.join().expect("server thread");
    assert_eq!(bodies.len(), 9);
    let prompts: Vec<String> = bodies
        .iter()
        .map(|body| {
            let parsed: serde_json::Value = serde_json::from_str(body).expect("request is JSON");
            parsed["messages"]
                .as_array()
                .and_then(|messages| messages.last())
                .and_then(|message| message["content"].as_str())
                .expect("request carries the user prompt")
                .to_string()
        })
        .collect();
    assert!(
        prompts
            .iter()
            .any(|prompt| prompt.contains(r#"framed:\"container\" > css:#textfield"#)),
        "framed field target missing from prompts: {prompts:#?}"
    );
    assert!(prompts.iter().any(|prompt| prompt.contains("css:#rows tr")));
    let trace = std::fs::read_to_string(&trace_path).expect("trace readable");
    assert!(
        !trace.contains("framed:\""),
        "synthetic frame tokens stay out of traces"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "plain human primitives must replay: {report:#?}"
    );
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
