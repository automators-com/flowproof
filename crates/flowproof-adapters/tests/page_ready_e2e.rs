#![cfg(feature = "web")]

//! Stadtwerke Flensburg's "Produkte" menu, reduced: its click handler is
//! attached when the page has finished loading, and the page finishes
//! loading only once a slow image arrives. Replay clicked the moment the
//! button existed, before the handler did, and the menu never opened.

use flowproof_driver::{AppDriver, UiaSelector};

/// Serves `MENU` at `/menu`, an image at `/slow` after `delay_ms`, and a
/// page linking to the menu everywhere else; one request at a time, like the
/// network-idle fixture.
fn serve(delay_ms: u64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            let slow = request.starts_with("GET /slow");
            if slow {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            let (content_type, body) = if slow {
                ("image/gif", "")
            } else if request.starts_with("GET /menu") {
                ("text/html", MENU)
            } else {
                ("text/html", START)
            };
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            );
        }
    });
    format!("http://127.0.0.1:{port}/")
}

/// Replay's failure came on a page reached by a click, so the test gets
/// there the same way.
const START: &str = r#"<!doctype html><html><body><a id="go" href="/menu">Strom</a></body></html>"#;

const MENU: &str = r#"<!doctype html><html><body>
<button id="menu-button">Produkte</button>
<nav id="menu" hidden><a id="item">Haushaltsstrom</a></nav>
<img src="/slow" alt="">
<script>
  window.addEventListener('load', () => {
    document.getElementById('menu-button').addEventListener('click', () => {
      document.getElementById('menu').hidden = false;
    });
  });
</script>
</body></html>"#;

#[test]
fn a_step_waits_for_its_page_to_finish_loading_before_it_clicks() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .launch(&serve(1500), "", std::time::Duration::from_secs(15))
        .expect("page opens");
    driver
        .invoke(&UiaSelector::css("#go"))
        .expect("follows the link");
    // Replay's own loop: the gate is asked until it lets the step through.
    let button = UiaSelector::css("#menu-button");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while driver.actionability_gate(&button).expect("gate").is_some() {
        assert!(
            std::time::Instant::now() < deadline,
            "the gate never opened"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    driver.invoke(&button).expect("clicks");
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert_eq!(
        driver
            .element_visible(&UiaSelector::css("#item"))
            .expect("looks"),
        Some(true),
        "the click landed before the page had wired up its menu"
    );
    // The page is waited for once: a second step on it does not pay again.
    let started = std::time::Instant::now();
    assert_eq!(driver.actionability_gate(&button).expect("gate"), None);
    assert!(started.elapsed() < std::time::Duration::from_secs(1));
}

/// The page's size is what recording writes into a trace that pins no
/// viewport, and staging that size is how replay gets it back: a headless
/// Chrome, whose own window is small, then lays the page out at the size a
/// full-screen recording had.
#[test]
fn a_staged_page_size_is_the_size_the_page_reports() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .stage_browser(flowproof_driver::WebBrowserConfig::from_setup_parts(
            Some((1512, 857, Some(2.0), None, None)),
            None,
            &[],
            None,
            None,
            None,
        ))
        .expect("stages");
    driver
        .launch(
            "data:text/html,<p>sized</p>",
            "",
            std::time::Duration::from_secs(10),
        )
        .expect("page opens");
    assert_eq!(driver.page_size().expect("reads"), Some((1512, 857, 2.0)));
}
