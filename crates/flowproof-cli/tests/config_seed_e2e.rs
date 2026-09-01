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

fn serve_health(server: tiny_http::Server, requests: usize) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        for _ in 0..requests {
            let Ok(request) = server.recv() else { break };
            let response = tiny_http::Response::from_string(r#"{"status":"ok"}"#)
                .with_status_code(if request.url() == "/health" { 200 } else { 404 });
            request.respond(response).ok();
        }
    })
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
fn a_suite_less_single_flow_resolves_sap_refs_from_the_config_file() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_dir("single-run-home");
    let spec_dir = temp_dir("single-run-spec");
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_health(server, 2);
    std::env::remove_var("SAP_USER");

    with_fake_home(&home, || {
        let config = flowproof_cli::config::Config {
            sap: Some(flowproof_cli::config::SapProfile {
                user: Some(base),
                ..Default::default()
            }),
            fiori: None,
        };
        flowproof_cli::config::save(&config).expect("fixture config writes");

        let spec = spec_dir.join("x.flow.yaml");
        std::fs::write(
            &spec,
            "name: x\napp: api\nsteps:\n  - assert_api:\n      request: GET ${SAP_USER}/health\n      status: 200\n",
        )
        .expect("spec");
        // No suite.yaml anywhere above `spec`: both commands must reach the
        // API flow using only `flowproof config`'s seeded env fallback.
        assert_eq!(
            flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]),
            0,
            "record succeeds without a suite.yaml"
        );
        std::env::remove_var("SAP_USER");
        assert_eq!(
            flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]),
            0,
            "run succeeds without a suite.yaml"
        );
    });

    std::env::remove_var("SAP_USER");
    server_thread.join().ok();
    std::fs::remove_dir_all(&home).ok();
    std::fs::remove_dir_all(&spec_dir).ok();
}

#[test]
fn a_suite_less_single_flow_resolves_business_data_from_sibling_values_file() {
    let _guard = ENV.lock().expect("env lock");
    let spec_dir = temp_dir("single-values-spec");
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let server_thread = serve_health(server, 2);
    std::env::remove_var("API_BASE");

    let spec = spec_dir.join("x.flow.yaml");
    std::fs::write(
        &spec,
        "name: x\napp: api\nsteps:\n  - assert_api:\n      request: GET ${API_BASE}/health\n      status: 200\n",
    )
    .expect("spec");
    std::fs::write(
        spec_dir.join("x.values.yaml"),
        format!("API_BASE: {base}\n"),
    )
    .expect("values");

    assert_eq!(
        flowproof_cli::run_cli(["record", spec.to_str().expect("utf8")]),
        0,
        "record succeeds from sibling values"
    );
    std::env::remove_var("API_BASE");
    assert_eq!(
        flowproof_cli::run_cli(["run", spec.to_str().expect("utf8")]),
        0,
        "run succeeds from sibling values"
    );

    std::env::remove_var("API_BASE");
    server_thread.join().ok();
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
