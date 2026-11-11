#![cfg(feature = "web")]

//! Consent dialogs (#815), by the markup each verified manager ships:
//! consentmanager inside an open shadow root on `#cmpwrapper`, arriving a
//! moment after the page and covering it (as on stadtwerke-flensburg.de),
//! OneTrust's banner and Cookiebot's dialog. Every button writes what it
//! was into `#result`, so the test can see what was pressed - and that
//! "accept" never is.

use flowproof_driver::{AppDriver, UiaSelector, WebBrowserConfig};

fn fixture() -> String {
    let html = r#"<button id='go'>Go</button><p id='result'></p>
<div id='onetrust-banner-sdk'><button id='onetrust-accept-btn-handler'>Accept All</button>
<button id='onetrust-reject-all-handler'>Reject All</button></div>
<div id='CybotCookiebotDialog'><button id='CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll'>Allow all</button>
<button id='CybotCookiebotDialogBodyButtonDecline'>Deny</button></div>
<script>
const mark = (what) => { document.getElementById('result').textContent += what + ' '; };
const wire = (root, id, what, box) => root.getElementById(id).addEventListener('click', () => {
  mark(what); if (box) { root.getElementById(box).style.display = 'none'; }
});
wire(document, 'onetrust-accept-btn-handler', 'ACCEPTED');
wire(document, 'onetrust-reject-all-handler', 'onetrust', 'onetrust-banner-sdk');
wire(document, 'CybotCookiebotDialogBodyLevelButtonLevelOptinAllowAll', 'ACCEPTED');
wire(document, 'CybotCookiebotDialogBodyButtonDecline', 'cookiebot', 'CybotCookiebotDialog');
setTimeout(() => {
  const host = document.createElement('div');
  host.id = 'cmpwrapper';
  document.body.appendChild(host);
  const root = host.attachShadow({ mode: 'open' });
  root.innerHTML = "<div id='cmpbox' style='position:fixed;inset:0;background:white'>"
    + "<a id='yes' class='cmpboxbtn cmpboxbtnyes'>Accept all</a> "
    + "<a id='no' class='cmpboxbtn cmpboxbtnno'>Reject all</a></div>";
  wire(root, 'yes', 'ACCEPTED');
  wire(root, 'no', 'consentmanager', 'cmpbox');
}, 800);
</script>"#;
    format!("data:text/html,{}", html.replace('\n', " "))
}

/// Stadtwerke Flensburg's race, reduced: a menu that any click outside it
/// closes, and a consentmanager dialog - its `__cmp` API already saying no
/// decision exists - that arrives after the flow's first click could. The
/// rejection is such an outside click.
fn race_fixture() -> String {
    let html = r#"<button id='menu-button'>Produkte</button><nav id='menu' hidden><a id='item'>Haushaltsstrom</a></nav><p id='result'></p>
<script>
let consented = false;
window.__cmp = (cmd) => cmd === 'getCMPData' ? { gdprApplies: true, consentExists: consented } : undefined;
const menu = document.getElementById('menu');
document.getElementById('menu-button').addEventListener('click', (e) => { e.stopPropagation(); menu.hidden = false; });
document.addEventListener('click', (e) => { if (!menu.contains(e.target)) { menu.hidden = true; } });
setTimeout(() => {
  const host = document.createElement('div');
  host.id = 'cmpwrapper';
  document.body.appendChild(host);
  const root = host.attachShadow({ mode: 'open' });
  root.innerHTML = "<div id='cmpbox' style='position:fixed;bottom:0;left:0;right:0;background:white'><a class='cmpboxbtn cmpboxbtnno'>Reject all</a></div>";
  root.querySelector('.cmpboxbtnno').addEventListener('click', () => {
    consented = true;
    root.getElementById('cmpbox').style.display = 'none';
    document.getElementById('result').textContent += 'consentmanager ';
  });
}, 1500);
</script>"#;
    format!("data:text/html,{}", html.replace('\n', " "))
}

fn open(reject_consent: bool) -> flowproof_adapters::WebAppDriver {
    open_page(&fixture(), reject_consent)
}

fn open_page(url: &str, reject_consent: bool) -> flowproof_adapters::WebAppDriver {
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .stage_browser(WebBrowserConfig {
            reject_consent,
            ..WebBrowserConfig::default()
        })
        .expect("staged");
    driver
        .launch(url, "", std::time::Duration::from_secs(10))
        .expect("fixture opens");
    driver
}

fn result_after(driver: &mut flowproof_adapters::WebAppDriver, wanted: &[&str]) -> String {
    let result = UiaSelector::css("#result");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(6);
    loop {
        let text = driver.read_text(&result).expect("reads the result");
        if wanted.iter().all(|w| text.contains(w)) || std::time::Instant::now() >= deadline {
            return text;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

#[test]
fn known_consent_dialogs_are_rejected_wherever_and_whenever_they_appear() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = open(true);
    let pressed = result_after(&mut driver, &["consentmanager", "onetrust", "cookiebot"]);
    for manager in ["consentmanager", "onetrust", "cookiebot"] {
        assert!(
            pressed.contains(manager),
            "{manager} not rejected: {pressed:?}"
        );
    }
    assert!(
        !pressed.contains("ACCEPTED"),
        "accept was pressed: {pressed:?}"
    );
    // The late dialog covered the page; with it gone the page is clickable.
    assert_eq!(
        driver
            .element_receives_events(&UiaSelector::css("#go"))
            .expect("hit-tests"),
        Some(true)
    );
}

#[test]
fn a_flow_that_keeps_consent_dialogs_gets_them_untouched() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = open(false);
    std::thread::sleep(std::time::Duration::from_secs(2));
    assert_eq!(
        driver
            .read_text(&UiaSelector::css("#result"))
            .expect("reads the result"),
        ""
    );
    assert!(driver
        .element_exists(&UiaSelector::css("#onetrust-banner-sdk"))
        .expect("looks"));
}

/// Replay clicked "Produkte" a second before the dialog arrived, and the
/// rejection closed the menu the click had opened. The gate before a step
/// now lets a pending dialog in and rejects it first.
#[test]
fn a_dialog_still_on_its_way_is_rejected_before_the_step_not_during_it() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        return;
    }
    let _guard = flowproof_adapters::SharedBrowserGuard::new();
    let mut driver = open_page(&race_fixture(), true);
    let button = UiaSelector::css("#menu-button");
    let started = std::time::Instant::now();
    assert_eq!(driver.actionability_gate(&button).expect("gate"), None);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(500),
        "the gate did not wait for the dialog due at 1.5 s"
    );
    driver.invoke(&button).expect("opens the menu");
    std::thread::sleep(std::time::Duration::from_millis(2500));
    assert!(
        driver
            .read_text(&UiaSelector::css("#result"))
            .expect("reads")
            .contains("consentmanager"),
        "the dialog was rejected"
    );
    assert_eq!(
        driver
            .element_visible(&UiaSelector::css("#item"))
            .expect("looks"),
        Some(true),
        "the rejection closed the menu the step had opened"
    );
}
