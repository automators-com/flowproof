//! The REAL SAP COM engine, exercised without SAP: a Python COM server
//! (tests/support/sap_simulator.py) publishes an object in the Running
//! Object Table under the item moniker `SAPGUI`, shaped like SAP's
//! scripting model. `SapAppDriver::new()` then attaches exactly as it
//! would to real SAP GUI — moniker binding through the ROT, IDispatch
//! late binding, VARIANT marshaling, collection walks, FindById error
//! paths, absolute→relative id stripping all execute for real.
//!
//! The moniker matters (issue #85). The simulator used to register a
//! `SAPGUI` ProgID as well, and the engine attached through it, so this
//! test passed for a year against a mechanism real SAP does not use: a
//! genuine 7.60 install has no such key in HKCR, and every real attach
//! failed. The simulator now publishes itself the way SAP does and
//! nothing else, so passing here means the real path works.
//!
//! Windows-only, opt-in via FLOWPROOF_E2E=1 (runs in windows CI, where
//! pywin32 is installed by the workflow step). The remaining untested
//! surface after this is SAP's own behavior, covered by the maintainer-run
//! `sap_e2e` against a real system.

#![cfg(windows)]

use std::io::BufRead;
use std::process::{Child, Command, Stdio};

use flowproof_agent::FlowSpec;

const SPEC: &str = "\
name: Create order
app: sap
steps:
  - Go to /nVA01
  - Type ZOR into the \"Order Type\" field
  - Type 4711 into the \"id:wnd[0]/usr/txtVBAK-KUNNR\" field
  - Press the \"Continue\" button
  - assert: page shows Order 4711 saved
";

/// Start the simulator and wait for its READY line.
fn start_simulator() -> Child {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("support")
        .join("sap_simulator.py");
    let mut child = Command::new("python")
        .arg(&script)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("python launches (pywin32 required)");
    let stdout = child.stdout.take().expect("stdout piped");
    let mut lines = std::io::BufReader::new(stdout).lines();
    match lines.next() {
        Some(Ok(line)) if line.trim() == "READY" => child,
        other => {
            let _ = child.kill();
            panic!("simulator did not become ready: {other:?}");
        }
    }
}

#[test]
fn real_com_engine_records_and_replays_against_the_simulator() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping SAP simulator E2E: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-sap-sim-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("order.trace.jsonl");

    let mut simulator = start_simulator();
    let result = std::panic::catch_unwind(|| {
        let spec = FlowSpec::parse(SPEC).expect("spec parses");

        // Record through the PRODUCTION COM engine.
        let mut driver =
            flowproof_adapters::sap_com::SapAppDriver::new().expect("COM engine initializes");
        flowproof_agent::record(&spec, &mut driver, &trace_path)
            .expect("rules author the flow via real COM");
        drop(driver);

        let trace = std::fs::read_to_string(&trace_path).expect("trace written");
        let header = trace.lines().next().expect("header");
        assert!(
            header.contains("\"adapter\":\"sap-com\""),
            "header: {header}"
        );
        assert!(
            trace.contains(r#""id":"wnd[0]/usr/txtVBAK-KUNNR""#),
            "scripting ids recorded under the documented payload key"
        );

        // Replay through a fresh COM attachment. The simulator keeps its
        // state (the status bar text), which the surface assert re-reads.
        let mut driver =
            flowproof_adapters::sap_com::SapAppDriver::new().expect("COM engine initializes");
        let (report, _run_dir) =
            flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
        for step in &report.steps {
            eprintln!("{:?} {} {}", step.status, step.id, step.intent);
        }
        assert!(report.passed, "flow must replay via real COM: {report:#?}");
        assert!(
            !report.degraded,
            "primary selectors must match: {report:#?}"
        );
    });
    let _ = simulator.kill();
    std::fs::remove_dir_all(&dir).ok();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
