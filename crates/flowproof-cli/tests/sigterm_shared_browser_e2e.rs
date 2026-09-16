//! Real regression, reproduced with a plain `kill -TERM <pid>` against a
//! running `flowproof run` (issue #605): `SharedBrowserGuard`'s `Drop` only
//! runs on a normal unwind, and `SIGTERM`'s default disposition terminates
//! the process immediately without one, so the shared Chrome process tree
//! leaked exactly like the #593/#594 bug it was written to close - just via
//! a different door. `run_cli_for_engine` now installs a `ctrlc` handler
//! (`termination` feature) that force-kills the shared browser before the
//! process actually exits on `SIGINT`/`SIGTERM`.
//!
//! This drives the REAL `flowproof` binary as its own OS process (not
//! `run_cli` in-process) because the bug is specifically about what happens
//! across a real signal delivered from outside the process.
#![cfg(unix)]

use std::io::Read;
use std::time::{Duration, Instant};

#[test]
fn sigterm_kills_the_shared_browser_before_the_process_exits() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping sigterm E2E test: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    let dir = std::env::temp_dir().join("flowproof-sigterm-e2e");
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    // Mirrors web_e2e.rs's "slow" pattern: a real ~3s async delay gives the
    // child process a comfortable window to be mid-flow (browser launched,
    // not yet exited) when the signal lands, without any fixed sleep on
    // either side of this test racing against it.
    let page = dir.join("slow.html");
    std::fs::write(
        &page,
        r#"<!doctype html><html><body>
            <div id="out">waiting</div>
            <script>
                setTimeout(() => {
                    document.getElementById('out').textContent = 'done';
                }, 3000);
            </script>
        </body></html>"#,
    )
    .expect("page written");

    let spec_yaml = format!(
        "name: Sigterm target\napp: web\nurl: file://{}\nsteps:\n  \
         - assert: page shows done within 10s\n",
        page.display()
    );
    let spec_path = dir.join("sigterm.flow.yaml");
    std::fs::write(&spec_path, &spec_yaml).expect("spec written");
    let spec = flowproof_agent::FlowSpec::parse(&spec_yaml).expect("spec parses");
    let trace_path = flowproof_cli::default_trace_path(&spec_path);

    // Record in-process, then make sure this process's OWN shared browser is
    // gone before measuring the CHILD process's one below.
    let mut driver = flowproof_cli::driver_for("web").expect("browser launches");
    flowproof_agent::record(&spec, &mut driver, &trace_path).expect("recording succeeds");
    drop(driver);
    flowproof_adapters::shutdown_shared_browser();

    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(["run", spec_path.to_str().expect("utf8")])
        .env_remove("FLOWPROOF_NO_SHARED_BROWSER")
        .env_remove("FLOWPROOF_HEADED")
        .env_remove("FLOWPROOF_KEEP_BROWSER_OPEN")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("flowproof run spawns");
    let child_pid = child.id();

    let browser_pid = wait_for_child_pid(child_pid, Duration::from_secs(10)).unwrap_or_else(|| {
        // Reap the child so it doesn't outlive an aborted assertion.
        let _ = child.kill();
        let _ = child.wait();
        panic!(
            "the run must launch its own Chrome process as a direct child of pid {child_pid} \
             within 10s - none appeared, so this test proves nothing"
        );
    });
    assert!(
        pid_is_alive(browser_pid),
        "browser pid {browser_pid} must be running right after it is found"
    );

    // The exact reproduction from the issue: a plain SIGTERM against the
    // running `flowproof` process, not a graceful Ctrl-C the loop chooses to
    // honor.
    assert!(
        std::process::Command::new("kill")
            .args(["-TERM", &child_pid.to_string()])
            .status()
            .expect("kill runs")
            .success(),
        "SIGTERM must actually reach pid {child_pid}"
    );

    let status = child.wait().expect("child process is waited on");
    assert!(
        !status.success(),
        "sanity: the run was interrupted mid-flow, not completed: {status:?}"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while pid_is_alive(browser_pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !pid_is_alive(browser_pid),
        "browser pid {browser_pid} survived SIGTERM to its flowproof parent {child_pid} - \
         the exact orphaned-Chrome leak this test guards against"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Poll for a direct OS child of `parent` to appear (the shared browser is
/// the only process `flowproof run` spawns for this flow) and return its
/// pid, or `None` if the deadline passes first.
fn wait_for_child_pid(parent: u32, timeout: Duration) -> Option<u32> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(pid) = direct_child_pid(parent) {
            return Some(pid);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    None
}

fn direct_child_pid(parent: u32) -> Option<u32> {
    let mut out = std::process::Command::new("pgrep")
        .args(["-P", &parent.to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut buf = String::new();
    out.stdout.take()?.read_to_string(&mut buf).ok()?;
    out.wait().ok()?;
    buf.lines().find_map(|l| l.trim().parse().ok())
}

fn pid_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
