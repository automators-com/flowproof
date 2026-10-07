//! End-to-end, against real headless Chromium (opt-in via FLOWPROOF_E2E=1,
//! like `web_e2e.rs`). Its own test binary because it drives the CLI, which
//! seeds the saved `flowproof config` into this process's environment; in a
//! shared binary that would change how later tests author.

const GREETER_HTML: &str = include_str!("../../../examples/web/greeter.html");

/// `heal` re-records, so it must resolve the flow's `${VAR}`s the way
/// `record` does: a flow whose start URL comes from `--var` heals healthy.
#[test]
fn heal_resolves_vars_like_record() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping web heal --var E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-web-heal-var-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("greeter.html");
    std::fs::write(&page, GREETER_HTML).expect("page written");
    let spec_path = dir.join("web.flow.yaml");
    let yaml = include_str!("../../../examples/web.flow.yaml")
        .replace("url: examples/web/greeter.html", "url: ${GREETER_URL}");
    std::fs::write(&spec_path, yaml).expect("spec written");
    let var = format!("GREETER_URL=file://{}", page.display());
    let spec = spec_path.to_str().expect("path");

    let record = [
        "record",
        spec,
        "--var",
        &var,
        "--author",
        "rules",
        "--no-repair",
        "--headless",
    ];
    assert_eq!(flowproof_cli::run_cli(record), 0, "record --var");
    let heal = [
        "heal",
        spec,
        "--var",
        &var,
        "--author",
        "rules",
        "--headless",
    ];
    assert_eq!(flowproof_cli::run_cli(heal), 0, "heal --var heals healthy");
    assert!(!dir.join("web.proposed.jsonl").exists());
}
