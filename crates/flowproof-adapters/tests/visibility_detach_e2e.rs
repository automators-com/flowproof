//! Visibility must tolerate a dialog disappearing after the existence probe.
use flowproof_driver::{AppDriver, UiaSelector};

#[test]
fn removed_element_is_not_visible_without_swallowing_browser_errors() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let html = "<html><body><div id='dialog'>Receipt checked</div><div id='hidden' style='display:none'>Hidden</div><button id='close' onclick=\"document.getElementById('dialog').remove()\">Close</button></body></html>";
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
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("Chrome launches");
    driver
        .launch(
            &format!("http://127.0.0.1:{port}"),
            "",
            std::time::Duration::from_secs(30),
        )
        .expect("fixture loads");
    let dialog = UiaSelector::css("#dialog");
    assert!(driver.element_exists(&dialog).expect("dialog existence"));
    assert_eq!(
        driver.element_visible(&dialog).expect("visible dialog"),
        Some(true)
    );
    assert_eq!(
        driver
            .element_visible(&UiaSelector::css("#hidden"))
            .expect("hidden element visibility"),
        Some(false)
    );
    driver
        .invoke(&UiaSelector::css("#close"))
        .expect("close dialog");
    assert_eq!(
        driver
            .element_visible(&dialog)
            .expect("removed element visibility"),
        Some(false)
    );
    assert_eq!(
        driver
            .element_visible(&UiaSelector::css("#never-present"))
            .expect("absent element visibility"),
        Some(false)
    );
    assert!(driver.element_visible(&UiaSelector::css("[")).is_err());
}
