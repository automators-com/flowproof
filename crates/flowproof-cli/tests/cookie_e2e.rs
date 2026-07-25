//! Cookie assertions, end to end in a real browser against a server that
//! sets real `Set-Cookie` headers. The control under test is a security one:
//! "the session cookie is httpOnly" regresses silently when an auth config
//! changes, with no visible change to the UI. Gated on FLOWPROOF_E2E=1.

use flowproof_agent::FlowSpec;

/// Serve one page that sets three cookies with different flags, so a single
/// flow can prove each fact independently. `file://` has no cookie jar, so
/// this has to be real HTTP.
fn serve_cookies() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming().take(16) {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let body = "<!doctype html><html><body><h1>Signed in</h1></body></html>";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\
                 Set-Cookie: session_token=abc123; HttpOnly; Path=/\r\n\
                 Set-Cookie: remember_me=yes; Path=/; Max-Age=86400\r\n\
                 Set-Cookie: locale=en; Path=/\r\n\
                 Content-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://127.0.0.1:{port}")
}

const COOKIE_SPEC: &str = r#"
name: session cookie controls
app: web
url: __URL__
control:
  id: sec.session.cookie-flags
  title: The session cookie is not readable by page scripts
steps:
  - assert: cookie "session_token" exists
  - assert: cookie "session_token" is httpOnly
  # A cookie with an explicit Max-Age outlives the browser session.
  - assert: cookie "remember_me" is persistent
"#;

/// The control failing is the case that matters: a cookie that IS set but
/// is readable by scripts must fail, not pass because it exists.
const NOT_HTTPONLY_SPEC: &str = r#"
name: a readable session cookie fails
app: web
url: __URL__
steps:
  - assert: cookie "locale" is httpOnly
"#;

const MISSING_SPEC: &str = r#"
name: a cookie that was never set
app: web
url: __URL__
steps:
  - assert: cookie "nope" exists
"#;

fn skip() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping cookie E2E: set FLOWPROOF_E2E=1 to run it");
        return true;
    }
    false
}

fn spec_for(yaml: &str, url: &str) -> FlowSpec {
    FlowSpec::parse(&yaml.replace("__URL__", url)).expect("spec parses")
}

fn dir_for(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-cookie-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

#[test]
fn cookie_flag_controls_record_and_replay() {
    if skip() {
        return;
    }
    let url = serve_cookies();
    let dir = dir_for("flags");
    let trace = dir.join("cookie.trace.jsonl");
    let spec = spec_for(COOKIE_SPEC, &url);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 3);
    drop(driver);

    // The NAME and the FACT travel. The value must not: this trace is meant
    // to be safe to commit and to attach to a bug report.
    let recorded = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        recorded.contains("\"cookie\":\"session_token\""),
        "{recorded}"
    );
    assert!(
        recorded.contains("\"cookie_fact\":\"http_only\""),
        "{recorded}"
    );
    assert!(
        !recorded.contains("abc123"),
        "a cookie VALUE must never reach the trace: {recorded}"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "cookie controls must replay: {report:#?}");
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_script_readable_cookie_fails_the_httponly_control() {
    if skip() {
        return;
    }
    let url = serve_cookies();
    let dir = dir_for("readable");
    let trace = dir.join("readable.trace.jsonl");
    let spec = spec_for(NOT_HTTPONLY_SPEC, &url);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace)
        .expect_err("a readable cookie must fail the control");
    let message = err.to_string();
    assert!(message.contains("not httpOnly"), "{message}");
    // The cookie is set, so the failure must not read as "missing".
    assert!(!message.contains("never set"), "{message}");
    assert!(
        !message.contains("en"),
        "no value in the failure: {message}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_missing_cookie_names_the_ones_that_are_set() {
    if skip() {
        return;
    }
    let url = serve_cookies();
    let dir = dir_for("missing");
    let trace = dir.join("missing.trace.jsonl");
    let spec = spec_for(MISSING_SPEC, &url);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace)
        .expect_err("a cookie that was never set must fail");
    let message = err.to_string();
    assert!(message.contains("never set"), "{message}");
    // Names are the affordance that fixes a typo. Values never appear.
    assert!(message.contains("session_token"), "{message}");
    assert!(
        !message.contains("abc123"),
        "names only, never values: {message}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}
