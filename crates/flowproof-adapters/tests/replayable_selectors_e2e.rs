#![cfg(feature = "web")]

//! Stadtwerke Flensburg's price calculator, reduced to its two traps: a
//! dialog that is shown while it still says `aria-hidden="true"`, holding a
//! field whose id is minted on every page load. Recording it wrote a
//! selector of each kind, and replay found the field with neither.

use flowproof_driver::{AppDriver, UiaSelector};

const FIXTURE: &str =
    "data:text/html,<main><aside role='dialog' aria-label='Rechner' aria-hidden='true'>\
<label id='plz-label' for='plz'>PLZ und Ort</label><input id='plz'></aside></main>\
<script>\
const field = document.getElementById('plz');\
field.id = 'user-widget-location-6a' + Date.now().toString(16);\
document.getElementById('plz-label').htmlFor = field.id;\
</script>";

const PER_LOAD: &str = "user-widget-location-6a";

#[test]
fn a_field_with_a_per_load_id_in_an_aria_hidden_dialog_is_found_after_a_reload() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .launch(FIXTURE, "", std::time::Duration::from_secs(10))
        .expect("fixture opens");

    // The target the authoring model would be offered for the field.
    let scene = driver.scene().expect("scene reads").expect("scene exists");
    let entries: Vec<serde_json::Value> = serde_json::from_str(&scene).expect("scene JSON");
    let field = entries
        .iter()
        .filter_map(|e| {
            e["target"]
                .as_str()?
                .strip_prefix("css:")
                .map(str::to_string)
        })
        .map(UiaSelector::css)
        .find(|selector| {
            driver
                .element_attribute(selector, "id")
                .ok()
                .flatten()
                .is_some_and(|id| id.starts_with(PER_LOAD))
        })
        .expect("the field is offered");
    let css = field.css.clone().unwrap_or_default();
    assert!(!css.contains(PER_LOAD), "a per-load id was offered: {css}");

    // Typing leaves focus in the field, where Chrome shows it to the
    // accessibility tree despite the dialog's aria-hidden; replay starts
    // with nothing focused, so no hint may be recorded from that moment.
    driver.type_text(&field, "24944").expect("types");
    assert_eq!(
        driver.a11y_hint(&field).expect("reads the hint"),
        None,
        "a hint only visible while focused cannot be replayed"
    );

    // A new load mints a new id; the recorded target still finds the field.
    driver.reload().expect("reloads");
    assert!(
        driver.element_exists(&field).expect("looks"),
        "{css} no longer finds the field after a reload"
    );
}
