//! Scoped-container targets, end to end in a real browser. Records
//! assertions and an ACTION scoped to one list item, replays them, then
//! edits the page in the two ways that matter: the anchor is REWORDED (the
//! record-time container id must carry the target) and the anchor is moved
//! OUT of every list (the timeout must say so, not "not found"). Ambiguity
//! is a hard error with its own test. Gated on FLOWPROOF_E2E=1, like the
//! other web E2Es.

use flowproof_agent::FlowSpec;

const LIST_HTML: &str = include_str!("../../../examples/web/list.html");
const CARDS_HTML: &str = include_str!("../../../examples/web/cards.html");

/// The happy path, an action, and innermost-wins - all against the `item`
/// rung, which never names a selector.
const LIST_SPEC: &str = r#"
name: Scoped container targets
app: web
url: __URL__
steps:
  - assert: the "css:.amount" in the item containing "Invoice 4711" shows 50.00
  - assert: the "css:.amount" inside the item containing "Invoice 4712" shows 75.00
  - assert: the "Select" checkbox in the item containing "Invoice 4711" is not checked
  - Check the "Select" checkbox in the item containing "Invoice 4711"
  - assert: the "Select" checkbox in the item containing "Invoice 4711" is checked
  - assert: the "Select" checkbox in the item containing "Invoice 4712" is not checked
  - Click the "Pay" in the item containing "Invoice 4711"
  - assert: the "css:.state" in the item containing "Invoice 4711" shows Paid
  - assert: the "css:.state" in the item containing "Invoice 4712" shows Unpaid
  # Innermost wins: "Invoice 4791" sits in a nested item inside "Group A",
  # so the INNER item is the container - the outer group's first Pay button
  # belongs to a different transaction and must not be the one clicked.
  - Click the "Pay" in the item containing "Invoice 4791"
  - assert: the "css:.state" in the item containing "Invoice 4791" shows Paid
  - assert: the "css:.state" in the item containing "Invoice 4790" shows Unpaid
"#;

/// The same target against an explicit `css:` container - divs are not list
/// items, so rung 2 cannot see them and the spec names the container.
const CARDS_SPEC: &str = r#"
name: Scoped container targets, explicit selector
app: web
url: __URL__
steps:
  - assert: the "css:.amount" in the "css:.card" containing "Order 8801" shows 120.00
  - Click the "Ship" in the "css:.card" containing "Order 8801"
  - assert: the "css:.state" in the "css:.card" containing "Order 8801" shows Shipped
  - assert: the "css:.state" in the "css:.card" containing "Order 8802" shows Open
"#;

/// An anchor every item carries cannot pick one: a hard error, by design.
const AMBIGUOUS_SPEC: &str = r#"
name: Ambiguous container anchor
app: web
url: __URL__
steps:
  - assert: the "css:.amount" in the item containing "Invoice" shows 50.00
"#;

fn skip() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping scoped container E2E: set FLOWPROOF_E2E=1 to run it");
        return true;
    }
    false
}

fn spec_for(yaml: &str, page: &std::path::Path) -> FlowSpec {
    FlowSpec::parse(&yaml.replace("__URL__", &format!("file://{}", page.display())))
        .expect("spec parses")
}

#[test]
fn scoped_targets_resolve_act_and_survive_an_anchor_rewording() {
    if skip() {
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-scoped-container-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("list.html");
    std::fs::write(&page, LIST_HTML).expect("page written");
    let trace = dir.join("list.trace.jsonl");
    let spec = spec_for(LIST_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 12);
    drop(driver);

    // The record-time hint is in the trace: the container's own id, the
    // `row_id` analog.
    let recorded = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        recorded.contains("\"container_id\":\"transaction-183\""),
        "the container id must be harvested at record time: {recorded}"
    );
    assert!(
        recorded.contains("\"inner_text\"") || recorded.contains("\"inner_css\""),
        "inner keys must be prefixed: {recorded}"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "scoped targets must replay: {report:#?}");
    drop(driver);

    // Reword the anchor. Text identity no longer matches, but the recorded
    // container_id resolves the item anyway - the cell's hint rule, applied
    // to a container.
    std::fs::write(&page, LIST_HTML.replace("Invoice 4711", "Inv. 4711-A")).expect("reworded page");
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(
        report.passed,
        "the container_id hint must survive an anchor rewording: {report:#?}"
    );
    drop(driver);

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_explicit_css_container_resolves_and_acts() {
    if skip() {
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-scoped-cards-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("cards.html");
    std::fs::write(&page, CARDS_HTML).expect("page written");
    let trace = dir.join("cards.trace.jsonl");
    let spec = spec_for(CARDS_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let summary = flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    assert_eq!(summary.steps, 4);
    drop(driver);

    let recorded = std::fs::read_to_string(&trace).expect("trace readable");
    assert!(
        recorded.contains("\"container\":\"css:.card\""),
        "the container travels as written: {recorded}"
    );
    assert!(
        recorded.contains("\"container_id\":\"order-8801\""),
        "the hint is harvested from data-test too: {recorded}"
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(report.passed, "css containers must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// An anchor that matches several items is a HARD error - the spec named an
/// identity that does not identify anything, and guessing would be the
/// silent wrong-element bug this target exists to remove.
#[test]
fn an_ambiguous_container_anchor_is_a_hard_error() {
    if skip() {
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-scoped-ambiguous-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("list.html");
    std::fs::write(&page, LIST_HTML).expect("page written");
    let trace = dir.join("ambiguous.trace.jsonl");
    let spec = spec_for(AMBIGUOUS_SPEC, &page);

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let err = flowproof_agent::record(&spec, &mut driver, &trace)
        .expect_err("an ambiguous anchor must not record");
    let message = err.to_string();
    assert!(
        message.contains("container anchor") && message.contains("more specific anchor"),
        "the error must name the ambiguity and the fix: {message}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The anchor is on the page but in no list item at all. That is an
/// ordinary miss while it auto-waits, and a NAMED failure at the deadline:
/// "not found" would send the author hunting for a typo that is not there.
#[test]
fn an_anchor_outside_every_container_names_the_fix_at_the_timeout() {
    if skip() {
        return;
    }
    let dir = std::env::temp_dir().join("flowproof-scoped-orphan-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let page = dir.join("list.html");
    std::fs::write(&page, LIST_HTML).expect("page written");
    let trace = dir.join("orphan.trace.jsonl");
    let spec = spec_for(
        r#"
name: Scoped assert that will lose its container
app: web
url: __URL__
steps:
  - assert: the "css:.amount" in the item containing "Invoice 4712" shows 75.00 within 2s
"#,
        &page,
    );

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace).expect("recording succeeds");
    drop(driver);

    // Move the anchor OUT of the list: same text on the page, no item
    // holding it. The item's id changes too, so the record-time hint
    // cannot rescue the lookup - this is the no-container case, not the
    // reworded-anchor one.
    let orphaned = LIST_HTML
        .replace(
            r#"<li id="transaction-184">
    <span class="ref">Invoice 4712</span>"#,
            r#"<li id="transaction-184-renamed">
    <span class="ref">no anchor here</span>"#,
        )
        .replace(
            r#"<p id="loose">"#,
            r#"<p id="orphan">Invoice 4712</p>
<p id="loose">"#,
        );
    assert!(
        orphaned.contains(r#"<p id="orphan">Invoice 4712</p>"#)
            && orphaned.contains("transaction-184-renamed"),
        "the fixture edit must apply"
    );
    std::fs::write(&page, &orphaned).expect("orphaned page");

    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    let (report, _) = flowproof_replay::run_trace(&trace, &mut driver).expect("replay runs");
    assert!(!report.passed, "the container is gone: {report:#?}");
    let reason = report
        .steps
        .iter()
        .find_map(|s| s.detail.clone())
        .unwrap_or_default();
    assert!(
        reason.contains("is visible but sits in no list item")
            && reason.contains("name the container"),
        "the timeout must name the fix: {reason}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
