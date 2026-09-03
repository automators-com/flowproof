//! `flowproof doctor --ai` end to end through `run_cli` with process-global
//! model env isolated by a lock.
#![cfg(unix)]

use std::sync::Mutex;

static ENV: Mutex<()> = Mutex::new(());

fn clear_ai_env() {
    for name in [
        "FLOWPROOF_AI_PROVIDER",
        "FLOWPROOF_AI_API_KEY",
        "FLOWPROOF_AI_MODEL",
        "FLOWPROOF_AI_BASE_URL",
        "ANTHROPIC_API_KEY",
        "OPENAI_API_KEY",
    ] {
        std::env::remove_var(name);
    }
}

#[test]
fn doctor_ai_without_a_key_fails_without_a_model_call() {
    let _guard = ENV.lock().expect("env lock");
    clear_ai_env();

    assert_eq!(
        flowproof_cli::run_cli(["doctor", "--ai"]),
        1,
        "default anthropic provider needs a key before doctor can call it"
    );

    clear_ai_env();
}

#[test]
fn doctor_ai_openai_can_validate_against_a_local_compatible_endpoint() {
    let _guard = ENV.lock().expect("env lock");
    clear_ai_env();

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
    clear_ai_env();
}

#[test]
fn doctor_ai_fails_when_the_connectivity_reply_is_not_ok() {
    let _guard = ENV.lock().expect("env lock");
    clear_ai_env();

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
    clear_ai_env();
}
