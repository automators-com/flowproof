//! Hidden password fields must not discard otherwise safe screenshots.
use flowproof_driver::{AppDriver, UiaSelector};

/// Serve a static page on its own loopback port and return the port.
fn serve(html: &'static str) -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let port = listener.local_addr().expect("fixture address").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut stream in listener.incoming().flatten() {
            let mut request = [0; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", html.len(), html);
        }
    });
    port
}

fn launch(port: u16) -> flowproof_adapters::WebAppDriver {
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("Chrome launches");
    driver
        .launch(
            &format!("http://127.0.0.1:{port}"),
            "",
            std::time::Duration::from_secs(30),
        )
        .expect("fixture loads");
    driver
}

#[test]
fn hidden_password_inputs_do_not_drop_frames_and_visible_passwords_stay_masked() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let html = r#"<html><body><input type='password' value='visible-secret' style='width:200px;height:40px'><section id='later' style='display:none'><input type='password' value='hidden-secret'></section><button id='reveal' onclick="document.getElementById('later').style.display='block'">Reveal</button></body></html>"#;
    let port = serve(html);
    let mut driver = launch(port);

    let rects = flowproof_driver::redact::resolve_rects(&mut driver, &[])
        .expect("hidden inputs are safe to skip");
    assert_eq!(rects.len(), 1);
    let mut frame = driver
        .capture()
        .expect("screenshot captured")
        .expect("web screenshots supported");
    flowproof_driver::redact::apply(&mut frame, &rects);
    let (x, y, w, h) = rects[0];
    assert!(w > 0 && h > 0);
    for py in y.max(0) as u32..((y.max(0) as u32 + h).min(frame.height())) {
        for px in x.max(0) as u32..((x.max(0) as u32 + w).min(frame.width())) {
            assert_eq!(frame.get_pixel(px, py).0, [0, 0, 0, 255]);
        }
    }
    driver
        .invoke(&UiaSelector::css("#reveal"))
        .expect("reveal password field");
    assert_eq!(
        flowproof_driver::redact::resolve_rects(&mut driver, &[])
            .expect("visible password rectangles resolved")
            .len(),
        2
    );
}

#[test]
fn pages_without_password_inputs_resolve_to_no_masks() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    // No password input anywhere: the common case must resolve to no masks,
    // not fail the resolve (which would drop every recorded frame).
    let port = serve(r#"<html><body><div>nothing sensitive</div></body></html>"#);
    let mut driver = launch(port);
    assert_eq!(
        flowproof_driver::redact::resolve_rects(&mut driver, &[])
            .expect("password-less pages resolve their masks"),
        Vec::new()
    );
}
