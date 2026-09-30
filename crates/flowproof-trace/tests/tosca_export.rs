//! `tosca::export` against a committed SAP trace: the migrator JSON it
//! produces for a real recording.

use flowproof_trace::{tosca, Header, Step, TraceLine};
use serde_json::json;

fn load(rel: &str) -> (Header, Vec<Step>) {
    let path = format!("{}/../../{rel}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut header = None;
    let mut steps = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match TraceLine::parse(line).expect("trace line") {
            TraceLine::Header(h) => header = Some(h),
            TraceLine::Step(s) => steps.push(s),
        }
    }
    (header.expect("header"), steps)
}

#[test]
fn sap_trace_maps_to_transaction_and_relative_id() {
    let (header, steps) = load("examples/sap/create-order.trace.jsonl");
    let out = tosca::export(&header, &steps).expect("export");
    let tc = &out.testcase;
    assert_eq!(tc["name"], "Create standard order");
    assert_eq!(tc["testcasetype"], "TestCase");
    assert!(
        !tc["hash"].as_str().expect("hash").is_empty(),
        "empty hash makes re-runs skip"
    );

    let steps = tc["steps"].as_array().expect("array");
    assert_eq!(
        steps[0]["actions"][0],
        json!({"name": "Start transaction", "keyword": "StartTransaction", "value": "VA01"})
    );
    let input = &steps[1]["actions"][0];
    assert_eq!(input["actionmode"], "Input");
    assert_eq!(input["value"], "OR");
    let el = &input["element"];
    assert_eq!(el["engine"], "SAP");
    assert_eq!(el["application"], "VA01");
    assert_eq!(el["steering_strategy"], "SAPGUI_CBTA");
    assert_eq!(el["business_type"], "TextBox");
    assert_eq!(
        el["properties"],
        json!([
            {"name": "RelativeId", "value": "wnd[0]/usr/ctxtVBAK-AUART"},
            {"name": "Name", "value": "VBAK-AUART"}
        ])
    );
    assert_eq!(steps[2]["actions"][0]["keyword"], "SendKey");
    assert_eq!(steps[2]["actions"][0]["value"], "{ENTER}");
    assert_eq!(steps.len(), 3, "the page-wide check is not exported");

    // ...and says so, rather than dropping it silently.
    assert_eq!(out.warnings.len(), 1, "{:?}", out.warnings);
    assert!(out.warnings[0].starts_with("s0004"), "{:?}", out.warnings);
}

#[test]
fn non_ui_traces_are_refused() {
    let (mut header, steps) = load("examples/sap/create-order.trace.jsonl");
    header.app.adapter = flowproof_trace::format::Adapter::Api;
    let err = tosca::export(&header, &steps).expect_err("api adapter refused");
    assert!(err.contains("sap traces"), "{err}");
}
