//! `flowproof doctor --sap` / `--fiori` / `--ai`: read whatever `flowproof config`
//! seeded into the environment (the same way `record`/`run` read it) and
//! report what can actually be reached, before anyone writes a flow against
//! it. Design and reasoning: plans/002-sap-fiori-doctor.md.
//!
//! Every mode is read-only and prints no secret material. With `--json` the
//! whole report goes out as one JSON object on stdout (exit code carries the
//! verdict either way); without it, the human-readable lines below are what
//! you see in a terminal.

use flowproof_agent::ModelClient;
use serde::Serialize;

use crate::{EXIT_ERROR, EXIT_FAIL, EXIT_PASS};

/// One diagnosis line. `status` mirrors the CLI's own exit-code spirit
/// (`ok`/`warn`/`fail`), `name` labels the thing being checked, and
/// `message` carries the finding. No secret material ever lands here.
#[derive(Serialize, Debug)]
pub struct DoctorCheck {
    pub status: String,
    pub name: String,
    pub message: String,
}

/// The whole `doctor` result, serialised on stdout when `--json` is set.
/// `pass` reflects the exit code the command is about to return.
#[derive(Serialize, Debug)]
pub struct DoctorReport {
    pub area: String,
    pub pass: bool,
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    pub fn new(area: &str) -> Self {
        DoctorReport {
            area: area.to_string(),
            pass: false,
            checks: Vec::new(),
        }
    }

    /// Record a structured check whose JSON message is the bare finding; the
    /// caller prints its own human-spelling line (the `provider:`/`endpoint:`
    /// prefixed prints), so the two formats can read naturally.
    fn check(&mut self, status: &str, name: &str, value: impl Into<String>) {
        self.checks.push(DoctorCheck {
            status: status.to_string(),
            name: name.to_string(),
            message: value.into(),
        });
    }

    /// Record a check and, when `!json`, print `message` as the terminal line,
    /// so the human text and the structured text never drift.
    pub fn note(&mut self, json: bool, status: &str, name: &str, message: impl Into<String>) {
        let message = message.into();
        self.checks.push(DoctorCheck {
            status: status.to_string(),
            name: name.to_string(),
            message: message.clone(),
        });
        if !json {
            println!("{message}");
        }
    }

    pub fn emit(&self, json: bool) -> Result<(), String> {
        if json {
            println!("{}", serde_json::to_string(self).map_err(|e| e.to_string())?);
        }
        Ok(())
    }
}

/// `doctor --sap`: a read-only look at whatever SAP GUI session already
/// exists. Never authenticates — SAP already rejects a bad credential on
/// its own, and repeatedly submitting a stale one from here risks locking
/// the account, a cost this check has no business paying just to answer
/// "is anything there" (plans/002-sap-fiori-doctor.md, "The SAP check").
pub fn cmd_doctor_sap(json: bool) -> Result<u8, String> {
    #[cfg(not(windows))]
    {
        // The identical refusal `driver_for` already gives a real `app: sap`
        // record/run off Windows - a doctor error that looked any different
        // would be its own small inconsistency to debug.
        let message = "app 'sap' needs SAP GUI Scripting (COM), which exists only on Windows";
        if json {
            let mut report = DoctorReport::new("sap");
            report.note(json, "fail", "platform", message);
            report.emit(json)?;
            return Ok(EXIT_ERROR);
        }
        Err(message.to_string())
    }
    #[cfg(windows)]
    {
        crate::config::seed_env();
        let mut report = DoctorReport::new("sap");
        let connection = std::env::var("SAP_CONNECTION").unwrap_or_default();
        if connection.is_empty() {
            report.note(
                json,
                "warn",
                "connection",
                "SAP_CONNECTION is not set; observing attach-only (any open session).",
            );
        } else {
            report.note(json, "ok", "connection", format!("SAP_CONNECTION={connection}"));
        }

        let observation =
            flowproof_adapters::sap_com::observe(&connection).map_err(|e| e.to_string())?;
        if !observation.attached {
            report.note(
                json,
                "fail",
                "sap gui",
                "SAP Logon is not reachable: no 'SAPGUI' entry in the Running Object Table.",
            );
            report.note(json, "fail", "sap gui", "Start SAP Logon (or SAP GUI) and try again.");
            report.emit(json)?;
            return Ok(EXIT_FAIL);
        }
        report.note(json, "ok", "sap gui", "attached to SAP GUI scripting.");

        if let Some(found) = observation.connection_found {
            report.note(
                json,
                if found { "ok" } else { "warn" },
                "connection",
                format!(
                    "connection '{connection}': {}",
                    if found { "open" } else { "not currently open" }
                ),
            );
        }
        if observation.sessions.is_empty() {
            report.note(json, "warn", "session", "no session found on this connection.");
        }
        for session in &observation.sessions {
            match session {
                flowproof_adapters::sap_com::SapSessionState::LoggedIn(user) => {
                    report.note(json, "ok", "session", format!("session logged in as {user}"));
                }
                flowproof_adapters::sap_com::SapSessionState::AtLoginScreen => {
                    report.note(
                        json,
                        "warn",
                        "session",
                        "session is on the login screen, not yet authenticated (doctor never \
                         submits a credential).",
                    );
                }
            }
        }
        report.pass = true;
        report.emit(json)?;
        Ok(EXIT_PASS)
    }
}

/// `doctor --fiori`: Stage 1 (unauthenticated reachability, always) then
/// Stage 2 (a real login attempt, only when `FIORI_USER`/`FIORI_PASSWORD`
/// both resolve). Stage 2 submits a real credential to a live system - see
/// the plan's "The Fiori check" for why that is a deliberate, accepted
/// trade rather than an oversight, and never wire this into CI.
pub fn cmd_doctor_fiori(timeout_secs: u64, json: bool) -> Result<u8, String> {
    crate::config::seed_env();
    let mut report = DoctorReport::new("fiori");

    let base_url = match non_empty_env("FIORI_BASE_URL") {
        Some(base_url) => base_url,
        None => {
            let message = "FIORI_BASE_URL is not set; run `flowproof config fiori` or export it";
            if json {
                report.note(json, "fail", "base url", message);
                report.emit(json)?;
                return Ok(EXIT_ERROR);
            }
            return Err(message.to_string());
        }
    };
    let client = non_empty_env("FIORI_CLIENT");
    let language = non_empty_env("FIORI_LANGUAGE");
    if client.is_none() {
        report.note(
            json,
            "warn",
            "client",
            "FIORI_CLIENT is not set; the launchpad's bootstrap path 404s without it on most \
             systems.",
        );
    }
    if language.is_none() {
        report.note(
            json,
            "warn",
            "language",
            "FIORI_LANGUAGE is not set; the launchpad's bootstrap path 404s without it on most \
             systems.",
        );
    }

    let mut query = Vec::new();
    if let Some(c) = &client {
        query.push(format!("sap-client={c}"));
    }
    if let Some(l) = &language {
        query.push(format!("sap-language={l}"));
    }
    let mut url = base_url;
    if !query.is_empty() {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str(&query.join("&"));
    }

    report.note(json, "ok", "url", format!("GET {url}"));
    let reachability = flowproof_adapters::fiori_reachability(&url);
    match reachability.status {
        Some(status) => report.note(
            json,
            "ok",
            "reachability",
            format!(
                "reachable: HTTP {status} in {:.2}s",
                reachability.elapsed.as_secs_f64()
            ),
        ),
        None => report.note(
            json,
            "fail",
            "reachability",
            format!(
                "NOT reachable: {}",
                reachability.error.as_deref().unwrap_or("unknown error")
            ),
        ),
    }
    if let Some(final_url) = &reachability.final_url {
        // ureq's `Uri` normalizes a bare authority to carry an explicit `/`
        // path, which is not a redirect - compare with that difference
        // ignored so a same-origin request doesn't print a false one.
        if final_url.trim_end_matches('/') != url.trim_end_matches('/') {
            report.note(json, "warn", "redirect", format!("redirected to: {final_url}"));
        }
    }

    if reachability.status.is_none() {
        // A browser navigating the same URL would fail the same way, only
        // slower - nothing left for Stage 2 to add.
        if !json {
            println!();
        }
        report.note(
            json,
            "fail",
            "login",
            "skipping the login check: the launchpad did not answer at all.",
        );
        report.emit(json)?;
        return Ok(EXIT_FAIL);
    }

    let user = non_empty_env("FIORI_USER");
    let password = non_empty_env("FIORI_PASSWORD");
    let (Some(user), Some(_password)) = (user, password) else {
        if !json {
            println!();
        }
        report.note(
            json,
            "warn",
            "login",
            "FIORI_USER/FIORI_PASSWORD are not both configured; skipping the login check (run \
             `flowproof config fiori` to set them).",
        );
        report.emit(json)?;
        return Ok(EXIT_PASS);
    };

    if !json {
        println!();
    }
    report.note(
        json,
        "warn",
        "login",
        format!(
            "attempting a real login as {user} - this submits a real credential to a live system. \
             Never run --fiori from CI or on a loop: a wrong password is a real failed logon."
        ),
    );
    let verdict = login_attempt(&url, timeout_secs, &mut report, json)?;
    report.pass = verdict == EXIT_PASS;
    report.emit(json)?;
    Ok(verdict)
}

/// The actual Fiori login attempt: the same 5 steps
/// `examples/fiori/login-smoke.flow.yaml` already proved live against a
/// real launchpad (its own comment: "Home" resolves from the shell's own
/// tab label once authenticated, regardless of assigned tiles) — built as an
/// in-memory [`flowproof_agent::FlowSpec`] rather than a file on disk, the
/// same trick `cmd_doctor_agent`'s synthetic `Cassette` already uses for the
/// agent-boundary check. `Author::Rules`, not `Auto`: these steps are already
/// exact deterministic grammar, and sending them to a model risks a
/// paraphrase the rules parser then rejects (the reason `login-smoke.flow.yaml`
/// records with `--author rules` too).
fn login_attempt(
    url: &str,
    timeout_secs: u64,
    report: &mut DoctorReport,
    json: bool,
) -> Result<u8, String> {
    let flow_yaml = format!(
        "name: {}\n\
         app: web\n\
         url: {}\n\
         steps:\n\
         \x20 - Type ${{FIORI_USER}} into the \"User\" field\n\
         \x20 - Type ${{FIORI_PASSWORD}} into the \"Password\" field\n\
         \x20 - Press the \"Log On\" button\n\
         \x20 - Wait until page shows Home within {timeout_secs}s\n\
         \x20 - assert: page shows Home\n",
        yaml_scalar("flowproof doctor: Fiori login")?,
        yaml_scalar(&format!("{url}#Shell-home"))?,
    );
    let spec = flowproof_agent::FlowSpec::parse(&flow_yaml)
        .map_err(|e| format!("building the doctor login flow: {e}"))?;

    let mut driver = crate::driver_for("web")?;
    let temp_out = std::env::temp_dir().join(format!(
        "flowproof-doctor-fiori-{}.trace.jsonl",
        std::process::id()
    ));
    let result = flowproof_agent::record_with_author(
        &spec,
        &mut driver,
        &temp_out,
        flowproof_agent::Author::Rules,
    );
    std::fs::remove_file(&temp_out).ok();

    match result {
        Ok(_) => {
            report.note(
                json,
                "ok",
                "login",
                "login succeeded: the shell loaded (\"Home\" is showing).",
            );
            Ok(EXIT_PASS)
        }
        Err(e) => {
            report.note(json, "fail", "login", format!("login did NOT succeed: {e}"));
            Ok(EXIT_FAIL)
        }
    }
}

/// `doctor --ai`: validate the model authoring backend after `flowproof config`
/// has had a chance to seed it. Prints no secret material.
pub fn cmd_doctor_ai(json: bool) -> Result<u8, String> {
    crate::config::seed_env();
    let mut report = DoctorReport::new("ai");

    let config = match flowproof_agent::BackendConfig::from_env() {
        Ok(config) => config,
        Err(e) => {
            let message = e.to_string();
            if json {
                report.note(json, "fail", "config", message);
                report.emit(json)?;
                return Ok(EXIT_ERROR);
            }
            return Err(message);
        }
    };
    let provider_str = match config.kind {
        flowproof_agent::BackendKind::Anthropic => "anthropic",
        flowproof_agent::BackendKind::OpenAi => "openai",
        flowproof_agent::BackendKind::OpenAiCompatible => "openai-compatible",
    };
    if !json {
        println!("provider: {provider_str}");
    }
    report.check("ok", "provider", provider_str);
    match config.base_url.as_deref() {
        Some(base_url) => {
            if !json {
                println!("endpoint: {base_url}");
            }
            report.check("ok", "endpoint", base_url);
        }
        None => {
            if !json {
                println!("endpoint: built-in provider default");
            }
            report.check("ok", "endpoint", "built-in provider default");
        }
    }
    let key_configured = config
        .api_key
        .as_ref()
        .is_some_and(|v| !v.trim().is_empty());
    if !json {
        println!(
            "api key: {}",
            if key_configured { "configured" } else { "not configured" }
        );
    }
    report.check(
        if key_configured { "ok" } else { "fail" },
        "api key",
        if key_configured { "configured" } else { "not configured" },
    );

    if !config.is_usable() {
        if !json {
            println!();
        }
        report.note(
            json,
            "fail",
            "usable",
            "AI backend is not usable; run `flowproof config ai` or set \
             FLOWPROOF_AI_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY.",
        );
        report.emit(json)?;
        return Ok(EXIT_FAIL);
    }

    let mut client = flowproof_agent::HttpModelClient::new(config);
    let (backend, model) = client.identity();
    if !json {
        println!("model: {backend}/{model}");
    }
    report.check("ok", "model", format!("{backend}/{model}"));
    match client.complete(
        "You are a connectivity check. Reply with exactly: ok",
        "Reply with exactly: ok",
    ) {
        Ok(reply) if reply.trim() == "ok" => {
            report.note(json, "ok", "model call", "model call succeeded.");
            report.pass = true;
            report.emit(json)?;
            Ok(EXIT_PASS)
        }
        Ok(_) => {
            report.note(
                json,
                "fail",
                "model call",
                "model call failed: expected `ok` response.",
            );
            report.emit(json)?;
            Ok(EXIT_FAIL)
        }
        Err(e) => {
            report.note(json, "fail", "model call", format!("model call failed: {e}"));
            report.emit(json)?;
            Ok(EXIT_FAIL)
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_serialises_checks_and_pass_under_the_area() {
        let mut report = DoctorReport::new("ai");
        report.check("ok", "provider", "anthropic");
        report.note(true, "fail", "api key", "not configured");
        report.pass = false;

        let value: serde_json::Value =
            serde_json::from_str(&serde_json::to_string(&report).unwrap()).unwrap();
        assert_eq!(value["area"], "ai");
        assert_eq!(value["pass"], false);
        assert_eq!(value["checks"][0]["name"], "provider");
        assert_eq!(value["checks"][0]["status"], "ok");
        assert_eq!(value["checks"][0]["message"], "anthropic");
        assert_eq!(value["checks"][1]["name"], "api key");
        assert_eq!(value["checks"][1]["status"], "fail");
        assert_eq!(value["checks"][1]["message"], "not configured");
    }
}

/// Render one Rust string as a single YAML scalar, so a URL or name with
/// YAML-significant characters (a literal `: `, a leading `#`) cannot be
/// misparsed when spliced into the hand-built flow spec above.
fn yaml_scalar(s: &str) -> Result<String, String> {
    let doc = serde_yaml::to_string(s).map_err(|e| format!("encoding YAML scalar: {e}"))?;
    Ok(doc.trim_end().to_string())
}