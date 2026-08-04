//! What the authoring client puts on the wire, and what it makes of what
//! comes back. Driven against a real local HTTP server: the request shape and
//! the error path are the things under test, and a mock client would prove
//! nothing about either.

use flowproof_agent::{BackendConfig, BackendKind, HttpModelClient, ModelClient};

/// Serve one request: capture its body, answer with `(status, body)`.
fn serve_once(status: u16, body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("server binds");
    let base = format!("http://{}", server.server_addr());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok(mut request) = server.recv() {
            let mut seen = String::new();
            request.as_reader().read_to_string(&mut seen).ok();
            tx.send(seen).ok();
            let response = tiny_http::Response::from_string(body).with_status_code(status);
            request.respond(response).ok();
        }
    });
    (base, rx)
}

fn anthropic_client(base: &str) -> HttpModelClient {
    HttpModelClient::new(BackendConfig {
        kind: BackendKind::Anthropic,
        base_url: Some(base.to_string()),
        model: Some("claude-opus-5".into()),
        api_key: Some("test-key-do-not-use".into()),
    })
}

/// The defect that made LLM authoring unusable on the DEFAULT model: the API
/// deprecated `temperature` on current models, and flowproof sent it
/// unconditionally, so every Sonnet 5 / Opus 5 request was rejected 400.
#[test]
fn the_anthropic_request_carries_no_temperature() {
    let (base, rx) = serve_once(200, r#"{"content":[{"type":"text","text":"ok"}]}"#);
    let reply = anthropic_client(&base)
        .complete("system", "user")
        .expect("call succeeds");
    assert_eq!(reply, "ok");

    let sent = rx.recv().expect("request captured");
    assert!(
        !sent.contains("temperature"),
        "the Anthropic request must not carry a deprecated `temperature`: {sent}"
    );
    assert!(sent.contains("claude-opus-5"), "model is sent: {sent}");
}

/// A reasoning model puts a `thinking` block ahead of its answer. Indexing
/// content[0] blindly read a block with no `text` and called the response
/// shape unexpected — with the answer sitting one element further on.
#[test]
fn a_thinking_block_before_the_answer_is_skipped() {
    let (base, _rx) = serve_once(
        200,
        r#"{"content":[{"type":"thinking","thinking":"hmm"},{"type":"text","text":"the answer"}]}"#,
    );
    let reply = anthropic_client(&base)
        .complete("system", "user")
        .expect("call succeeds");
    assert_eq!(reply, "the answer", "the first TEXT block is the answer");
}

/// Sonnet 5 may use the entire output allowance for reasoning and return only
/// a signed thinking block. That is token exhaustion, not an unknown response
/// shape, and the opaque signature must not flood the terminal diagnostic.
#[test]
fn a_thinking_only_max_tokens_response_names_token_exhaustion() {
    let (base, rx) = serve_once(
        200,
        r#"{"content":[{"type":"thinking","thinking":"","signature":"opaque-secret-signature"}],"stop_reason":"max_tokens"}"#,
    );
    let err = anthropic_client(&base)
        .complete("system", "user")
        .expect_err("a response without a text answer must fail");
    let text = err.to_string();
    assert!(
        text.contains("exhausted its 8192-token output budget"),
        "the actual failure and configured budget are named: {text}"
    );
    assert!(
        !text.contains("opaque-secret-signature"),
        "the thinking signature must not be dumped: {text}"
    );

    let sent: serde_json::Value =
        serde_json::from_str(&rx.recv().expect("request captured")).expect("request is JSON");
    assert_eq!(
        sent["max_tokens"], 8192,
        "Anthropic gets enough headroom to reason and still answer"
    );
}

/// `http status: 400` with the body discarded is not a diagnostic. The
/// sentence that solves it is in the body.
#[test]
fn a_provider_error_quotes_the_providers_own_explanation() {
    let (base, _rx) = serve_once(
        400,
        r#"{"type":"error","error":{"message":"`temperature` is deprecated for this model."}}"#,
    );
    let err = anthropic_client(&base)
        .complete("system", "user")
        .expect_err("a 400 must fail");
    let text = err.to_string();
    assert!(text.contains("400"), "the status survives: {text}");
    assert!(
        text.contains("`temperature` is deprecated for this model."),
        "the provider's explanation must reach the user: {text}"
    );
}

/// This text is what someone pastes into a bug report. A gateway that echoes
/// the request back must not turn that into a key disclosure.
#[test]
fn an_echoed_key_is_redacted_from_the_error() {
    let (base, _rx) = serve_once(401, r#"{"error":"bad key: test-key-do-not-use"}"#);
    let err = anthropic_client(&base)
        .complete("system", "user")
        .expect_err("a 401 must fail");
    let text = err.to_string();
    assert!(
        !text.contains("test-key-do-not-use"),
        "the key must not survive into the error: {text}"
    );
    assert!(
        text.contains("<redacted>"),
        "and the redaction is visible: {text}"
    );
}
