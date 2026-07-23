//! Table-cell addressing by identity (#58), end to end in a real browser.
//! Records cell assertions against a grid, replays them, then INSERTS a row
//! and replays the SAME trace again: identity addressing (column header +
//! row anchor) must survive the reorder that broke the nth-ordinal
//! workaround. Gated on FLOWPROOF_E2E=1, like the other web E2Es.

use flowproof_agent::FlowSpec;

const GRID_HTML: &str = include_str!("../../../examples/web/grid.html");

const SPEC: &str = r#"
name: Cell addressing by identity
app: web
url: __URL__
steps:
  - assert: the "Status" column of the row containing "Grace Hopper" shows Suspended
  - assert: the "Balance" column of the row containing "Grace Hopper" is empty
  - assert: the "Status" column of the row containing "Ada Lovelace" shows Active
"#;

/// The same page with one extra row inserted ABOVE the anchored records, so
/// Grace moves from the 2nd body row to the 3rd - the exact edit that made
/// a positional trace assert against the wrong record.
fn with_inserted_row(html: &str) -> String {
    let inserted = "    <tr class=\"RaDatagrid-row\" id=\"row-100\">\n\
        \x20     <td class=\"column-name\">Aaron Adams</td>\n\
        \x20     <td class=\"column-email\">aaron@example.com</td>\n\
        \x20     <td class=\"column-status status-active\">Active</td>\n\
        \x20     <td class=\"column-balance\">$50.00</td>\n\
        \x20   </tr>\n    <tr class=\"RaDatagrid-row\" id=\"row-101\">";
    html.replacen(
        "    <tr class=\"RaDatagrid-row\" id=\"row-101\">",
        inserted,
        1,
    )
}

#[test]
fn cell_addressing_survives_a_row_insert() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping grid cell E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-grid-cell-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("grid.html");
    std::fs::write(&page, GRID_HTML).expect("page written");
    let trace = dir.join("grid.trace.jsonl");

    let spec = FlowSpec::parse(&SPEC.replace("__URL__", &format!("file://{}", page.display())))
        .expect("spec parses");

    // Record against the original grid.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 3);
    drop(driver);

    // Replay against the original: all cells resolve by identity.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "identity cells must replay: {report:#?}");
    drop(driver);

    // The decisive test: insert a row above Grace and replay the SAME trace.
    // A positional selector would now hit the wrong record; identity does
    // not - Grace is still found by her anchor, in whatever row she landed.
    std::fs::write(&page, with_inserted_row(GRID_HTML)).expect("reordered page");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "identity addressing must survive a row insert: {report:#?}"
    );

    // And the record-time hint fallback: rename the column HEADER. Text
    // identity ("Status") no longer matches, but the recorded column_field
    // hint (the `column-status` class) resolves it anyway.
    std::fs::write(
        &page,
        GRID_HTML.replacen(
            "<th class=\"column-status\">Status</th>",
            "<th class=\"column-status\">State</th>",
            1,
        ),
    )
    .expect("renamed header");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "the column_field hint must survive a header rename: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
