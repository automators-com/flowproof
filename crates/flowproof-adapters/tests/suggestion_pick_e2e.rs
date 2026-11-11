#![cfg(feature = "web")]

use flowproof_driver::{AppDriver, UiaSelector};

/// Stadtwerke Flensburg's address fields, reduced: a bare input with no
/// ARIA beside a `role=listbox` of buttons that fills in after a request,
/// and a house number that stays disabled until a suggestion is picked.
/// Beside it: a plain field, a search box with a list, and a list that
/// offers the typed text twice - none of which may answer a pick.
///
/// The list is filled once typing pauses, as a real autocomplete does. Filled
/// after every keystroke instead, a late rebuild could replace the option
/// between finding it and clicking it ("Node is detached from document"),
/// which failed this test on main once.
const FIXTURE: &str = "data:text/html,<form>\
<fieldset><label for='plz'>PLZ und Ort</label><input id='plz' autocomplete='off'>\
<div role='listbox' id='plzlist' hidden></div></fieldset>\
<fieldset><input id='hausnr' aria-label='Hausnr' disabled></fieldset>\
<fieldset><input id='plain' aria-label='Plain'></fieldset>\
<div role='search'><input id='q' aria-label='Query'>\
<div role='listbox'><button type='button'>flowproof</button></div></div>\
<fieldset><input id='twice' aria-label='Twice'>\
<div role='listbox'><button type='button'>Nordstrasse</button><button type='button'>Nordstrasse</button></div></fieldset>\
</form><script>\
const plz = document.getElementById('plz'), list = document.getElementById('plzlist');\
let pending;\
plz.addEventListener('input', () => { clearTimeout(pending); pending = setTimeout(() => {\
  list.innerHTML = '';\
  for (const o of ['24937 Flensburg', '24939 Flensburg', '24944 Flensburg', '24955 Harrislee']) {\
    const b = document.createElement('button'); b.type = 'button'; b.textContent = o;\
    b.onclick = () => { plz.value = o; list.hidden = true; document.getElementById('hausnr').disabled = false; };\
    list.appendChild(b);\
  }\
  list.hidden = false;\
}, 400); });\
</script>";

fn text(label: &str) -> UiaSelector {
    UiaSelector {
        name: Some(label.into()),
        ..UiaSelector::default()
    }
}

#[test]
fn a_typed_value_is_offered_only_from_the_fields_own_unambiguous_list() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .launch(FIXTURE, "", std::time::Duration::from_secs(10))
        .expect("fixture opens");

    let plz = UiaSelector::css("#plz");
    driver.type_text(&plz, "24944 Flensburg").expect("types");
    let picked = driver
        .typed_suggestion(&plz, "24944 flensburg")
        .expect("reads suggestions");
    assert_eq!(picked.as_deref(), Some("24944 Flensburg"));
    // The label is what the recorder clicks by text: it must land on the
    // option, and the pick is what unlocks the next field.
    driver.invoke(&text("24944 Flensburg")).expect("picks");
    assert!(driver
        .element_enabled(&UiaSelector::css("#hausnr"))
        .expect("reads enabled"));

    for (field, typed) in [
        ("#plain", "Nordstrasse"),
        ("#q", "flowproof"),
        ("#twice", "Nordstrasse"),
    ] {
        let field = UiaSelector::css(field);
        driver.type_text(&field, typed).expect("types");
        assert_eq!(
            driver.typed_suggestion(&field, typed).expect("reads"),
            None,
            "{field} must not offer a pick"
        );
    }
}
