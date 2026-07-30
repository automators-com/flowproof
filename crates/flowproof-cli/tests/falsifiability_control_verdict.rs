//! Falsifiability proof for `control:` verdict mapping (issue #253).
//!
//! `audit_record_e2e.rs` proves audit READS a record rather than re-replaying,
//! and that `--since` exits non-zero on a regression. But every verdict it
//! asserts is either `capability-error` (a flow skipped by an env gate) or
//! hand-written into a record fixture. Nothing there runs a control-bearing
//! flow that genuinely FAILS and checks what verdict the record receives.
//!
//! That is the gap this closes, and it is the highest-consequence false green
//! available: the control map is what a reader trusts when they cannot read
//! the trace, so a control reporting `pass` over a failed flow would misreport
//! the one thing the artifact exists to say.
//!
//! The red path is built the way `api_pipeline.rs` builds its green one -- a
//! real loopback server, with the host travelling as `${API_BASE}` so it never
//! enters the trace. Record against a live endpoint (an honest trace is
//! minted), then run with the port dead, so the assertion fails for real at
//! replay rather than being simulated.
//!
//! Two layers, deliberately, because the streaming false green of 0.9.0 got
//! through by being checked at the wrong one:
//!   1. the VERDICT recorded in `.flowproof/runs/<id>/report.json`, and
//!      re-rendered by `flowproof audit`;
//!   2. the process EXIT CODE of the real binary.
//!
//! A run that failed loudly while recording `verdict: pass` would sail through
//! a one-layer check.

use std::path::{Path, PathBuf};

const FLOWPROOF_BIN: &str = env!("CARGO_BIN_EXE_flowproof");

/// A port with nothing on it. Chosen by binding and dropping, so the number is
/// free at this instant rather than merely assumed to be.
fn dead_base() -> String {
    let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("probe binds");
    let addr = probe.local_addr().expect("probe addr");
    drop(probe);
    format!("http://{addr}")
}

/// Serve `GET /health` -> 200 for up to `requests` requests, then stop.
///
/// Deliberately DETACHED and never joined. Joining would block forever the
/// moment the flow makes fewer probes than the count guessed here -- a hang is
/// a far worse failure mode for a falsifiability test than an over-generous
/// server, because a hung test reports nothing at all.
fn serve(server: tiny_http::Server, requests: usize) {
    std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok(request) = server.recv() else { break };
            let (code, body) = if request.url() == "/health" {
                (200, r#"{"status":"ok"}"#)
            } else {
                (404, r#"{"error":"not found"}"#)
            };
            request
                .respond(tiny_http::Response::from_string(body).with_status_code(code))
                .ok();
        }
    });
}

/// The guilty flow, kept as a committed fixture so it is reviewable as
/// evidence rather than buried in a string literal.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/falsifiability/fixtures/control-verdict-fail.flow.yaml")
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-fals-control-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("work dir");
    dir
}

/// The single `report.json` written under `<dir>/.flowproof/runs`.
fn find_record(dir: &Path) -> Option<PathBuf> {
    let runs = dir.join(".flowproof").join("runs");
    for entry in std::fs::read_dir(&runs).ok()?.filter_map(Result::ok) {
        let candidate = entry.path().join("report.json");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// A control-bearing flow that fails must be recorded as `fail` -- never
/// `pass`, and never quietly downgraded to `capability-error`, which would
/// read as "we could not tell" when in fact we could.
#[test]
fn a_failing_control_flow_is_recorded_as_fail() {
    let dir = work_dir("verdict");
    let spec = dir.join("control-verdict-fail.flow.yaml");
    std::fs::copy(fixture(), &spec).expect("stage fixture");

    // --- Record honestly, against a live endpoint. ---
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let live = format!("http://{}", server.server_addr());
    serve(server, 4);
    std::env::set_var("API_BASE", &live);

    let rec = std::process::Command::new(FLOWPROOF_BIN)
        .arg("record")
        .arg(&spec)
        .current_dir(&dir)
        .env("API_BASE", &live)
        .output()
        .expect("record against the live endpoint");
    assert!(
        rec.status.success(),
        "the recording leg must be honest and green; stdout={} stderr={}",
        String::from_utf8_lossy(&rec.stdout),
        String::from_utf8_lossy(&rec.stderr)
    );

    // --- Now run it with the endpoint gone. The assertion fails for real. ---
    let dead = dead_base();
    let out = std::process::Command::new(FLOWPROOF_BIN)
        .arg("run")
        .arg(&spec)
        .current_dir(&dir)
        .env("API_BASE", &dead)
        .output()
        .expect("run against the dead endpoint");

    // Layer 2: the exit code CI would see.
    assert!(
        !out.status.success(),
        "a flow whose assertion cannot pass must exit non-zero; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    // Layer 1: the verdict the record actually carries.
    let record = find_record(&dir).unwrap_or_else(|| {
        panic!(
            "a failing run must still write a run record, or the control \
             vanishes from the audit map instead of reporting fail; stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    let body = std::fs::read_to_string(&record).expect("read record");
    let value: serde_json::Value = serde_json::from_str(&body).expect("record is JSON");
    let verdict = value["flows"]
        .as_array()
        .expect("flows array")
        .iter()
        .find_map(|f| {
            let c = &f["control"];
            (c["id"] == "falsifiability.control.verdict").then(|| c["verdict"].clone())
        })
        .unwrap_or_else(|| panic!("the record folds the flow's control: {body}"));

    assert_eq!(
        verdict, "fail",
        "a control whose flow failed must be recorded as fail, not {verdict} -- \
         this is the false green the audit map cannot afford: {body}"
    );

    // And `audit` must re-render that verdict rather than soften it.
    let audit = std::process::Command::new(FLOWPROOF_BIN)
        .args(["audit", dir.to_str().expect("utf8"), "--json"])
        .output()
        .expect("audit --json");
    let map: serde_json::Value =
        serde_json::from_slice(&audit.stdout).expect("audit --json is valid JSON");
    let rendered = map["controls"]
        .as_array()
        .expect("controls array")
        .iter()
        .find(|c| c["id"] == "falsifiability.control.verdict")
        .unwrap_or_else(|| {
            panic!(
                "audit names the control: {}",
                String::from_utf8_lossy(&audit.stdout)
            )
        })
        .clone();
    assert_eq!(
        rendered["verdict"], "fail",
        "audit renders the recorded verdict unchanged"
    );

    std::fs::remove_dir_all(&dir).ok();
}
