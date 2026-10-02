//! `flowproof author-from-code` end to end against a fake model server: the
//! model picks files, the draft cites them, and `--json` hands each step's
//! source line to the caller. The server also proves what is never sent.

use std::process::Command;

fn reply(content: &str) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let payload = serde_json::json!({
        "choices": [{"message": {"role": "assistant", "content": content}}]
    });
    tiny_http::Response::from_string(payload.to_string()).with_header(
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
            .expect("header"),
    )
}

#[test]
fn drafts_steps_that_cite_the_code_they_came_from() {
    let repo = std::env::temp_dir().join(format!("fp-afc-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    std::fs::create_dir_all(repo.join("src")).expect("repo dir");
    std::fs::write(
        repo.join("src/Nav.tsx"),
        "export const Nav = () => (\n  <a href=\"/suppliers\">Suppliers</a>\n);\n",
    )
    .expect("source written");
    std::fs::write(repo.join(".env"), "API_TOKEN=do-not-send\n").expect("secret written");

    let server = tiny_http::Server::http("127.0.0.1:0").expect("fake server binds");
    let base_url = format!("http://{}", server.server_addr());
    let seen = std::thread::spawn(move || {
        let mut bodies = Vec::new();
        for answer in [
            "FILE: src/Nav.tsx",
            "STEP: Click \"Suppliers\" @ src/Nav.tsx:2\nASSERT: page shows Suppliers @ src/Nav.tsx:2",
        ] {
            let mut request = server.recv().expect("model request");
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).ok();
            bodies.push(body);
            request.respond(reply(answer)).ok();
        }
        bodies
    });

    let out = repo.join("draft.flow.yaml");
    let output = Command::new(env!("CARGO_BIN_EXE_flowproof"))
        .args(["author-from-code", repo.to_str().expect("utf-8 path")])
        .args([
            "--goal",
            "the suppliers page is open",
            "--name",
            "Open suppliers",
        ])
        .args(["--url", "https://portal.example.test", "--json", "--out"])
        .arg(&out)
        .env("HOME", &repo)
        .env("FLOWPROOF_AI_PROVIDER", "openai-compatible")
        .env("FLOWPROOF_AI_BASE_URL", &base_url)
        .env("FLOWPROOF_AI_MODEL", "fake")
        .env("FLOWPROOF_NO_UPDATE_CHECK", "1")
        .output()
        .expect("flowproof runs");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("json report");
    assert_eq!(report["steps"][0]["text"], "Click \"Suppliers\"");
    assert_eq!(report["steps"][0]["source"]["file"], "src/Nav.tsx");
    assert_eq!(report["steps"][0]["source"]["line"], 2);
    assert_eq!(report["steps"][1]["kind"], "assert");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("reading src/Nav.tsx"), "{stderr}");

    let yaml = std::fs::read_to_string(&out).expect("draft written");
    assert!(
        yaml.contains("url: \"https://portal.example.test\""),
        "{yaml}"
    );
    assert!(yaml.contains("- \"Click \\\"Suppliers\\\"\""), "{yaml}");

    let bodies = seen.join().expect("server thread");
    assert!(bodies
        .iter()
        .all(|b| !b.contains("do-not-send") && !b.contains(".env")));
    std::fs::remove_dir_all(&repo).ok();
}
