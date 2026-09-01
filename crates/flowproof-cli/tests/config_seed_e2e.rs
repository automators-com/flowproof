//! Phase 2 of plans/001-credential-config.md: `apply_suite_context` seeds
//! `${VAR}`s from `flowproof config`'s file, fill-gaps-only, as its very
//! first action — even for a bare single flow with no governing suite.yaml
//! at all, which is the case with nothing else to fall back on.
//!
//! `HOME` and the mapped `SAP_*` vars are process-global, so — like
//! `sap_com.rs`'s own credential tests — everything here runs under one
//! lock rather than side by side.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-config-seed-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Point `dirs::config_dir()` at a fake `HOME` for the duration of `body`,
/// restoring whatever was there before (and clearing `XDG_CONFIG_HOME`,
/// which would otherwise win over `HOME` on Linux).
fn with_fake_home<T>(home: &std::path::Path, body: impl FnOnce() -> T) -> T {
    let previous_home = std::env::var_os("HOME");
    let previous_xdg = std::env::var_os("XDG_CONFIG_HOME");
    std::env::set_var("HOME", home);
    std::env::remove_var("XDG_CONFIG_HOME");

    let result = body();

    match previous_home {
        Some(v) => std::env::set_var("HOME", v),
        None => std::env::remove_var("HOME"),
    }
    match previous_xdg {
        Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
        None => std::env::remove_var("XDG_CONFIG_HOME"),
    }
    result
}

#[test]
fn an_unset_var_is_filled_from_the_config_file_for_a_bare_flow() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_dir("fill-home");
    let spec_dir = temp_dir("fill-spec");
    std::env::remove_var("SAP_CONNECTION");

    with_fake_home(&home, || {
        let config = flowproof_cli::config::Config {
            sap: Some(flowproof_cli::config::SapProfile {
                connection: Some("TS9".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        flowproof_cli::config::save(&config).expect("fixture config writes");

        let spec = spec_dir.join("x.flow.yaml");
        std::fs::write(&spec, "name: x\napp: sap\nsteps:\n  - Type 1\n").expect("spec");
        // No suite.yaml anywhere above `spec` — the case this test exists for.
        flowproof_cli::apply_suite_context(&spec).expect("no manifest is still fine");
    });

    assert_eq!(
        std::env::var("SAP_CONNECTION").as_deref(),
        Ok("TS9"),
        "the unset var is filled from the config file"
    );
    std::env::remove_var("SAP_CONNECTION");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&spec_dir).ok();
}

#[test]
fn an_already_set_var_wins_over_the_config_file() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_dir("winover-home");
    let spec_dir = temp_dir("winover-spec");
    std::env::set_var("SAP_CONNECTION", "FROM_SHELL");

    with_fake_home(&home, || {
        let config = flowproof_cli::config::Config {
            sap: Some(flowproof_cli::config::SapProfile {
                connection: Some("FROM_CONFIG_FILE".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        flowproof_cli::config::save(&config).expect("fixture config writes");

        let spec = spec_dir.join("x.flow.yaml");
        std::fs::write(&spec, "name: x\napp: sap\nsteps:\n  - Type 1\n").expect("spec");
        flowproof_cli::apply_suite_context(&spec).expect("no manifest is still fine");
    });

    assert_eq!(
        std::env::var("SAP_CONNECTION").as_deref(),
        Ok("FROM_SHELL"),
        "an already-set var is left alone; the config file never overrides"
    );
    std::env::remove_var("SAP_CONNECTION");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&spec_dir).ok();
}

/// A suite's own `env:` still overrides unconditionally, exactly as it does
/// today — the config file only ever fills the gap the suite left, matching
/// `apply_suite_env`'s existing precedent at the other end of the stack.
#[test]
fn a_suites_own_env_still_overrides_the_config_file() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_dir("suite-wins-home");
    let spec_dir = temp_dir("suite-wins-spec");
    std::env::remove_var("SAP_CONNECTION");

    with_fake_home(&home, || {
        let config = flowproof_cli::config::Config {
            sap: Some(flowproof_cli::config::SapProfile {
                connection: Some("FROM_CONFIG_FILE".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        flowproof_cli::config::save(&config).expect("fixture config writes");

        std::fs::write(
            spec_dir.join("suite.yaml"),
            "env:\n  SAP_CONNECTION: FROM_SUITE\n",
        )
        .expect("manifest");
        let spec = spec_dir.join("x.flow.yaml");
        std::fs::write(&spec, "name: x\napp: sap\nsteps:\n  - Type 1\n").expect("spec");
        flowproof_cli::apply_suite_context(&spec).expect("context applies");
    });

    assert_eq!(
        std::env::var("SAP_CONNECTION").as_deref(),
        Ok("FROM_SUITE"),
        "the suite's own env: still wins over the global config file"
    );
    std::env::remove_var("SAP_CONNECTION");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&spec_dir).ok();
}

/// `flowproof run <suite-dir>` takes a different code path than a bare
/// single-flow `run`/`record` — it never went through `apply_suite_context`,
/// so the config file was invisible to it even though the single-flow path
/// already picked it up. Same fixture shape as the bare-flow test above, but
/// exercised through `run_cli(["run", <dir>])`, the actual suite-mode entry
/// point.
#[test]
fn an_unset_var_is_filled_from_the_config_file_for_a_suite_directory() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_dir("suite-fill-home");
    let spec_dir = temp_dir("suite-fill-spec");
    std::env::remove_var("SAP_CONNECTION");

    with_fake_home(&home, || {
        let config = flowproof_cli::config::Config {
            sap: Some(flowproof_cli::config::SapProfile {
                connection: Some("FROM_CONFIG_FILE".into()),
                ..Default::default()
            }),
            fiori: None,
        };
        flowproof_cli::config::save(&config).expect("fixture config writes");

        std::fs::write(
            spec_dir.join("x.flow.yaml"),
            "name: x\napp: sap\nsteps:\n  - Type 1\n",
        )
        .expect("spec");
        // The flow has no trace and no suite.yaml; run_cli should still fail
        // to actually run it, but seeding happens before any of that.
        flowproof_cli::run_cli(["run", &spec_dir.to_string_lossy()]);
    });

    assert_eq!(
        std::env::var("SAP_CONNECTION").as_deref(),
        Ok("FROM_CONFIG_FILE"),
        "the unset var is filled from the config file for a suite-directory run too"
    );
    std::env::remove_var("SAP_CONNECTION");
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&spec_dir).ok();
}
