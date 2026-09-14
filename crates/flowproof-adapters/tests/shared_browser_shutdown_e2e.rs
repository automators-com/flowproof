//! Real reliability finding, not a hypothetical: scoring the Fiori Phase 0c
//! corpus tonight (`evals/fiori/dev/`) left 28 orphaned Chrome process trees
//! behind, dating back hours, plus 3.5GB of their leaked temp profile
//! directories - one full leak per `flowproof` invocation. `ps` showed each
//! process still alive with its real parent already exited (reparented to
//! init), confirming this is not a timing artifact.
//!
//! Root cause: `shared_browser` (web.rs) holds the shared `Browser` in a
//! process-lifetime `static` so a whole suite pays Chrome's cold start once.
//! Its own doc comment claims this "keeps the process alive until the test
//! binary exits" - true only in the sense that nothing stops it sooner. A
//! child process is not killed automatically when its parent exits on macOS
//! or Linux, and Rust does not run a `static`'s destructor on a normal
//! process exit either, so the shared path got zero cleanup attempts, not
//! even a failed one. Even the underlying `headless_chrome` fork's own
//! `Drop` (`BrowserInner::drop`, which the private-browser path does rely
//! on) only asks Chrome to close itself gracefully over CDP and silently
//! swallows any error via `.ok()` - it never force-kills the process either.
//!
//! `shutdown_shared_browser` closes this: `flowproof-cli::run_cli` holds a
//! guard whose `Drop` calls it unconditionally when the CLI process is
//! about to exit, and it force-kills the shared browser's OS PID after
//! giving the graceful path its normal chance.

#[test]
fn shutdown_shared_browser_actually_kills_the_process() {
    if std::env::var("FLOWPROOF_E2E").as_deref() != Ok("1") {
        eprintln!("skipping shared-browser-shutdown measurement: set FLOWPROOF_E2E=1 to run it");
        return;
    }

    // Force the shared-browser path regardless of the ambient environment -
    // this finding is specifically about that path (`should_share_browser`
    // in web.rs: shared unless headed, kept-open, or opted out).
    // SAFETY: this test does not run concurrently with other tests that
    // read these vars (single-threaded by nature of driving one real
    // browser), and each is restored before returning.
    let saved: Vec<(&str, Option<std::ffi::OsString>)> = [
        "FLOWPROOF_NO_SHARED_BROWSER",
        "FLOWPROOF_HEADED",
        "FLOWPROOF_KEEP_BROWSER_OPEN",
    ]
    .into_iter()
    .map(|k| (k, std::env::var_os(k)))
    .collect();
    for (k, _) in &saved {
        unsafe { std::env::remove_var(k) };
    }

    let driver = flowproof_adapters::WebAppDriver::new().expect("browser launches");
    let pid = driver
        .browser_process_id()
        .expect("a launched browser reports its PID");

    assert!(
        pid_is_alive(pid),
        "the shared browser's own PID {pid} must be a real, running process \
         right after launch - otherwise this test proves nothing"
    );

    // The driver itself going out of scope must NOT kill the shared
    // browser - that is the whole point of sharing it across flows. Prove
    // the process survives driver teardown, then that
    // `shutdown_shared_browser` (and only it) terminates it.
    drop(driver);
    assert!(
        pid_is_alive(pid),
        "a per-flow driver's own Drop must not kill the SHARED browser - \
         only shutdown_shared_browser (called once, at CLI exit) may"
    );

    flowproof_adapters::shutdown_shared_browser();

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while pid_is_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        !pid_is_alive(pid),
        "pid {pid} is still alive after shutdown_shared_browser() - the \
         orphaned-Chrome leak this test guards against is back"
    );

    for (k, v) in saved {
        if let Some(v) = v {
            unsafe { std::env::set_var(k, v) };
        }
    }
}

/// Whether a PID currently names a running process, portably enough for
/// this test's one real use (macOS/Linux CI; the shared-browser path this
/// covers is not exercised on Windows the same way).
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(windows)]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}")])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false)
    }
}
