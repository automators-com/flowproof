//! A suite hook that prints to stdout must not break `--json`. `run --json`
//! prints its report on stdout, and `before_each`/`after_each` inherited
//! that stdout, so one `echo` in a hook put text in front of the JSON and
//! every caller (the desktop app, `flowproof mcp`, CI) failed to parse it.
//! The hook's output now goes to stderr, where it is still visible.
//! Unix-only, like the other suite tests (hooks run through `sh`).
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "flowproof-hook-stdout-{name}-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

/// A local HTTP server answering 200, so an `app: api` flow records and
/// replays without a browser.
fn serve_ok() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}");
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn flowproof(dir: &Path, base: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(args)
        .current_dir(dir)
        .env("HOOK_BASE", base)
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .env_remove("FLOWPROOF_ALLOW_EXEC")
        .output()
        .expect("run flowproof")
}

#[test]
fn a_hook_that_prints_to_stdout_leaves_run_json_parseable() {
    let dir = work_dir("json");
    let base = serve_ok();
    std::fs::write(
        dir.join("only.flow.yaml"),
        "name: only\napp: api\nsteps:\n  - assert_api:\n      request: GET ${HOOK_BASE}/health\n      status: 200\n",
    )
    .expect("spec");
    std::fs::write(
        dir.join("suite.yaml"),
        "before_each: echo hook-said-before\nafter_each: echo hook-said-after\n",
    )
    .expect("suite");
    let recorded = flowproof(&dir, &base, &["record", "--allow-exec", "only.flow.yaml"]);
    assert!(recorded.status.success(), "{recorded:?}");

    for target in ["only.flow.yaml", "."] {
        let out = flowproof(
            &dir,
            &base,
            &["run", "--json", "--allow-exec", "--", target],
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let report: Value = serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("{target}: stdout is not JSON ({e}): {stdout}"));
        assert!(report.is_object(), "{target}: {report}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("hook-said-before") && stderr.contains("hook-said-after"),
            "{target}: the hooks' output is still shown, on stderr: {stderr}"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}
