//! `flowproof doctor` end to end: the whole point is that it distinguishes
//! an agent that reached the proxy from one that did not, without a spec,
//! without assertions and without an API key.
#![cfg(unix)]

use std::io::Write;

fn work_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-doctor-e2e-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn script(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut f = std::fs::File::create(&path).expect("create");
    f.write_all(body.as_bytes()).expect("write");
    drop(f);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    path
}

/// An agent that honours the injected base URL reaches the proxy, and doctor
/// exits 0. No key is set anywhere in this test.
#[test]
fn an_agent_that_honours_the_base_url_reaches_the_proxy() {
    let dir = work_dir("wired");
    let agent = script(
        &dir,
        "agent.sh",
        "#!/bin/sh\n\
         curl -sS -X POST \"$OPENAI_BASE_URL/chat/completions\" \
           -H 'content-type: application/json' \
           -d '{\"model\":\"x\",\"messages\":[]}' >/dev/null\n",
    );
    let code = flowproof_cli::run_cli([
        "doctor",
        "--agent",
        agent.to_str().expect("utf8"),
        "--timeout",
        "30",
    ]);
    assert_eq!(code, 0, "a wired agent must pass doctor");
    std::fs::remove_dir_all(&dir).ok();
}

/// The failure this command exists for: a client reading its own variable,
/// which flowproof never set. It must FAIL, not pass quietly - a green
/// doctor on an unwired agent would send someone off to record and waste a
/// key discovering the same thing.
#[test]
fn an_agent_that_ignores_the_base_url_fails_doctor() {
    let dir = work_dir("unwired");
    let agent = script(
        &dir,
        "agent.sh",
        "#!/bin/sh\necho \"would call ${MY_OWN_GATEWAY:-https://real.example}\" >&2\n",
    );
    let code = flowproof_cli::run_cli([
        "doctor",
        "--agent",
        agent.to_str().expect("utf8"),
        "--timeout",
        "30",
    ]);
    assert_ne!(code, 0, "an unwired agent must fail doctor");
    std::fs::remove_dir_all(&dir).ok();
}

/// A command that cannot start is a setup error, not a wiring verdict.
#[test]
fn a_command_that_does_not_exist_is_an_error() {
    let code = flowproof_cli::run_cli([
        "doctor",
        "--agent",
        "/nonexistent/flowproof-doctor-agent",
        "--timeout",
        "10",
    ]);
    assert_ne!(code, 0);
}
