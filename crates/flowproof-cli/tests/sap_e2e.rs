//! End-to-end against a REAL SAP GUI session. No CI runner has one, so
//! this is maintainer-run: Windows, SAP GUI installed, logged in, scripting
//! enabled (`sapgui/user_scripting = TRUE`), and FLOWPROOF_E2E_SAP=1.
//!
//! The flow only navigates and reads — it types a transaction code and
//! asserts on the status/title, touching no business data.

#![cfg(windows)]

use flowproof_adapters::sap_com::SapAppDriver;
use flowproof_agent::FlowSpec;

const SPEC: &str = "\
name: Navigate to session status
app: sap
steps:
  - Go to /nSESSION_MANAGER
  - assert: page shows Session
";

#[test]
fn navigates_a_real_sap_session() {
    if std::env::var("FLOWPROOF_E2E_SAP").as_deref() != Ok("1") {
        eprintln!(
            "skipping SAP E2E: set FLOWPROOF_E2E_SAP=1 on a machine with a logged-in SAP GUI"
        );
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-sap-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("session.trace.jsonl");

    let spec = FlowSpec::parse(SPEC).expect("spec parses");

    let mut driver = SapAppDriver::new().expect("COM engine");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("records against live SAP");
    drop(driver);

    let mut driver = SapAppDriver::new().expect("COM engine");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "sap flow must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
