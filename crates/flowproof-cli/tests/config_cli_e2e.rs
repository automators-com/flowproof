//! `flowproof config sap`/`fiori`/`show`/`path` end to end through
//! `run_cli`, per plans/001-credential-config.md's "Command surface".
//!
//! `HOME` is process-global, so — like `sap_com.rs`'s own credential tests —
//! everything here runs under one lock rather than side by side.
#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("flowproof-config-cli-{name}"));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).expect("home dir");
    dir
}

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
fn config_sap_via_flags_writes_and_a_second_call_merges() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("sap-merge");

    with_fake_home(&home, || {
        let code = flowproof_cli::run_cli([
            "config",
            "sap",
            "--user",
            "obeva",
            "--client",
            "100",
            "--connection",
            "TS3",
        ]);
        assert_eq!(code, 0, "first write succeeds");

        let code = flowproof_cli::run_cli(["config", "sap", "--password", "secret"]);
        assert_eq!(code, 0, "second write succeeds");

        let config = flowproof_cli::config::load().expect("loads");
        let sap = config.sap.expect("sap profile written");
        assert_eq!(sap.user.as_deref(), Some("obeva"), "untouched by 2nd call");
        assert_eq!(sap.client.as_deref(), Some("100"), "untouched by 2nd call");
        assert_eq!(
            sap.connection.as_deref(),
            Some("TS3"),
            "untouched by 2nd call"
        );
        assert_eq!(sap.password.as_deref(), Some("secret"), "set by 2nd call");
    });
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_sap_and_fiori_are_independent_profiles() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("independence");

    with_fake_home(&home, || {
        assert_eq!(
            flowproof_cli::run_cli(["config", "sap", "--user", "gui-user"]),
            0
        );
        assert_eq!(
            flowproof_cli::run_cli([
                "config",
                "fiori",
                "--user",
                "fiori-user",
                "--base-url",
                "https://launchpad.test/",
            ]),
            0
        );

        let config = flowproof_cli::config::load().expect("loads");
        assert_eq!(config.sap.expect("sap").user.as_deref(), Some("gui-user"));
        let fiori = config.fiori.expect("fiori");
        assert_eq!(fiori.user.as_deref(), Some("fiori-user"));
        assert_eq!(fiori.base_url.as_deref(), Some("https://launchpad.test/"));
    });
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_ai_via_flags_writes_merges_and_clears() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("ai-merge-clear");

    with_fake_home(&home, || {
        let code = flowproof_cli::run_cli([
            "config",
            "ai",
            "--provider",
            "anthropic",
            "--api-key",
            "sk-ant",
            "--model",
            "claude-sonnet-5",
        ]);
        assert_eq!(code, 0, "first write succeeds");

        let code = flowproof_cli::run_cli(["config", "ai", "--provider", "openai"]);
        assert_eq!(code, 0, "second write succeeds");

        let config = flowproof_cli::config::load().expect("loads");
        let ai = config.ai.expect("ai profile written");
        assert_eq!(
            ai.provider,
            Some(flowproof_cli::config::AiProvider::Openai),
            "provider changed by second call"
        );
        assert_eq!(ai.api_key.as_deref(), Some("sk-ant"), "key preserved");
        assert_eq!(
            ai.model.as_deref(),
            Some("claude-sonnet-5"),
            "model preserved"
        );

        let code = flowproof_cli::run_cli(["config", "ai", "--clear-api-key"]);
        assert_eq!(code, 0, "clear key succeeds");
        let code = flowproof_cli::run_cli(["config", "ai", "--clear-model"]);
        assert_eq!(code, 0, "clear model succeeds");

        let config = flowproof_cli::config::load().expect("loads after clear");
        let ai = config.ai.expect("ai profile still present");
        assert_eq!(ai.provider, Some(flowproof_cli::config::AiProvider::Openai));
        assert_eq!(ai.api_key, None);
        assert_eq!(ai.model, None);
    });
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_ai_rejects_a_misspelled_provider_without_writing() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("ai-provider-typo");

    with_fake_home(&home, || {
        let code = flowproof_cli::run_cli(["config", "ai", "--provider", "antropic"]);
        assert_eq!(code, 2, "clap rejects the misspelled provider");
        let config = flowproof_cli::config::load().expect("missing stays empty");
        assert_eq!(config.ai, None, "invalid provider never writes config");
    });
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_show_and_path_succeed_on_an_empty_config() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("show-empty");

    with_fake_home(&home, || {
        assert_eq!(flowproof_cli::run_cli(["config", "path"]), 0);
        assert_eq!(
            flowproof_cli::run_cli(["config", "show"]),
            0,
            "no file yet is not an error"
        );
    });
    std::fs::remove_dir_all(&home).ok();
}

#[test]
fn config_sap_without_flags_or_a_tty_fails_fast_naming_the_flags() {
    let _guard = ENV.lock().expect("env lock");
    let home = temp_home("no-tty");

    with_fake_home(&home, || {
        // The test harness's own stdin is never a TTY, so no flags means
        // the fast-fail path below — never a hang waiting on input that
        // will never arrive.
        let code = flowproof_cli::run_cli(["config", "sap"]);
        assert_eq!(code, 2, "a clear error, not a hang");
    });
    std::fs::remove_dir_all(&home).ok();
}
