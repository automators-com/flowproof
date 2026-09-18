//! `flowproof doctor --ai` end to end through `run_cli` with process-global
//! model env isolated by a lock.
#![cfg(unix)]

use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

// Keep both inherited credentials and the developer's saved config out of
// these tests. Restore the process environment even when an assertion panics.
struct IsolatedAiEnv {
    previous: Vec<(&'static str, Option<std::ffi::OsString>)>,
    config_home: std::path::PathBuf,
}

impl IsolatedAiEnv {
    fn new() -> Self {
        let names = [
            "HOME",
            "XDG_CONFIG_HOME",
            "FLOWPROOF_AI_PROVIDER",
            "FLOWPROOF_AI_API_KEY",
            "FLOWPROOF_AI_MODEL",
            "FLOWPROOF_AI_BASE_URL",
            "FLOWPROOF_AI_WORKSPACE_ID",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
        ];
        let config_home = std::env::temp_dir().join(format!(
            "flowproof-doctor-ai-isolated-{}",
            std::process::id()
        ));
        std::fs::create_dir(&config_home).expect("fresh config home");
        let previous = names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect();
        for name in names {
            std::env::remove_var(name);
        }
        // dirs::config_dir uses HOME on macOS and XDG_CONFIG_HOME on Linux.
        std::env::set_var("HOME", &config_home);
        std::env::set_var("XDG_CONFIG_HOME", &config_home);
        Self {
            previous,
            config_home,
        }
    }
}

impl Drop for IsolatedAiEnv {
    fn drop(&mut self) {
        for (name, value) in &self.previous {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        std::fs::remove_dir_all(&self.config_home).ok();
    }
}

#[test]
fn doctor_ai_without_a_key_fails_without_a_model_call() {
    let _guard = ENV.lock().expect("env lock");
    let _env = IsolatedAiEnv::new();

    assert_eq!(
        flowproof_cli::run_cli(["doctor", "--ai"]),
        1,
        "default anthropic provider needs a key before doctor can call it"
    );
}

#[test]
fn doctor_ai_json_keeps_the_same_verdict_as_human_output() {
    let _guard = ENV.lock().expect("env lock");
    let _env = IsolatedAiEnv::new();

    assert_eq!(
        flowproof_cli::run_cli(["doctor", "--ai", "--json"]),
        1,
        "--json must not change the verdict: a missing key still fails without a model call"
    );
}

#[test]
fn doctor_ai_openai_can_validate_against_a_local_compatible_endpoint() {
    let _guard = ENV.lock().expect("env lock");
    let _env = IsolatedAiEnv::new();

    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let handle = std::thread::spawn(move || {
        let request = server.recv().expect("doctor sends one request");
        assert_eq!(request.url(), "/chat/completions");
        let response =
            tiny_http::Response::from_string(r#"{"choices":[{"message":{"content":"ok"}}]}"#)
                .with_status_code(200)
                .with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .expect("header"),
                );
        request.respond(response).expect("responds");
    });

    std::env::set_var("FLOWPROOF_AI_PROVIDER", "openai");
    std::env::set_var("FLOWPROOF_AI_API_KEY", "sk-test");
    std::env::set_var("FLOWPROOF_AI_MODEL", "gpt-5");
    std::env::set_var("FLOWPROOF_AI_BASE_URL", base);

    assert_eq!(flowproof_cli::run_cli(["doctor", "--ai"]), 0);

    handle.join().expect("server thread joins");
}

#[test]
fn doctor_ai_json_passes_against_a_local_compatible_endpoint() {
    let _guard = ENV.lock().expect("env lock");
    let _env = IsolatedAiEnv::new();

    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let handle = std::thread::spawn(move || {
        let request = server.recv().expect("doctor sends one request");
        assert_eq!(request.url(), "/chat/completions");
        let response =
            tiny_http::Response::from_string(r#"{"choices":[{"message":{"content":"ok"}}]}"#)
                .with_status_code(200)
                .with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .expect("header"),
                );
        request.respond(response).expect("responds");
    });

    std::env::set_var("FLOWPROOF_AI_PROVIDER", "openai");
    std::env::set_var("FLOWPROOF_AI_API_KEY", "sk-test");
    std::env::set_var("FLOWPROOF_AI_MODEL", "gpt-5");
    std::env::set_var("FLOWPROOF_AI_BASE_URL", base);

    assert_eq!(flowproof_cli::run_cli(["doctor", "--ai", "--json"]), 0);

    handle.join().expect("server thread joins");
}

#[test]
fn doctor_ai_fails_when_the_connectivity_reply_is_not_ok() {
    let _guard = ENV.lock().expect("env lock");
    let _env = IsolatedAiEnv::new();

    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let handle = std::thread::spawn(move || {
        let request = server.recv().expect("doctor sends one request");
        assert_eq!(request.url(), "/chat/completions");
        let response =
            tiny_http::Response::from_string(r#"{"choices":[{"message":{"content":"not ok"}}]}"#)
                .with_status_code(200)
                .with_header(
                    tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                        .expect("header"),
                );
        request.respond(response).expect("responds");
    });

    std::env::set_var("FLOWPROOF_AI_PROVIDER", "openai");
    std::env::set_var("FLOWPROOF_AI_API_KEY", "sk-test");
    std::env::set_var("FLOWPROOF_AI_MODEL", "gpt-5");
    std::env::set_var("FLOWPROOF_AI_BASE_URL", base);

    assert_eq!(flowproof_cli::run_cli(["doctor", "--ai"]), 1);

    handle.join().expect("server thread joins");
}
