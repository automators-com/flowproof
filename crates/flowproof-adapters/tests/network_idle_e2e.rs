//! H2's fix, measured against a real delayed network response rather than
//! only the synthetic unit tests in `web.rs` - those prove the settle
//! LOGIC; this proves the CDP `Network` listener that feeds it actually
//! tracks a real in-flight request on a real page.
//!
//! docs/fiori-reliability/FINDINGS.md: Fiori fires an OData batch after DOM
//! ready, then re-renders - a growing table's real rows, a launchpad's real
//! tiles. This fixture reproduces the shape without SAP: a static page whose
//! only content arrives after a deliberately slow fetch.

fn serve(fast_html: &'static str, slow_body: &'static str, delay_ms: u64) -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            use std::io::{Read, Write};
            let mut buf = [0u8; 2048];
            let n = stream.read(&mut buf).unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
            let is_slow = request.starts_with("GET /slow");
            if is_slow {
                std::thread::sleep(std::time::Duration::from_millis(delay_ms));
            }
            let (content_type, body) = if is_slow {
                ("application/json", slow_body)
            } else {
                ("text/html", fast_html)
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
    format!("http://127.0.0.1:{port}")
}

const PAGE: &str = r#"<!doctype html><html><body>
    <script>
      fetch('/slow').then(() => {
        document.body.innerHTML += '<button id="loaded">Ready</button>';
      });
    </script>
    </body></html>"#;

#[test]
fn scene_waits_for_a_real_delayed_fetch_before_settling() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping network-idle measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    use flowproof_driver::AppDriver;

    let url = serve(PAGE, "{}", 2000);
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    // Measured from before `launch` itself, not just around `scene()`: the
    // page's script fires the fetch as soon as it parses, which can happen
    // during `launch`'s own navigation wait - the claim under test is that
    // the TOTAL time before flowproof considers the page ready spans the
    // real delay, not that any one call in isolation blocks for it.
    let started = std::time::Instant::now();
    driver
        .launch(&url, "", std::time::Duration::from_secs(30))
        .expect("page opens");
    let scene = driver.scene().expect("scene reads").unwrap_or_default();
    let elapsed = started.elapsed();

    assert!(
        scene.contains("loaded"),
        "the scene must include the button the slow fetch adds, not race ahead of it: {scene}"
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(1800),
        "settling in {elapsed:?} means the fetch was never actually waited for"
    );
}
