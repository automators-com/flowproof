//! The full SAP pipeline without SAP: spec → rules → record → trace →
//! deterministic replay, against the in-memory fake scripting engine. This
//! is what CI proves on every platform; the real SAP GUI E2E (`sap_e2e`)
//! is opt-in on a machine that has one.

use flowproof_adapters::sap_com::{fake::FakeEngine, SapAppDriver, SapElement};
use flowproof_agent::FlowSpec;
use flowproof_driver::AppDriver;

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

/// A flow that carries its own credentials, end to end: no environment is
/// set anywhere in this test, the driver is told who to log in as, the
/// header names that identity — and the password appears NOWHERE in the
/// recorded artifact, which is the invariant that lets a trace be committed.
const SPEC_WITH_LOGIN: &str = "\
name: Create order as obeva
app: sap
connection: FLOWPROOF-TEST
login:
  user: obeva
  password: literal-password-in-the-spec
  client: '100'
steps:
  - Go to /nVA01
  - Press the \"Continue\" button
  - assert: page shows Order 4711 saved
";

#[test]
fn a_flow_logs_in_as_its_own_user_and_keeps_the_password_out_of_the_trace() {
    let dir = std::env::temp_dir().join("flowproof-sap-login");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let trace_path = dir.join("login.trace.jsonl");

    let spec = FlowSpec::parse(SPEC_WITH_LOGIN).expect("spec parses");
    let login = spec.login.as_ref().expect("login block");

    // Record. The CLI stages credentials before launch; do the same here,
    // which is what makes this a test of the wiring and not of the fake.
    let mut driver = SapAppDriver::with_engine(engine());
    driver
        .stage_credentials(login.resolved().expect("resolves with no environment set"))
        .expect("sap accepts credentials");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("records");

    let trace = std::fs::read_to_string(&trace_path).expect("trace written");
    assert!(
        trace.contains("\"login_user\":\"obeva\""),
        "the identity a recording ran as is part of what it means: {}",
        trace.lines().next().unwrap_or_default()
    );
    assert!(
        !trace.contains("literal-password-in-the-spec"),
        "the password must never reach the trace"
    );
    assert!(
        !trace.contains("password"),
        "not even as an empty or redacted field — there is no such field"
    );

    // Replay: same staging, from the spec, on a fresh screen.
    let mut driver = SapAppDriver::with_engine(engine());
    driver
        .stage_credentials(login.resolved().expect("resolves"))
        .expect("sap accepts credentials");
    let (report, _run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).expect("replay runs");
    assert!(report.passed, "flow with `login:` must replay: {report:#?}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Two flows, two users, one process — the case that environment variables
/// could not express, since they are process-global and set once per run.
#[test]
fn two_flows_in_one_process_run_as_two_different_users() {
    let clerk = FlowSpec::parse(SPEC_WITH_LOGIN).expect("clerk spec");
    let approver = FlowSpec::parse(
        &SPEC_WITH_LOGIN
            .replace("user: obeva", "user: approver")
            .replace("Create order as obeva", "Release order as approver"),
    )
    .expect("approver spec");

    let mut users = Vec::new();
    for spec in [&clerk, &approver] {
        let mut driver = SapAppDriver::with_engine(engine());
        driver
            .stage_credentials(
                spec.login
                    .as_ref()
                    .expect("login")
                    .resolved()
                    .expect("resolves"),
            )
            .expect("sap accepts credentials");
        driver
            .launch("FLOWPROOF-TEST", "SAP", std::time::Duration::from_secs(1))
            .expect("connects");
        users.push(driver.engine().logged_in_as.clone());
    }
    assert_eq!(
        users,
        vec![Some("obeva".to_string()), Some("approver".to_string())],
        "each flow logged in as its own user"
    );
}

#[test]
fn multi_surface_record_and_replay_use_distinct_logins_and_resume_them() {
    use flowproof_adapters::sap_com::SapEngine;
    use flowproof_driver::surface::{SurfaceFactory, SurfaceRegistry};
    use flowproof_driver::{DriverError, LoginCredentials};
    use std::{cell::RefCell, rc::Rc, time::Duration};
    struct ObservedEngine {
        inner: FakeEngine,
        log: Rc<RefCell<Vec<(String, String)>>>,
    }
    impl SapEngine for ObservedEngine {
        fn connect(
            &mut self,
            connection: &str,
            credentials: Option<&LoginCredentials>,
            timeout: Duration,
        ) -> Result<(), DriverError> {
            assert!(
                credentials.is_some(),
                "each surface must stage its own credentials"
            );
            self.inner.connect(connection, credentials, timeout)?;
            self.log.borrow_mut().push((
                connection.into(),
                self.inner
                    .logged_in_as
                    .clone()
                    .expect("valid login test fixture"),
            ));
            Ok(())
        }
        fn find_by_id(&mut self, id: &str) -> Result<Option<SapElement>, DriverError> {
            self.inner.find_by_id(id)
        }
        fn walk(&mut self) -> Result<Vec<SapElement>, DriverError> {
            self.inner.walk()
        }
        fn set_text(&mut self, id: &str, text: &str) -> Result<(), DriverError> {
            self.inner.set_text(id, text)
        }
        fn press(&mut self, id: &str) -> Result<(), DriverError> {
            self.inner.press(id)
        }
        fn select(&mut self, id: &str) -> Result<(), DriverError> {
            self.inner.select(id)
        }
        fn set_selected(&mut self, id: &str, selected: bool) -> Result<(), DriverError> {
            self.inner.set_selected(id, selected)
        }
        fn set_focus(&mut self, id: &str) -> Result<(), DriverError> {
            self.inner.set_focus(id)
        }
        fn send_vkey(&mut self, vkey: u16) -> Result<(), DriverError> {
            self.inner.send_vkey(vkey)
        }
        fn screen_size(&mut self) -> Result<(u32, u32), DriverError> {
            self.inner.screen_size()
        }
    }
    let spec = FlowSpec::parse("name: Two users\napps:\n  clerk: {app: sap, connection: QA, login: {user: clerk, password: clerk-secret}}\n  approver: {app: sap, connection: Review, login: {user: approver, password: approver-secret}}\nsteps:\n  - in: clerk\n    steps: [Go to /nVA01]\n  - in: approver\n    steps: [Go to /nVA01]\n  - in: clerk\n    steps: [Go to /nVA01]\n").expect("valid login test fixture");
    let log = Rc::new(RefCell::new(Vec::new()));
    let registry = |log: Rc<RefCell<Vec<(String, String)>>>| {
        let factory: SurfaceFactory = Box::new(move |_| {
            Ok(Box::new(SapAppDriver::with_engine(ObservedEngine {
                inner: engine(),
                log: log.clone(),
            })))
        });
        SurfaceRegistry::new(
            flowproof_agent::surface_targets(&spec).expect("valid login test fixture"),
            factory,
            Duration::from_secs(1),
        )
    };
    let dir = std::env::temp_dir().join(format!("flowproof-multi-login-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("valid login test fixture");
    let trace = dir.join("users.trace.jsonl");
    let mut driver: Box<dyn AppDriver> = Box::new(registry(log.clone()));
    flowproof_agent::record(&spec, &mut driver, &trace).expect("valid login test fixture");
    assert!(log
        .borrow()
        .iter()
        .all(|(connection, user)| (connection == "QA" && user == "clerk")
            || (connection == "Review" && user == "approver")));
    assert!(log.borrow().iter().any(|(_, user)| user == "approver"));
    let saved = std::fs::read_to_string(&trace).expect("valid login test fixture");
    assert!(!saved.contains("clerk-secret") && !saved.contains("approver-secret"));
    log.borrow_mut().clear();
    let mut replay: Box<dyn AppDriver> = Box::new(registry(log.clone()));
    for (name, surface) in &spec.apps {
        replay
            .stage_surface_credentials(
                name,
                surface
                    .login
                    .as_ref()
                    .expect("valid login test fixture")
                    .resolved()
                    .expect("valid login test fixture"),
            )
            .expect("valid login test fixture");
    }
    let (report, _) =
        flowproof_replay::run_trace(&trace, &mut replay).expect("valid login test fixture");
    assert!(report.passed);
    assert!(log.borrow().iter().any(|(_, user)| user == "approver"));
    assert!(log
        .borrow()
        .iter()
        .all(|(connection, user)| (connection == "QA" && user == "clerk")
            || (connection == "Review" && user == "approver")));
    std::fs::remove_dir_all(dir).expect("valid login test fixture");
}
