//! `tosca::export` against committed traces: the migrator JSON it produces
//! for a real SAP and a real web recording.

use flowproof_trace::{tosca, Header, Step, TraceLine};
use serde_json::{json, Value};

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
fn web_trace_opens_the_url_and_maps_dom_ids() {
    let (header, steps) = load("examples/tricentis-insurance-natural.trace.jsonl");
    let out = tosca::export(&header, &steps).expect("export");
    let steps = out.testcase["steps"].as_array().expect("array");
    assert_eq!(steps[0]["actions"][0]["keyword"], "NavigateBrowser");
    assert_eq!(
        steps[0]["actions"][0]["value"],
        "https://sampleapp.tricentis.com/101"
    );

    let click = &steps[1]["actions"][0];
    assert_eq!(click["value"], "Click");
    assert_eq!(click["element"]["engine"], "Html");
    assert_eq!(click["element"]["application"], "sampleapp.tricentis.com");
    assert_eq!(click["element"]["steering_strategy"], "Html_NWBC");
    assert_eq!(
        click["element"]["properties"],
        json!([{"name": "html id", "value": "get_truck"}])
    );

    // One authored step that filled several fields stays one Tosca step.
    let vehicle = steps
        .iter()
        .find(|s| s["name"] == "Fill out all the vehicle data and click next")
        .expect("vehicle step");
    assert!(vehicle["actions"].as_array().expect("array").len() > 3);

    let last = &steps[steps.len() - 1]["order"];
    assert_eq!(last, &Value::String(steps.len().to_string()));
}

#[test]
fn non_ui_traces_are_refused() {
    let (mut header, steps) = load("examples/sap/create-order.trace.jsonl");
    header.app.adapter = flowproof_trace::format::Adapter::Api;
    let err = tosca::export(&header, &steps).expect_err("api adapter refused");
    assert!(err.contains("sap and web"), "{err}");
}
