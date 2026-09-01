//! `flowproof doctor --sap` / `--fiori`: read whatever `flowproof config`
//! seeded into the environment (the same way `record`/`run` read it) and
//! report what can actually be reached, before anyone writes a flow against
//! it. Design and reasoning: plans/002-sap-fiori-doctor.md.

use crate::{EXIT_FAIL, EXIT_PASS};

/// `doctor --sap`: a read-only look at whatever SAP GUI session already
/// exists. Never authenticates — SAP already rejects a bad credential on
/// its own, and repeatedly submitting a stale one from here risks locking
/// the account, a cost this check has no business paying just to answer
/// "is anything there" (plans/002-sap-fiori-doctor.md, "The SAP check").
pub fn cmd_doctor_sap() -> Result<u8, String> {
    #[cfg(not(windows))]
    {
        // The identical refusal `driver_for` already gives a real `app: sap`
        // record/run off Windows - a doctor error that looked any different
        // would be its own small inconsistency to debug.
        Err("app 'sap' needs SAP GUI Scripting (COM), which exists only on Windows".to_string())
    }
    #[cfg(windows)]
    {
        crate::config::seed_env();
        let connection = std::env::var("SAP_CONNECTION").unwrap_or_default();
        if connection.is_empty() {
            println!("SAP_CONNECTION is not set; observing attach-only (any open session).");
        } else {
            println!("SAP_CONNECTION={connection}");
        }

        let observation =
            flowproof_adapters::sap_com::observe(&connection).map_err(|e| e.to_string())?;
        if !observation.attached {
            println!("SAP Logon is not reachable: no 'SAPGUI' entry in the Running Object Table.");
            println!("Start SAP Logon (or SAP GUI) and try again.");
            return Ok(EXIT_FAIL);
        }
        println!("attached to SAP GUI scripting.");

        if let Some(found) = observation.connection_found {
            println!(
                "connection '{connection}': {}",
                if found { "open" } else { "not currently open" }
            );
        }
        if observation.sessions.is_empty() {
            println!("no session found on this connection.");
        }
        for session in &observation.sessions {
            match session {
                flowproof_adapters::sap_com::SapSessionState::LoggedIn(user) => {
                    println!("session logged in as {user}");
                }
                flowproof_adapters::sap_com::SapSessionState::AtLoginScreen => {
                    println!(
                        "session is on the login screen, not yet authenticated (doctor never \
                         submits a credential)."
                    );
                }
            }
        }
        Ok(EXIT_PASS)
    }
}

/// `doctor --fiori`: Stage 1 (unauthenticated reachability, always) then
/// Stage 2 (a real login attempt, only when `FIORI_USER`/`FIORI_PASSWORD`
/// both resolve). Stage 2 submits a real credential to a live system - see
/// the plan's "The Fiori check" for why that is a deliberate, accepted
/// trade rather than an oversight, and never wire this into CI.
pub fn cmd_doctor_fiori(timeout_secs: u64) -> Result<u8, String> {
    crate::config::seed_env();

    let base_url = non_empty_env("FIORI_BASE_URL").ok_or_else(|| {
        "FIORI_BASE_URL is not set; run `flowproof config fiori` or export it".to_string()
    })?;
    let client = non_empty_env("FIORI_CLIENT");
    let language = non_empty_env("FIORI_LANGUAGE");
    if client.is_none() {
        println!(
            "FIORI_CLIENT is not set; the launchpad's bootstrap path 404s without it on most \
             systems."
        );
    }
    if language.is_none() {
        println!(
            "FIORI_LANGUAGE is not set; the launchpad's bootstrap path 404s without it on most \
             systems."
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

    println!("GET {url}");
    let reachability = flowproof_adapters::fiori_reachability(&url);
    match reachability.status {
        Some(status) => println!(
            "reachable: HTTP {status} in {:.2}s",
            reachability.elapsed.as_secs_f64()
        ),
        None => println!(
            "NOT reachable: {}",
            reachability.error.as_deref().unwrap_or("unknown error")
        ),
    }
    if let Some(final_url) = &reachability.final_url {
        // ureq's `Uri` normalizes a bare authority to carry an explicit `/`
        // path, which is not a redirect - compare with that difference
        // ignored so a same-origin request doesn't print a false one.
        if final_url.trim_end_matches('/') != url.trim_end_matches('/') {
            println!("redirected to: {final_url}");
        }
    }

    if reachability.status.is_none() {
        // A browser navigating the same URL would fail the same way, only
        // slower - nothing left for Stage 2 to add.
        println!();
        println!("skipping the login check: the launchpad did not answer at all.");
        return Ok(EXIT_FAIL);
    }

    let user = non_empty_env("FIORI_USER");
    let password = non_empty_env("FIORI_PASSWORD");
    let (Some(user), Some(_password)) = (user, password) else {
        println!();
        println!(
            "FIORI_USER/FIORI_PASSWORD are not both configured; skipping the login check (run \
             `flowproof config fiori` to set them)."
        );
        return Ok(EXIT_PASS);
    };

    println!();
    println!(
        "attempting a real login as {user} - this submits a real credential to a live system. \
         Never run --fiori from CI or on a loop: a wrong password is a real failed logon."
    );
    login_attempt(&url, timeout_secs)
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
fn login_attempt(url: &str, timeout_secs: u64) -> Result<u8, String> {
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
            println!("login succeeded: the shell loaded (\"Home\" is showing).");
            Ok(EXIT_PASS)
        }
        Err(e) => {
            println!("login did NOT succeed: {e}");
            Ok(EXIT_FAIL)
        }
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// Render one Rust string as a single YAML scalar, so a URL or name with
/// YAML-significant characters (a literal `: `, a leading `#`) cannot be
/// misparsed when spliced into the hand-built flow spec above.
fn yaml_scalar(s: &str) -> Result<String, String> {
    let doc = serde_yaml::to_string(s).map_err(|e| format!("encoding YAML scalar: {e}"))?;
    Ok(doc.trim_end().to_string())
}
