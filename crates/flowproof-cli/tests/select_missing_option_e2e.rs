//! `Select` with an option that does not exist must FAIL, not type.
//!
//! A JS exception inside the driver does not reach Rust as an `Err`, so
//! throwing on a missing option was indistinguishable from "this element is
//! not a `<select>`" — and fell through to typing the option's name into the
//! dropdown. Typing into a `<select>` keyboard-selects by prefix, so the
//! step landed on whatever option starts with the same letters and reported
//! success. A wrong option, selected quietly.
//!
//! The green half is here for the same reason: a fix that made every select
//! fail would pass the red test on its own. Gated on FLOWPROOF_E2E=1.

use flowproof_agent::FlowSpec;

const PAGE: &str = r#"<!doctype html>
<html><head><meta charset="utf-8"><title>Roles</title></head><body>
  <select id="role">
    <option>Admin</option>
    <option>Auditor</option>
    <option>Viewer</option>
  </select>
  <input id="free" placeholder="free text">
  <div id="out">none</div>
  <script>
    document.getElementById('role').addEventListener('change', function () {
      document.getElementById('out').textContent = 'SEL:' + this.value;
    });
  </script>
</body></html>
"#;

fn fixture(dir: &std::path::Path) -> String {
    std::fs::create_dir_all(dir).expect("temp dir");
    let page = dir.join("roles.html");
    std::fs::write(&page, PAGE).expect("page written");
    format!("file://{}", page.display())
}

/// The red path: an option that matches NOTHING on the ladder.
///
/// Note what is deliberately absent here. `Audit` is not one of these,
/// because prefix matching is documented behaviour — value, then exact
/// visible text, then prefix — so `Audit` legitimately selects `Auditor`.
/// The bug was never about prefixes; it was that a name matching nothing
/// at all fell through to typing, and typing into a `<select>` is itself a
/// prefix search, so the step landed on some other option and passed.
#[test]
fn selecting_an_option_that_does_not_exist_fails_instead_of_typing() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping select E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-select-missing-e2e");
    let url = fixture(&dir);
    let trace = dir.join("missing.trace.jsonl");

    for missing in ["Telepathy", "Zzz Nonexistent"] {
        let spec = FlowSpec::parse(&format!(
            "name: Missing\napp: web\nurl: {url}\nsteps:\n  \
             - Select {missing} from the \"id:role\" field\n"
        ))
        .expect("spec parses");
        let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
        let err = flowproof_agent::record(&spec, &mut driver, &trace)
            .expect_err("a missing option must fail the recording");
        let message = err.to_string();
        assert!(
            message.contains("no option matching"),
            "{missing}: the failure must name the missing option: {message}"
        );
        drop(driver);
    }

    std::fs::remove_dir_all(&dir).ok();
}

/// The other direction, so the fix cannot be "make selecting fail": a real
/// option still commits and fires `change`, a prefix that IS unambiguous
/// still resolves, and typing into an ordinary input still falls through to
/// typing — which is what the `not_select` answer exists to preserve.
#[test]
fn a_real_option_still_selects_and_ordinary_typing_still_types() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping select E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-select-ok-e2e");
    let url = fixture(&dir);
    let trace = dir.join("ok.trace.jsonl");

    let spec = FlowSpec::parse(&format!(
        "name: Selects\napp: web\nurl: {url}\nsteps:\n  \
         - Select Auditor from the \"id:role\" field\n  \
         - assert: page shows SEL:Auditor\n  \
         - Type hello into the \"id:free\" field\n  \
         - assert: the \"id:free\" field contains hello\n"
    ))
    .expect("spec parses");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 4);
    drop(driver);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "select must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
