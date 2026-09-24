//! SAP-style empty custom editors must survive a crowded scene inventory.
use flowproof_driver::AppDriver;

#[test]
fn empty_grid_editors_have_column_labels_and_row_identity() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let toolbar = (0..120)
        .map(|i| format!("<span id='tool{i}'>Tool {i}</span>"))
        .collect::<String>();
    let html = format!(
        r#"<html><body>{toolbar}
      <table><tr><th id="material-label">Material</th><th id="plant-label">Plant</th></tr>
      <tr><td role="gridcell" lsmatrixrowindex="1"><span id="material" role="textbox" aria-labelledby="material-label" tabindex="0" style="display:block;width:100px;height:24px"></span></td>
      <td role="gridcell" lsmatrixrowindex="1"><span id="plant" role="textbox" aria-labelledby="plant-label" tabindex="0" style="display:block;width:100px;height:24px"></span></td></tr></table>
      <input type="password" value="never-export-this">
      </body></html>"#
    );
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
    let scene = driver.scene().expect("scene reads").expect("scene exists");
    let entries: serde_json::Value = serde_json::from_str(&scene).expect("scene JSON");
    for (id, label) in [("material", "Material"), ("plant", "Plant")] {
        let entry = entries
            .as_array()
            .expect("scene entries")
            .iter()
            .find(|e| e["target"] == format!("css:#{id}"))
            .expect("empty editor in scene");
        assert_eq!(entry["label"], label);
        assert_eq!(entry["row"], "1");
        assert_eq!(entry["actionable"], true);
        assert_eq!(entry["role"], "textbox");
    }
    assert!(!scene.contains("never-export-this"));
}

/// SAP WebGUI names many fields only by `title`. The authoring model must
/// see that name, or the only way it can address the field is by id.
#[test]
fn fields_named_only_by_title_carry_that_name_into_the_scene() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let html = r#"<html><body><div id="webguiPage0">
      <input id="M0:46:::0:34" title="Purchase Order">
      <input id="labelled" title="Ignored" aria-label="Posting Date">
      </div></body></html>"#;
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
    let scene = driver.scene().expect("scene reads").expect("scene exists");
    let entries: serde_json::Value = serde_json::from_str(&scene).expect("scene JSON");
    let label_of = |fragment: &str| {
        entries
            .as_array()
            .expect("scene entries")
            .iter()
            .find(|e| e["css"].as_str().is_some_and(|c| c.contains(fragment)))
            .map(|e| e["label"].clone())
            .expect("field in scene")
    };
    assert_eq!(label_of("M0"), "Purchase Order");
    assert_eq!(
        label_of("labelled"),
        "Posting Date",
        "an explicit name still wins"
    );
}
