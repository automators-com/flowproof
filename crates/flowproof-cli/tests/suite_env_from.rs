//! `env_from` end-to-end: a suite.yaml's data command runs, its stdout
//! becomes env vars, `env:` composes on top, and failures abort. Uses
//! `sh`, so unix-only — the semantics under test are platform-neutral.
#![cfg(unix)]

use std::path::PathBuf;

/// Each test gets its own temp suite dir; env mutation is process-global,
/// so distinct var names per test keep parallel tests independent.
fn suite_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-env-from-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("suite dir");
    dir
}

#[test]
fn env_from_stdout_becomes_env_and_env_composes() {
    let dir = suite_dir("capture");
    std::fs::write(
        dir.join("suite.yaml"),
        "env_from: |\n  echo '# minted'\n  echo EF_MATERIAL=100-100\n  echo EF_PRICE=42\n\
         env:\n  EF_URL: http://host/${EF_MATERIAL}\n",
    )
    .expect("manifest");
    let spec = dir.join("x.flow.yaml");
    std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");

    flowproof_cli::apply_suite_context(&spec).expect("context applies");
    assert_eq!(std::env::var("EF_MATERIAL").as_deref(), Ok("100-100"));
    assert_eq!(std::env::var("EF_PRICE").as_deref(), Ok("42"));
    // env: composes over the captured value via ${VAR}.
    assert_eq!(
        std::env::var("EF_URL").as_deref(),
        Ok("http://host/100-100")
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn env_from_nonzero_exit_fails_closed() {
    let dir = suite_dir("exit");
    std::fs::write(
        dir.join("suite.yaml"),
        "env_from: 'echo half >&2; exit 3'\n",
    )
    .expect("manifest");
    let spec = dir.join("x.flow.yaml");
    std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");

    let err = flowproof_cli::apply_suite_context(&spec).expect_err("must fail closed");
    assert!(err.contains("exited with 3"), "{err}");
    assert!(err.contains("half"), "stderr surfaced: {err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn env_from_malformed_output_fails_closed() {
    let dir = suite_dir("malformed");
    std::fs::write(
        dir.join("suite.yaml"),
        "env_from: 'echo EF_OK=1; echo not-a-pair'\n",
    )
    .expect("manifest");
    let spec = dir.join("x.flow.yaml");
    std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");

    let err = flowproof_cli::apply_suite_context(&spec).expect_err("must fail closed");
    assert!(err.contains("line 2"), "{err}");
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn spec_without_a_manifest_is_a_noop() {
    let dir = suite_dir("nomanifest");
    // Nested so no suite.yaml exists between the spec and the temp root —
    // and none is written anywhere.
    let nested = dir.join("inner");
    std::fs::create_dir_all(&nested).expect("dirs");
    let spec = nested.join("x.flow.yaml");
    std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");
    // Must not error; whatever ancestor manifests exist outside the temp
    // tree are out of scope for this test's assertion.
    flowproof_cli::apply_suite_context(&spec).expect("no manifest is fine");
    std::fs::remove_dir_all(&dir).ok();
}
