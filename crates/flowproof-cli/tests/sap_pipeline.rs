//! The full SAP pipeline without SAP: spec → rules → record → trace →
//! deterministic replay, against the in-memory fake scripting engine. This
//! is what CI proves on every platform; the real SAP GUI E2E (`sap_e2e`)
//! is opt-in on a machine that has one.

use flowproof_adapters::sap_com::{fake::FakeEngine, SapAppDriver, SapElement};
use flowproof_agent::FlowSpec;

const SPEC: &str = "\
name: Create order
app: sap
connection: FLOWPROOF-TEST
steps:
  - Go to /nVA01
  - Type ZOR into the \"Order Type\" field
  - Type 4711 into the \"id:wnd[0]/usr/txtVBAK-KUNNR\" field
  - Press the \"Continue\" button
  - assert: page shows Order 4711 saved
";

/// A fresh VA01-ish screen; pressing Continue posts the order.
fn engine() -> FakeEngine {
    let mut engine = FakeEngine::with_elements(vec![
        SapElement {
            id: "wnd[0]/tbar[0]/okcd".into(),
            kind: "GuiOkCodeField".into(),
            name: "okcd".into(),
            changeable: true,
            ..Default::default()
        },
        SapElement {
            id: "wnd[0]/usr/ctxtVBAK-AUART".into(),
            kind: "GuiCTextField".into(),
            name: "VBAK-AUART".into(),
            tooltip: "Order Type".into(),
            changeable: true,
            ..Default::default()
        },
        SapElement {
            id: "wnd[0]/usr/txtVBAK-KUNNR".into(),
            kind: "GuiTextField".into(),
            name: "VBAK-KUNNR".into(),
            tooltip: "Customer".into(),
            changeable: true,
            ..Default::default()
        },
        SapElement {
            id: "wnd[0]/tbar[1]/btn[8]".into(),
            kind: "GuiButton".into(),
            name: "btn[8]".into(),
            text: "Continue".into(),
            ..Default::default()
        },
        SapElement {
            id: "wnd[0]/sbar".into(),
            kind: "GuiStatusbar".into(),
            name: "sbar".into(),
            ..Default::default()
        },
    ]);
    engine.on_press.push((
        "wnd[0]/tbar[1]/btn[8]".into(),
        "wnd[0]/sbar".into(),
        "Order 4711 saved".into(),
    ));
    engine
}

#[test]
fn records_and_replays_a_sap_flow_via_the_fake_engine() {
    let dir = std::env::temp_dir().join("flowproof-sap-pipeline");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("order.trace.jsonl");

    let spec = FlowSpec::parse(SPEC).expect("spec parses");

    // Record against a fresh screen.
    let mut driver = SapAppDriver::with_engine(engine());
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("rules author the whole flow");

    // The trace speaks the sap-com provenance end to end.
    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    let header = trace.lines().next().expect("header");
    assert!(
        header.contains("\"adapter\":\"sap-com\""),
        "header: {header}"
    );
    assert!(
        header.contains("FLOWPROOF-TEST"),
        "connection travels in the header: {header}"
    );
    assert!(
        trace.contains(r#""id":"wnd[0]/usr/txtVBAK-KUNNR""#),
        "scripting id is the native rung with the documented payload key"
    );
    assert!(
        trace.contains(r#""provenance":"sap-com""#),
        "selectors carry sap-com provenance"
    );

    // Replay on a NEW screen (state reset, like a fresh SAP session).
    let mut driver = SapAppDriver::with_engine(engine());
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "sap flow must replay: {report:#?}");
    assert!(
        !report.degraded,
        "primary selectors must match: {report:#?}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
