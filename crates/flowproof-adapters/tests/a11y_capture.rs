//! The `a11y` selector tier's capture path, against a real browser.
//!
//! docs/fiori-reliability/FINDINGS.md's design note for this tier: role +
//! accessible name (+ nearest named ancestor), read from the browser's own
//! computed accessibility tree via CDP, not a JS-side approximation. This
//! measures the actual mechanism (`WebAppDriver::a11y_hint`) against a real
//! Chromium instance rather than asserting it works from reading the code.

use flowproof_driver::{AppDriver, UiaSelector};

const FIXTURE: &str = r#"<!doctype html><html><body>
    <nav aria-label="Main">
      <button id="save" aria-label="Save the current order">Save</button>
      <div id="anon" style="width:10px;height:10px"></div>
    </nav>
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
fn an_aria_labelled_button_yields_role_and_name_with_its_named_ancestor() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping a11y capture measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    driver
        .launch(&serve(FIXTURE), "", std::time::Duration::from_secs(30))
        .expect("page opens");

    let hints = driver
        .a11y_hint(&UiaSelector::css("#save"))
        .expect("a11y_hint does not error")
        .expect("the button has a role and an accessible name");

    assert_eq!(hints.role, "button");
    assert_eq!(hints.name, "Save the current order");
    // The nearest named ancestor is the <nav aria-label="Main">, not the
    // unnamed <body>/<html> further up - proves the ancestor walk stops at
    // the first NAMED one rather than always landing on the root.
    assert_eq!(hints.ancestor_name.as_deref(), Some("Main"));
}

#[test]
fn an_element_with_no_accessible_name_yields_no_hint() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping a11y capture measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    driver
        .launch(&serve(FIXTURE), "", std::time::Duration::from_secs(30))
        .expect("page opens");

    // A bare, unlabelled <div> has no accessible name - graceful absence,
    // not an error, so the recorder falls through to the other rungs.
    let hints = driver
        .a11y_hint(&UiaSelector::css("#anon"))
        .expect("a11y_hint does not error");
    assert!(hints.is_none(), "expected no hint, got {hints:?}");
}
