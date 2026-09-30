//! `WebAppDriver::fingerprint` against a real browser (plans/014): what the
//! target is, labels never values. Opt-in via FLOWPROOF_E2E=1, like
//! `a11y_capture.rs`.

use flowproof_driver::{AppDriver, UiaSelector};

const FIXTURE: &str = r#"<!doctype html><html><head><title>Enter Vehicle Data</title></head><body>
    <label for="make">Make</label>
    <select id="make" name="make"><option>Audi</option><option>Volvo</option></select>
    <label for="user">User</label>
    <input id="user" name="username" value="typed-secret-4711">
    <button id="next">  Next
      step </button>
    </body></html>"#;

fn serve(html: &'static str) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n{}",
                    html.len(),
                    html
                )
                .as_bytes(),
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[test]
fn fingerprints_describe_the_control_and_never_its_value() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping fingerprint capture: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    let origin = serve(FIXTURE);
    driver
        .launch(&origin, "", std::time::Duration::from_secs(30))
        .expect("page opens");
    let mut fingerprint = |css: &str| {
        driver
            .fingerprint(&UiaSelector::css(css))
            .expect("fingerprint does not error")
            .expect("the control is described")
    };

    let make = fingerprint("#make");
    assert_eq!(
        make.kind.as_deref(),
        Some("select"),
        "a dropdown, not a text box"
    );
    assert_eq!(make.name.as_deref(), Some("make"));
    assert_eq!(
        make.label.as_deref(),
        Some("Make"),
        "its label, not its options"
    );
    assert_eq!(make.title.as_deref(), Some("Enter Vehicle Data"));
    assert_eq!(make.app.as_deref(), Some(origin.as_str()));

    let user = fingerprint("#user");
    assert_eq!(user.kind.as_deref(), Some("input[text]"));
    assert_eq!(user.label.as_deref(), Some("User"));
    assert!(
        !format!("{user:?}").contains("typed-secret-4711"),
        "an input's value is never read: {user:?}"
    );

    let next = fingerprint("#next");
    assert_eq!(next.kind.as_deref(), Some("button"));
    assert_eq!(
        next.label.as_deref(),
        Some("Next step"),
        "whitespace collapsed"
    );
}
