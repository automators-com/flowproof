//! End-to-end: actually drives Windows Calculator through record and
//! replay. Windows-only, and opt-in via FLOWPROOF_E2E=1 because CI runners
//! (Windows Server) don't ship the Calculator app — run it on a Windows
//! desktop VM:
//!
//! ```text
//! set FLOWPROOF_E2E=1
//! cargo test -p flowproof-cli --test calc_e2e -- --nocapture
//! ```

#![cfg(windows)]

use flowproof_agent::FlowSpec;
use flowproof_driver::UiaAppDriver;

const CALC_SPEC: &str = include_str!("../../../examples/calc.flow.yaml");

#[test]
fn records_and_replays_calculator() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping calculator E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-calc-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("calc.trace.jsonl");

    let spec = FlowSpec::parse(CALC_SPEC).expect("example spec parses");

    let mut driver = UiaAppDriver::new().expect("UIA client initializes");
    let summary =
        flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    assert_eq!(summary.steps, 5);

    let mut driver = UiaAppDriver::new().expect("UIA client initializes");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    for step in &report.steps {
        eprintln!("{:?} {} {}", step.status, step.id, step.intent);
    }
    assert!(report.passed, "calculator flow must pass: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}
