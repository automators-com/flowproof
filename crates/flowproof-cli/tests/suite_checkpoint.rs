//! Offline real CLI record/replay proves recovery does not replay a confirmed
//! operation, retains its exports, and refuses ambiguous or changed inputs.
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Fixture {
    dir: PathBuf,
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
    status: Arc<Mutex<u16>>,
    hang: Arc<AtomicBool>,
}
impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "flowproof-checkpoint-{name}-{}",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("fixture operation succeeds");
        let server = tiny_http::Server::http("127.0.0.1:0").expect("fixture operation succeeds");
        let base = format!("http://{}", server.server_addr());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&requests);
        let status = Arc::new(Mutex::new(200));
        let served_status = Arc::clone(&status);
        let hang = Arc::new(AtomicBool::new(false));
        let served_hang = Arc::clone(&hang);
        std::thread::spawn(move || {
            for request in server.incoming_requests() {
                observed
                    .lock()
                    .expect("fixture state lock")
                    .push(request.url().into());
                while served_hang.load(Ordering::Relaxed) {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                request
                    .respond(
                        tiny_http::Response::from_string(r#"{"id":"0010"}"#)
                            .with_status_code(*served_status.lock().expect("fixture state lock")),
                    )
                    .expect("fixture operation succeeds");
            }
        });
        let fixture = Self {
            dir,
            base,
            requests,
            status,
            hang,
        };
        for (name, route, exports) in [
            (
                "create",
                "create",
                "exports:\n  CHECKPOINT_ORDER: ${INPUT_ID}\n",
            ),
            ("verify", "verify/${CHECKPOINT_ORDER}", ""),
        ] {
            let spec = format!("{name}.flow.yaml");
            std::fs::write(fixture.dir.join(&spec), format!("name: {name}\napp: api\nsteps:\n  - assert_api:\n      request: GET ${{CHECKPOINT_BASE}}/{route}\n      status: 200\n      timeout_seconds: 1\n      body_json: id\n      equals: ${{INPUT_ID}}\n{exports}")).expect("fixture operation succeeds");
            let output = fixture
                .command()
                .args(["record", &spec, "--author", "rules", "--no-repair"])
                .env("CHECKPOINT_ORDER", "0010")
                .output()
                .expect("fixture operation succeeds");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::write(fixture.dir.join("suite.yaml"), "flows: [create.flow.yaml, verify.flow.yaml]\ndepends_on:\n  verify.flow.yaml: [create.flow.yaml]\nstop_on_failure: true\n").expect("fixture operation succeeds");
        fixture.requests.lock().expect("fixture state lock").clear();
        fixture
    }
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_flowproof"));
        cmd.current_dir(&self.dir)
            .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
            .env("CHECKPOINT_BASE", &self.base)
            .env("INPUT_ID", "0010")
            .env_remove("CHECKPOINT_ORDER");
        cmd
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command()
            .args(["run", ".", "--json", "--checkpoint", "progress.json"])
            .args(args)
            .output()
            .expect("fixture operation succeeds")
    }
    fn assert_error_without_requests(&self, output: Output, message: &str) {
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(message),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(self.requests.lock().expect("fixture state lock").is_empty());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.dir).ok();
    }
}
fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn pause_resume_retains_exports_and_never_reexecutes_confirmed_stages() {
    let f = Fixture::new("resume");
    let first = f.run(&["--stop-after", "create.flow.yaml"]);
    assert_eq!(
        first.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert_eq!(json(&first)["blocked"], 1);
    assert_eq!(
        *f.requests.lock().expect("fixture state lock"),
        vec!["/create"]
    );
    let saved =
        std::fs::read_to_string(f.dir.join("progress.json")).expect("fixture operation succeeds");
    assert!(
        saved.contains("0010"),
        "the private checkpoint retains the resolved ID"
    );
    assert!(
        !String::from_utf8_lossy(&first.stdout).contains("0010"),
        "exports must not leak into JSON output"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(f.dir.join("progress.json"))
                .expect("fixture operation succeeds")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    f.requests.lock().expect("fixture state lock").clear();
    f.assert_error_without_requests(f.run(&[]), "already exists");
    let resumed = f.run(&["--resume"]);
    assert_eq!(
        resumed.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert_eq!(json(&resumed)["flows"][0]["resumed"], true);
    assert_eq!(
        *f.requests.lock().expect("fixture state lock"),
        vec!["/verify/0010"]
    );
    f.requests.lock().expect("fixture state lock").clear();
    let records_before = std::fs::read_dir(f.dir.join(".flowproof/runs"))
        .expect("audit records")
        .count();
    let complete = f.run(&["--resume"]);
    assert_eq!(complete.status.code(), Some(0));
    assert_eq!(
        std::fs::read_dir(f.dir.join(".flowproof/runs"))
            .expect("audit records")
            .count(),
        records_before,
        "resuming old evidence must not mint a new audit timestamp"
    );
    assert!(
        f.requests.lock().expect("fixture state lock").is_empty(),
        "completed checkpoint makes no system requests"
    );
}

#[test]
fn changed_inputs_and_corrupted_or_locked_checkpoints_fail_before_replay() {
    let f = Fixture::new("integrity");
    assert_eq!(
        f.run(&["--stop-after", "create.flow.yaml"]).status.code(),
        Some(1)
    );
    f.requests.lock().expect("fixture state lock").clear();
    let changed = f
        .command()
        .args(["run", ".", "--checkpoint", "progress.json", "--resume"])
        .env("INPUT_ID", "0099")
        .output()
        .expect("fixture operation succeeds");
    f.assert_error_without_requests(changed, "inputs changed");
    for file in ["verify.flow.yaml", "verify.trace.jsonl", "suite.yaml"] {
        let path = f.dir.join(file);
        let original = std::fs::read_to_string(&path).expect("fixture operation succeeds");
        // Whitespace leaves valid data but changes reviewed bytes.
        std::fs::write(&path, format!("{original}\n")).expect("fixture operation succeeds");
        let output = f.run(&["--resume"]);
        f.assert_error_without_requests(output, "inputs changed");
        std::fs::write(&path, original).expect("fixture operation succeeds");
    }
    std::fs::write(f.dir.join("verify.values.yaml"), "INPUT_ID: '0010'\n")
        .expect("fixture operation succeeds");
    f.assert_error_without_requests(f.run(&["--resume"]), "inputs changed");
    std::fs::remove_file(f.dir.join("verify.values.yaml")).expect("fixture operation succeeds");
    std::fs::write(f.dir.join("progress.json.lock"), "active or stale\n")
        .expect("fixture operation succeeds");
    f.assert_error_without_requests(f.run(&["--resume"]), "locked");
    std::fs::remove_file(f.dir.join("progress.json.lock")).expect("fixture operation succeeds");
    let path = f.dir.join("progress.json");
    let original = std::fs::read_to_string(&path).expect("fixture operation succeeds");
    std::fs::write(&path, original.replace("0010", "tampered"))
        .expect("fixture operation succeeds");
    f.assert_error_without_requests(f.run(&["--resume"]), "integrity");
}

#[test]
fn failed_flow_cannot_be_retried_by_resume() {
    let f = Fixture::new("uncertain");
    *f.status.lock().expect("fixture state lock") = 409;
    let failed = f.run(&[]);
    assert_eq!(failed.status.code(), Some(1));
    assert_eq!(json(&failed)["blocked"], 1);
    f.requests.lock().expect("fixture state lock").clear();
    *f.status.lock().expect("fixture state lock") = 200;
    f.assert_error_without_requests(f.run(&["--resume"]), "uncertain outcome");
    let saved =
        std::fs::read_to_string(f.dir.join("progress.json")).expect("fixture operation succeeds");
    assert!(saved.contains("\"started\": \"create.flow.yaml\""));
}

#[test]
fn unsafe_options_and_export_overrides_are_refused_before_execution() {
    let f = Fixture::new("guards");
    for (args, message) in [
        (vec!["--retries", "1"], "refuse retries"),
        (vec!["--record-missing"], "refuse retries"),
        (
            vec!["--stop-after", "absent.flow.yaml"],
            "not a selected flow",
        ),
        (
            vec!["--var", "CHECKPOINT_ORDER=stale"],
            "must not be overwritten",
        ),
    ] {
        f.assert_error_without_requests(f.run(&args), message);
    }
    let path = f.dir.join("suite.yaml");
    let original = std::fs::read_to_string(&path).expect("fixture operation succeeds");
    for hook in ["env_from", "before_each", "after_each"] {
        std::fs::write(&path, format!("{original}{hook}: exit 99\n"))
            .expect("fixture operation succeeds");
        f.assert_error_without_requests(f.run(&[]), "refuse env_from and shell hooks");
    }
}

#[test]
fn datamaker_json_values_preserve_flat_scalar_identity_in_native_vars() {
    let f = Fixture::new("native-values");
    let values = serde_json::json!({"CHECKPOINT_BASE": f.base, "INPUT_ID": "0010", "PRICE": "0.50", "EMPTY": "", "FLAG": false, "COUNT": 0});
    std::fs::write(
        f.dir.join("inputs.values.yaml"),
        serde_json::to_vec_pretty(&values).expect("fixture operation succeeds"),
    )
    .expect("fixture operation succeeds");
    let spec = "name: Native values\napp: api\nsteps:\n  - assert_api:\n      request: GET ${CHECKPOINT_BASE}/values/${INPUT_ID}/${PRICE}/${FLAG}/${COUNT}?empty=${EMPTY}\n      status: 200\n";
    std::fs::write(f.dir.join("values.flow.yaml"), spec).expect("fixture operation succeeds");
    let record = f
        .command()
        .args([
            "record",
            "values.flow.yaml",
            "--vars",
            "inputs.values.yaml",
            "--author",
            "rules",
            "--no-repair",
        ])
        .output()
        .expect("fixture operation succeeds");
    assert!(
        record.status.success(),
        "{}",
        String::from_utf8_lossy(&record.stderr)
    );
    f.requests.lock().expect("fixture state lock").clear();
    let replay = f
        .command()
        .args(["run", "values.flow.yaml", "--vars", "inputs.values.yaml"])
        .output()
        .expect("fixture operation succeeds");
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        *f.requests.lock().expect("fixture state lock"),
        vec!["/values/0010/0.50/false/0?empty="]
    );
}

#[test]
fn killed_flow_keeps_its_claim_after_stale_lock_is_reconciled() {
    let f = Fixture::new("interrupted");
    f.hang.store(true, Ordering::Relaxed);
    let mut child = f
        .command()
        .args(["run", ".", "--checkpoint", "progress.json"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("fixture operation succeeds");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while f.requests.lock().expect("fixture state lock").is_empty()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    child.kill().expect("fixture operation succeeds");
    assert!(!child.wait().expect("fixture operation succeeds").success());
    f.hang.store(false, Ordering::Relaxed);
    assert!(
        !f.requests.lock().expect("fixture state lock").is_empty(),
        "the operation must have begun before killing it"
    );
    f.requests.lock().expect("fixture state lock").clear();
    f.assert_error_without_requests(f.run(&["--resume"]), "locked");
    // The process is confirmed dead, so the stale mutex may be removed.
    // This must never clear the durable business-operation claim.
    std::fs::remove_file(f.dir.join("progress.json.lock")).expect("fixture operation succeeds");
    f.assert_error_without_requests(f.run(&["--resume"]), "uncertain outcome");
}

#[test]
fn changing_a_pending_values_file_during_a_run_never_executes_that_stage() {
    let f = Fixture::new("in-flight-inputs");
    f.hang.store(true, Ordering::Relaxed);
    let mut child = f
        .command()
        .args(["run", ".", "--checkpoint", "progress.json"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("start guarded run");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(90);
    while f.requests.lock().expect("fixture state lock").is_empty()
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // A values file absent during preflight appears while creation is active.
    std::fs::write(f.dir.join("verify.values.yaml"), "INPUT_ID: '0099'\n")
        .expect("change pending inputs");
    f.hang.store(false, Ordering::Relaxed);
    let status = child.wait().expect("guarded run exits");
    assert_eq!(status.code(), Some(2));
    assert_eq!(
        *f.requests.lock().expect("fixture state lock"),
        vec!["/create"],
        "changed verification must not execute"
    );
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(f.dir.join("progress.json")).expect("checkpoint"))
            .expect("checkpoint JSON");
    assert_eq!(
        saved["state"]["confirmed"]
            .as_array()
            .expect("confirmed stages")
            .len(),
        1
    );
    assert!(
        saved["state"]["started"].is_null(),
        "confirmed creation remains resumable after restoring reviewed inputs"
    );
}
