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
fn unresolvable_env_entry_warns_and_skips_instead_of_blocking() {
    let dir = suite_dir("lazy");
    std::fs::write(
        dir.join("suite.yaml"),
        "env:\n  LZ_OK: hello\n  LZ_BROKEN: ${LZ_DEFINITELY_UNSET_XYZ}\n",
    )
    .expect("manifest");
    let spec = dir.join("x.flow.yaml");
    std::fs::write(&spec, "name: x\napp: web\nsteps:\n  - Type 1\n").expect("spec");

    std::env::remove_var("LZ_BROKEN");
    // Must NOT error: the broken entry is skipped, the good one applies.
    flowproof_cli::apply_suite_context(&spec).expect("lazy env never blocks the context");
    assert_eq!(std::env::var("LZ_OK").as_deref(), Ok("hello"));
    assert!(
        std::env::var("LZ_BROKEN").is_err(),
        "unresolvable entry must not be exported"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn flow_referencing_a_skipped_var_still_fails_at_use_naming_it() {
    let dir = suite_dir("lazy-use");
    std::fs::write(
        dir.join("suite.yaml"),
        "env:\n  LZU_API: ${LZU_UNSET_AMBIENT_XYZ}\n",
    )
    .expect("manifest");
    // An api flow that references the skipped key: context applies fine,
    // but recording fails at moment-of-use, naming the variable.
    let spec = dir.join("uses.flow.yaml");
    std::fs::write(
        &spec,
        "name: uses\napp: api\nsteps:\n  - assert_api:\n      request: GET ${LZU_API}/x\n",
    )
    .expect("spec");
    std::env::remove_var("LZU_API");

    let code = flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]);
    assert_eq!(code, 2, "moment-of-use failure is a hard error");
    // And a sibling flow that never references it is unaffected.
    let ok_spec = dir.join("clean.flow.yaml");
    std::fs::write(
        &ok_spec,
        "name: clean\napp: api\nsteps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n      timeout_seconds: 1\n",
    )
    .expect("spec");
    // (Fails on connection, not on env — proving env didn't block it.)
    let code = flowproof_cli::run_cli(["record", ok_spec.to_str().expect("utf8")]);
    assert_eq!(code, 2, "fails on the unreachable host, not before");
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

/// Field regression (cypress-realworld-app, round 3): a mint script that
/// logs in over HTTP to produce a session needs the suite's own base URL
/// and password, and got neither - the data command ran with `env:` not yet
/// applied, so `$RWA_API` was empty and the flow failed closed downstream
/// with a 401. Minting test data almost always needs the suite's config,
/// so this is the common case rather than an edge.
#[test]
fn env_from_command_sees_the_suites_own_env() {
    let dir = suite_dir("sees-env");
    std::fs::write(
        dir.join("suite.yaml"),
        "env:\n  EF_BASE: http://example.test\nenv_from: printf 'EF_MINTED=%s/minted\\n' \"$EF_BASE\"\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("probe.flow.yaml"),
        "name: Probe\napp: api\nsteps:\n  - assert_api:\n      request: GET ${EF_MINTED}\n      status: 200\n",
    )
    .expect("spec");

    // Running the suite applies env_from; the command must have seen
    // EF_BASE and composed the minted value from it.
    flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(
        std::env::var("EF_MINTED").as_deref(),
        Ok("http://example.test/minted"),
        "the data command must see the suite's env: block"
    );

    std::env::remove_var("EF_MINTED");
    std::env::remove_var("EF_BASE");
    std::fs::remove_dir_all(&dir).ok();
}

/// The OTHER ordering is unchanged, and the two are easy to conflate:
/// what the command SEES changed, but which value WINS for `${VAR}` at flow
/// time did not - `env:` still overrides the data command's output.
#[test]
fn env_precedence_is_unchanged_env_still_wins_over_env_from() {
    let dir = suite_dir("precedence");
    std::fs::write(
        dir.join("suite.yaml"),
        "env:\n  EP_SHARED: from-env-block\nenv_from: echo EP_SHARED=from-data-command\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("probe.flow.yaml"),
        "name: Probe\napp: api\nsteps:\n  - assert_api:\n      request: GET http://127.0.0.1:1/x\n      status: 200\n",
    )
    .expect("spec");

    flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(
        std::env::var("EP_SHARED").as_deref(),
        Ok("from-env-block"),
        "env: must still win over env_from output"
    );

    std::env::remove_var("EP_SHARED");
    std::fs::remove_dir_all(&dir).ok();
}

/// An `env:` entry that cannot resolve YET must not break the data command:
/// it may reference that very command's output, and it gets its turn when
/// `env:` is applied afterwards.
#[test]
fn an_unresolvable_env_entry_does_not_break_the_data_command() {
    let dir = suite_dir("forward-ref");
    std::fs::write(
        dir.join("suite.yaml"),
        "env:\n  FR_DERIVED: ${FR_MINTED}/derived\nenv_from: echo FR_MINTED=http://minted.test\n",
    )
    .expect("manifest");
    std::fs::write(
        dir.join("probe.flow.yaml"),
        "name: Probe\napp: api\nsteps:\n  - assert_api:\n      request: GET ${FR_DERIVED}\n      status: 200\n",
    )
    .expect("spec");

    flowproof_cli::run_cli(["run", dir.to_str().expect("utf8")]);
    assert_eq!(
        std::env::var("FR_MINTED").as_deref(),
        Ok("http://minted.test"),
        "the data command still ran"
    );
    assert_eq!(
        std::env::var("FR_DERIVED").as_deref(),
        Ok("http://minted.test/derived"),
        "an entry referencing the command's own output resolves afterwards"
    );

    std::env::remove_var("FR_MINTED");
    std::env::remove_var("FR_DERIVED");
    std::fs::remove_dir_all(&dir).ok();
}
