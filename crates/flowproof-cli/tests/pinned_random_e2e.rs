//! `browser.random` makes a page that mints values with `Math.random`
//! deterministic — the clock's sibling, and the same argument.
//!
//! The decisive test is the CONTRAST. A pinned run that happens to agree
//! with itself proves nothing if the page was going to agree anyway, so
//! this asserts both halves: unpinned, the page draws different values
//! across loads; pinned, it draws the same one every time.
//!
//! Gated on FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

/// Draws once per load and shows it. Nothing else on the page moves, so a
/// difference between two loads can only come from `Math.random`.
const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Draw</title></head><body>
  <div id="drawn">?</div>
  <script>
    document.getElementById('drawn').textContent =
      String(Math.floor(Math.random() * 1000000));
  </script>
</body></html>
"#;

fn fixture(dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).expect("temp dir");
    let page = dir.join("draw.html");
    std::fs::write(&page, PAGE).expect("page written");
    format!("file://{}", page.display())
}

/// Read the drawn value by asserting a wrong one and mining the failure —
/// the value is the app's, so this needs no capture machinery.
fn drawn(url: &str, seed: Option<u32>, dir: &std::path::Path) -> String {
    let pin = match seed {
        Some(s) => format!("browser:\n  random:\n    seed: {s}\n"),
        None => String::new(),
    };
    let spec = FlowSpec::parse(&format!(
        "name: Draw\napp: web\nurl: {url}\n{pin}steps:\n  \
         - assert: the \"css:#drawn\" shows __never__\n"
    ))
    .expect("spec parses");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &dir.join("t.trace.jsonl"))
        .expect_err("the deliberately wrong expectation must fail");
    let message = err.to_string();
    let marker = "shows '";
    let start = message
        .find(marker)
        .expect("the failure quotes what it saw")
        + marker.len();
    let rest = &message[start..];
    rest[..rest.find('\'').expect("closing quote")].to_string()
}

#[test]
fn a_pinned_seed_makes_the_page_draw_the_same_value_every_run() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping pinned-random E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-pinned-random-e2e");
    let url = fixture(&dir);

    // Pinned: identical across independent loads.
    let a = drawn(&url, Some(1234), &dir);
    let b = drawn(&url, Some(1234), &dir);
    let c = drawn(&url, Some(1234), &dir);
    assert_eq!(a, b, "a pinned seed must draw the same value");
    assert_eq!(b, c, "a pinned seed must draw the same value");

    // A DIFFERENT seed must draw something else, or the shim is not
    // seeding at all — it could be returning a constant and still pass
    // the equality above.
    let other = drawn(&url, Some(99), &dir);
    assert_ne!(
        a, other,
        "a different seed must draw a different value; equal means the \
         shim is returning a constant rather than seeding a sequence"
    );

    // Unpinned: the page is genuinely random, so the pin above was doing
    // the work. Three loads to keep an accidental repeat from passing.
    let x = drawn(&url, None, &dir);
    let y = drawn(&url, None, &dir);
    let z = drawn(&url, None, &dir);
    assert!(
        x != y || y != z,
        "unpinned draws must vary, or this fixture proves nothing about \
         pinning: {x}, {y}, {z}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
