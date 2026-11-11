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

fn open(reject_consent: bool) -> flowproof_adapters::WebAppDriver {
    let mut driver = flowproof_adapters::WebAppDriver::new().expect("browser starts");
    driver
        .stage_browser(WebBrowserConfig {
            reject_consent,
            ..WebBrowserConfig::default()
        })
        .expect("staged");
    driver
        .launch(&fixture(), "", std::time::Duration::from_secs(10))
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
