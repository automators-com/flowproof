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
use std::sync::{Mutex, MutexGuard};

use flowproof_adapters::sap_com::SapAppDriver;
use flowproof_agent::FlowSpec;
use flowproof_driver::{AppDriver, UiaSelector};

const SPEC: &str = "\
name: Create order
app: sap
connection: SIM
steps:
  - Go to /nVA01
  - Type ZOR into the \"Order Type\" field
  - Type 4711 into the \"id:wnd[0]/usr/txtVBAK-KUNNR\" field
  - Press the \"Continue\" button
  - assert: page shows Order 4711 saved
";

/// The simulator publishes itself in the MACHINE-WIDE Running Object Table
/// under `SAPGUI`, so two live at once would make which one the engine
/// attaches to a coin flip. Cargo runs the tests in a binary in parallel, so
/// a test holds this for as long as it owns a simulator process.
static SIMULATOR: Mutex<()> = Mutex::new(());

fn own_the_simulator() -> MutexGuard<'static, ()> {
    SIMULATOR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// This tier is opt-in: it needs Windows COM and pywin32.
fn e2e_enabled() -> bool {
    if std::env::var("FLOWPROOF_E2E").as_deref() == Ok("1") {
        return true;
    }
    eprintln!("skipping SAP simulator E2E: set FLOWPROOF_E2E=1 to run it");
    false
}

/// Address an element by its scripting id — the native SAP selector rung.
fn by_id(id: &str) -> UiaSelector {
    UiaSelector {
        automation_id: Some(id.to_string()),
        ..Default::default()
    }
}

/// Set an environment variable for the duration of a test, then put back
/// whatever was there.
struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        std::env::set_var(key, value);
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            std::env::set_var(self.key, value);
        } else {
            std::env::remove_var(self.key);
        }
    }
}

/// The credentials the simulator's login screen accepts.
fn staged_login() -> [EnvGuard; 3] {
    [
        EnvGuard::set("SAP_USER", "SIMUSER"),
        EnvGuard::set("SAP_PASSWORD", "SIMPASS"),
        EnvGuard::set("SAP_CLIENT", "001"),
    ]
}

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
    if !e2e_enabled() {
        return;
    }
    let _serial = own_the_simulator();

    // The simulator puts an unrelated logged-in connection first and the
    // requested SIM connection second, sitting at the login screen. This one
    // run therefore proves connection selection and environment-backed login
    // through the production COM implementation.
    let _login = staged_login();

    let dir = std::env::temp_dir().join("flowproof-sap-sim-e2e");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("order.trace.jsonl");

    let mut simulator = start_simulator();
    let result = std::panic::catch_unwind(|| {
        let spec = FlowSpec::parse(SPEC).expect("spec parses");

        // Record through the PRODUCTION COM engine.
        let mut driver = SapAppDriver::new().expect("COM engine initializes");
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
            header.contains("\"url\":\"SIM\""),
            "the requested SAP connection must remain replayable: {header}"
        );
        assert!(
            trace.contains(r#""id":"wnd[0]/usr/txtVBAK-KUNNR""#),
            "scripting ids recorded under the documented payload key"
        );

        // Replay through a fresh COM attachment. The simulator keeps its
        // state (the status bar text), which the surface assert re-reads.
        let mut driver = SapAppDriver::new().expect("COM engine initializes");
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

/// The two screen shapes a flat single-window fixture can never fail on: a
/// classic `GuiTableControl` whose cells are NESTED children carrying SAP's
/// `[column,row]` ids, and a `wnd[1]` modal that opens over the main window
/// and hands it back when dismissed.
///
/// Driven through the production COM engine, so what is proved is the real
/// thing: a tree walk that recurses past depth two, `FindById` surviving an
/// id with brackets and a comma in it, and a session whose SET OF WINDOWS
/// changes underneath a running flow.
///
/// The modal is deliberately built so the two windows share no text — the
/// main window says "Create Standard Order", the popup says "Do you want to
/// save your data?". That is what lets a caller tell which window it is
/// reading, and it is the fixture #475's window scoping needs: once that
/// lands, asserting the background is NOT in `surface_text` while the popup
/// is open is one more line here.
#[test]
fn the_com_engine_drives_a_table_control_and_a_modal_window() {
    if !e2e_enabled() {
        return;
    }
    let _serial = own_the_simulator();
    let _login = staged_login();

    let mut simulator = start_simulator();
    let result = std::panic::catch_unwind(|| {
        let mut driver = SapAppDriver::new().expect("COM engine initializes");
        driver
            .launch("SIM", "SAP", std::time::Duration::from_secs(60))
            .expect("attaches to the simulated SIM session");

        // --- the classic table control ---------------------------------
        const CELL: &str = "wnd[0]/usr/tblSAPMV45ATCTRL_U_ERF_AUFTRAG/ctxtVBAP-MATNR[0,1]";
        driver
            .type_text(&by_id(CELL), "M-01")
            .expect("a nested table cell takes input");
        assert_eq!(
            driver.read_text(&by_id(CELL)).expect("cell reads back"),
            "M-01",
            "an id with brackets and a comma must survive the round trip"
        );

        let scene = driver.scene().expect("scene").expect("sap grounds a scene");
        assert!(
            scene.contains(&format!("id:{CELL}")),
            "cells inside a table are targets a model can be offered: {scene}"
        );
        assert!(
            !scene.contains("id:wnd[0]/usr/tblSAPMV45ATCTRL_U_ERF_AUFTRAG\""),
            "the table CONTAINER is not something to act on: {scene}"
        );
        let surface = driver.surface_text().expect("surface");
        assert!(
            surface.contains("Order Quantity"),
            "a label nested inside the table still reaches the surface: {surface}"
        );

        // --- the wnd[1] modal ------------------------------------------
        driver
            .invoke(&by_id("wnd[0]/tbar[0]/btn[3]"))
            .expect("Back opens the save prompt");
        let surface = driver.surface_text().expect("surface");
        assert!(
            surface.contains("Do you want to save your data?"),
            "a second window's text must be readable at all: {surface}"
        );
        let scene = driver.scene().expect("scene").expect("json");
        assert!(
            scene.contains("id:wnd[1]/usr/btnSPOP-OPTION2"),
            "the popup's own buttons are what a flow can press: {scene}"
        );

        // Dismissing it removes the whole window from the session tree, so
        // nothing behind a closed popup stays addressable.
        driver
            .invoke(&by_id("wnd[1]/usr/btnSPOP-OPTION2"))
            .expect("'No' dismisses the prompt");
        assert!(
            !driver
                .element_exists(&by_id("wnd[1]/usr/btnSPOP-OPTION2"))
                .expect("lookup succeeds"),
            "a dismissed popup's controls must stop resolving"
        );
        let surface = driver.surface_text().expect("surface");
        assert!(
            !surface.contains("Do you want to save your data?"),
            "the dismissed popup must leave the surface: {surface}"
        );
        assert!(
            surface.contains("Create Standard Order"),
            "the main window is the surface again once the popup closes: {surface}"
        );
    });
    let _ = simulator.kill();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
