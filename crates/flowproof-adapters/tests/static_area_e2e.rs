//! H4: does the web adapter's search reach content rendered OUTSIDE the
//! app's own DOM root - the pattern UI5 uses for `sap.m.Dialog`/`sap.m.Popover`
//! (rendered into `#sap-ui-static`, a SIBLING of the app root, not a
//! descendant of it)?
//!
//! Reproduced generically rather than requiring the full UI5 fixture: a
//! plain page whose "app" root has one button, and whose "static area" (a
//! sibling `<div>`, not a UI5 concept - just the same DOM shape) starts
//! empty and gets a new button appended to it only once the app button is
//! clicked - mirroring a dialog opening on demand outside the app subtree.

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

const FIXTURE: &str = r#"<!doctype html><html><body>
    <input id="space-toggle" type="checkbox">
    <div id="app">
      <button id="open" onclick="
        var d = document.createElement('div');
        d.setAttribute('aria-label', 'Static area');
        d.innerHTML = '<button id=&quot;confirm&quot;>Confirm delete</button>';
        document.body.appendChild(d);
      ">Open dialog</button>
    </div>
    </body></html>"#;

#[test]
fn search_reaches_content_outside_the_app_root() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping H4 static-area measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    use flowproof_driver::{AppDriver, UiaSelector};

    // The shared browser this launches has no destructor on normal process
    // exit (see SharedBrowserGuard's own doc comment) - without this, every
    // e2e test run leaked its own Chrome process tree and profile dir.
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    driver
        .launch(&serve(FIXTURE), "", std::time::Duration::from_secs(30))
        .expect("page opens");

    assert!(
        !driver
            .element_exists(&UiaSelector::css("#confirm"))
            .unwrap_or(true),
        "the static-area button must not exist yet, or this test proves nothing"
    );
    driver
        .invoke(&UiaSelector::css("#open"))
        .expect("opening the dialog succeeds");

    // Space aliases must produce real keyboard activation, not text insertion.
    driver
        .invoke(&UiaSelector::css("#space-toggle"))
        .expect("focus checkbox");
    assert!(driver
        .element_exists(&UiaSelector::css("#space-toggle:checked"))
        .expect("checked"));
    for key in ["Space", "Spacebar", " "] {
        driver
            .press_key(key, &[])
            .expect("space key activates checkbox");
        assert!(driver
            .element_exists(&UiaSelector::css("#space-toggle:not(:checked)"))
            .expect("unchecked"));
        driver.press_key(key, &[]).expect("space toggles back");
        assert!(driver
            .element_exists(&UiaSelector::css("#space-toggle:checked"))
            .expect("checked again"));
    }

    // Native-id (css) reach: the sibling div is appended to <body>, a
    // structural ancestor of neither #app nor anything inside it.
    assert!(
        driver
            .element_exists(&UiaSelector::css("#confirm"))
            .expect("css lookup does not error"),
        "css search must reach a sibling of the app root, not just its descendants"
    );

    // a11y reach: the same element, found by role+name instead of id -
    // proves the accessibility-tree path (the a11y tier's own resolution)
    // isn't scoped to the app root either.
    let a11y_selector = UiaSelector {
        control_type: None,
        role: Some("button".into()),
        name: Some("Confirm delete".into()),
        ..UiaSelector::default()
    };
    assert!(
        driver
            .element_exists(&a11y_selector)
            .expect("a11y lookup does not error"),
        "the a11y rung must also reach a sibling of the app root"
    );
}
