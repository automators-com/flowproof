use flowproof_driver::AppDriver;

#[test]
fn empty_and_populated_lists_offer_grounded_collection_targets() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .launch(
            "data:text/html,<input id='tick' type='checkbox' aria-label='Toggle task' style='opacity:0;width:30px;height:30px'><input id='hidden' type='checkbox' style='display:none'><button id='remove' onclick='document.getElementById(&quot;steps&quot;).remove()'>Remove</button><ul id='tasks'></ul><ol id='steps'><li>One</li><li>Two</li></ol>",
            "",
            std::time::Duration::from_secs(10),
        )
        .expect("fixture opens");
    let scene = driver.scene().expect("scene reads").expect("scene exists");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("scene JSON");
    let tick = entries
        .iter()
        .find(|e| e["target"] == "css:#tick")
        .expect("transparent hit-testable checkbox offered");
    assert_eq!(tick["actionable"], true);
    assert_eq!(tick["checked"], false);
    assert!(!entries.iter().any(|e| e["target"] == "css:#hidden"));
    for (id, count) in [("tasks", 0), ("steps", 2)] {
        let token = format!("css:#{id} > :is(li, [role=\"listitem\"])");
        let entry = entries
            .iter()
            .find(|e| e["target"] == token)
            .expect("collection target exists even when empty");
        assert_eq!(entry["count"], count);
        assert_eq!(entry["actionable"], false);
    }
    driver
        .invoke(&flowproof_driver::UiaSelector::css("#remove"))
        .expect("remove populated list");
    let scene = driver
        .scene()
        .expect("scene after removal")
        .expect("scene exists");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("scene JSON");
    let previous = entries
        .iter()
        .find(|e| e["target"] == "css:#steps > :is(li, [role=\"listitem\"])")
        .expect("previous list retained");
    assert_eq!(previous["count"], 0);
    assert_eq!(previous["observed_before"], true);
}
