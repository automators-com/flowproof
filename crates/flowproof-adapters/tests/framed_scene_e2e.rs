//! The scene travels to the authoring model while recording, so a password's
//! value must never be in it. The page-level inventory has always held that
//! line; the same-origin iframe pass read `el.value` for every field,
//! password included, so a login form inside an iframe sent what was typed
//! into it.
#![cfg(feature = "web")]

use flowproof_driver::AppDriver;

#[test]
fn a_password_typed_inside_an_iframe_never_reaches_the_scene() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let html = r#"<html><body>
      <label for="mainpw">Main password</label>
      <input id="mainpw" type="password" value="page-secret-111">
      <iframe title="login" style="width:400px;height:200px" srcdoc='<body>
        <label for="user">User</label><input id="user" value="alice">
        <label for="pw">Password</label><input id="pw" type="password" value="framed-secret-222">
        <button id="go">Sign in</button></body>'></iframe>
      </body></html>"#;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("fixture listener");
    let port = listener.local_addr().expect("listener address").port();
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut stream in listener.incoming().flatten() {
            let mut buf = [0; 2048];
            let _ = stream.read(&mut buf);
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
    // The iframe document loads after the page; read until its fields are in.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let (scene, entries) = loop {
        let scene = driver.scene().expect("scene reads").expect("scene exists");
        let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("scene JSON");
        let framed = entries
            .iter()
            .any(|e| e["target"] == r#"framed:"login" > css:#pw"#);
        if framed || std::time::Instant::now() > deadline {
            break (scene, entries);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let entry = |target: &str| {
        entries
            .iter()
            .find(|e| e["target"] == target)
            .unwrap_or_else(|| panic!("{target} missing from scene: {scene}"))
    };

    // The framed password field is still addressable by its label...
    let password = entry(r#"framed:"login" > css:#pw"#);
    assert_eq!(password["label"], "Password");
    assert!(
        password.get("text").is_none(),
        "password value leaked: {password}"
    );
    // ...an ordinary framed field still reports its value...
    assert_eq!(entry(r#"framed:"login" > css:#user"#)["text"], "alice");
    // ...and neither secret appears anywhere in what goes to the model.
    assert!(
        !scene.contains("framed-secret-222"),
        "framed password in scene"
    );
    assert!(!scene.contains("page-secret-111"), "page password in scene");
}
