//! `flowproof` CLI logic, exposed as a library so both the Rust binary and
//! the Python entry point (via PyO3) share one implementation.

mod agent_flow;
mod capture;
pub mod config;
mod doctor;

use std::path::{Path, PathBuf};
use std::time::Instant;

use clap::{Parser, Subcommand, ValueEnum};
use flowproof_agent::FlowSpec;
use flowproof_driver::{AppDriver, UiaAppDriver};
use flowproof_replay::StepStatus;

/// Process exit codes: 0 = pass, 1 = test failure, 2 = error.
pub const EXIT_PASS: u8 = 0;
pub const EXIT_FAIL: u8 = 1;
pub const EXIT_ERROR: u8 = 2;

/// Scope the adapter's interactive browser lifetime to one CLI invocation.
/// The adapter reads this at launch and again when its driver is dropped.
fn with_keep_browser_open<T>(enabled: bool, action: impl FnOnce() -> T) -> T {
    if !enabled {
        return action();
    }
    const KEY: &str = "FLOWPROOF_KEEP_BROWSER_OPEN";
    let previous = std::env::var_os(KEY);
    std::env::set_var(KEY, "1");
    let result = action();
    match previous {
        Some(value) => std::env::set_var(KEY, value),
        None => std::env::remove_var(KEY),
    }
    result
}

/// Resolve whether this invocation should show the browser: an explicit
/// `--headed`/`--headless` flag wins outright, then an ambient
/// `FLOWPROOF_HEADED` someone already set in their shell, then the command's
/// own default (`record` watches by default; `run` stays headless — see
/// `with_headed_mode`).
fn resolve_headed(headed: bool, headless: bool, default_headed: bool) -> bool {
    if headed {
        true
    } else if headless {
        false
    } else {
        std::env::var_os("FLOWPROOF_HEADED").is_some() || default_headed
    }
}

/// Scope `FLOWPROOF_HEADED` — the same variable `flowproof-adapters` already
/// reads — to one CLI invocation, so the resolved headed/headless decision
/// reaches the adapter without it needing to know which command is running.
/// Unlike `with_keep_browser_open`, this must be able to force the variable
/// *off* as well as on: an explicit `--headless` has to win even when
/// `FLOWPROOF_HEADED` is already set in the caller's environment.
fn with_headed_mode<T>(headed: bool, action: impl FnOnce() -> T) -> T {
    const KEY: &str = "FLOWPROOF_HEADED";
    let previous = std::env::var_os(KEY);
    if headed {
        std::env::set_var(KEY, "1");
    } else {
        std::env::remove_var(KEY);
    }
    let result = action();
    match previous {
        Some(value) => std::env::set_var(KEY, value),
        None => std::env::remove_var(KEY),
    }
    result
}

#[derive(Parser)]
#[command(name = "flowproof", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Authoring backend selection for record/heal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum AuthorArg {
    /// Model-ground plain steps; visibly fall back to rules if no model exists.
    #[default]
    Auto,
    /// Deterministic rules only.
    Rules,
    /// Model for every plain UI step; explicit/structured forms stay deterministic.
    Llm,
}

impl From<AuthorArg> for flowproof_agent::Author {
    fn from(value: AuthorArg) -> Self {
        match value {
            AuthorArg::Auto => flowproof_agent::Author::Auto,
            AuthorArg::Rules => flowproof_agent::Author::Rules,
            AuthorArg::Llm => flowproof_agent::Author::Llm,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum RecordingDetailArg {
    /// Capture before and after every step.
    #[default]
    Full,
    /// Capture the initial state, every fifth step, and the final state.
    Low,
    /// Do not capture screenshots or create a video.
    Off,
}

impl From<RecordingDetailArg> for flowproof_driver::RecordingDetail {
    fn from(value: RecordingDetailArg) -> Self {
        match value {
            RecordingDetailArg::Full => flowproof_driver::RecordingDetail::Full,
            RecordingDetailArg::Low => flowproof_driver::RecordingDetail::Low,
            RecordingDetailArg::Off => flowproof_driver::RecordingDetail::Off,
        }
    }
}

fn recording_options(
    detail: RecordingDetailArg,
    video: bool,
    highlight_cursor: bool,
) -> flowproof_driver::RecordingOptions {
    flowproof_driver::RecordingOptions {
        detail: detail.into(),
        video,
        highlight_cursor,
    }
}

/// `flowproof config <action>` — see [`Command::Config`].
#[derive(Subcommand)]
enum ConfigAction {
    /// SAP GUI: user, password, client, language, and the SAP Logon
    /// connection name. Prompts interactively unless any flag is given.
    Sap {
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        client: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        connection: Option<String>,
    },
    /// Fiori: user, password, client, language, and the launchpad base
    /// URL — an identity independent of `sap`, not shared with it
    /// (plans/001-credential-config.md, "Two profiles, not one identity").
    Fiori {
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        client: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long = "base-url")]
        base_url: Option<String>,
    },
    /// AI authoring: provider and API key for model-assisted recording/doc
    /// authoring. Prompts interactively unless any flag is given; model is an
    /// advanced flag-only override.
    Ai {
        #[arg(long, value_enum)]
        provider: Option<config::AiProvider>,
        #[arg(long = "api-key")]
        api_key: Option<String>,
        #[arg(long)]
        model: Option<String>,
        #[arg(long = "clear-api-key")]
        clear_api_key: bool,
        #[arg(long = "clear-model")]
        clear_model: bool,
    },
    /// Print the config file's path and contents, password masked.
    Show,
    /// Print the resolved config file path alone (for scripting or opening
    /// in an editor).
    Path,
    /// Install the `flowproof-config` Agent Skill into the current
    /// directory, so a coding agent (Claude Code, or anything reading the
    /// shared `.agents/skills/` convention — Codex CLI, GitHub Copilot,
    /// Cursor, Gemini CLI) can walk the user through `sap`/`fiori`
    /// (plans/003-agent-config-skill.md). Defaults to writing both
    /// `.claude/skills/` and `.agents/skills/`.
    Skill {
        /// Write only `.claude/skills/flowproof-config/SKILL.md`.
        #[arg(long)]
        claude: bool,
        /// Write only `.agents/skills/flowproof-config/SKILL.md`.
        #[arg(long)]
        agents: bool,
        /// Also write to `<dir>/flowproof-config/SKILL.md` — for a harness
        /// that reads neither `.claude/skills/` nor `.agents/skills/`.
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Overwrite a target file that already exists with different
        /// content.
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum Command {
    /// Manage flowproof's own global, per-machine config file: SAP GUI and
    /// Fiori credentials, seeded into the environment as a fallback so
    /// `${VAR}` resolution picks them up (plans/001-credential-config.md).
    /// Writes only — nothing here is checked against a live system, since
    /// SAP already gives a specific error the first time a bad value is
    /// actually used at record/run time.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Record a flow from a YAML spec: perform it once against the live app
    /// and write a deterministic trace next to the spec.
    Record {
        /// Path to the YAML flow spec (e.g. calc.flow.yaml).
        spec: PathBuf,
        /// Output trace file (default: <spec>.trace.jsonl next to the spec).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Business-data values file to load before resolving ${VAR}s.
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Business-data override, repeatable as KEY=VALUE.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        var: Vec<String>,
        /// Emit the result as JSON on stdout (for programmatic callers).
        #[arg(long)]
        json: bool,
        /// Authoring backend: rules, llm, or auto (model intent with a visible no-model fallback).
        #[arg(long, value_enum, default_value_t)]
        author: AuthorArg,
        /// Incremental re-record: reuse every old step whose target still
        /// resolves; re-author only what drifted (needs an existing trace).
        #[arg(long)]
        reuse: bool,
        /// Replay the trace once, immediately, and refuse the recording if it
        /// cannot reproduce itself. PERFORMS THE FLOW A SECOND TIME against
        /// the live app, repeating whatever it does - orders, e-mails,
        /// payments - so it is opt-in rather than the default.
        #[arg(long)]
        verify: bool,
        /// Show Chromium and keep it open after the flow finishes. Close the
        /// flow window to let Flowproof exit.
        #[arg(long, conflicts_with = "json")]
        keep_open: bool,
        /// Show Chromium during recording. Already the default; this makes
        /// it explicit and overrides FLOWPROOF_HEADED if unset elsewhere.
        #[arg(long, conflicts_with = "headless")]
        headed: bool,
        /// Record headless (hide Chromium), overriding the default and any
        /// ambient FLOWPROOF_HEADED for this run.
        #[arg(long, conflicts_with_all = ["headed", "keep_open"])]
        headless: bool,
        /// Visual capture density: full, low, or off.
        #[arg(long, value_enum, default_value_t)]
        recording_detail: RecordingDetailArg,
        /// Assemble screenshot checkpoints into recording.gif (off by default).
        #[arg(long)]
        video: bool,
        /// Draw a visible cursor and prominent click halo into recordings.
        #[arg(long)]
        highlight_cursor: bool,
    },
    /// Deterministically replay a recorded flow (zero LLM calls). Point it
    /// at a DIRECTORY to run every *.flow.yaml under it as a suite with one
    /// merged junit.xml.
    Run {
        /// Path to the YAML flow spec the trace was recorded from, or a
        /// directory of specs.
        spec: PathBuf,
        /// Trace file (default: the trace `record` wrote for this spec).
        #[arg(short, long)]
        trace: Option<PathBuf>,
        /// Business-data values file to load before resolving ${VAR}s.
        #[arg(long)]
        vars: Option<PathBuf>,
        /// Business-data override, repeatable as KEY=VALUE.
        #[arg(long = "var", value_name = "KEY=VALUE")]
        var: Vec<String>,
        /// Emit the full report as JSON on stdout (for programmatic callers).
        #[arg(long)]
        json: bool,
        /// Re-run a FAILED flow up to this many extra times before calling
        /// it failed — absorbs infra flakiness (default 0, no retries).
        #[arg(long, default_value_t = 0)]
        retries: u8,
        /// Suite runs only: record any spec whose trace is missing, then
        /// replay it (default: traceless specs are reported as skipped).
        #[arg(long)]
        record_missing: bool,
        /// Authoring backend used only when --record-missing records a suite flow.
        #[arg(long, value_enum, default_value_t)]
        author: AuthorArg,
        /// Suite runs only: a missing trace is a hard error (pre-0.2.2
        /// behavior) instead of a skipped flow. For CI that must not let
        /// coverage silently shrink. Single-spec runs always error.
        #[arg(long, conflicts_with = "record_missing")]
        strict: bool,
        /// Show Chromium and keep it open after one flow finishes. Close the
        /// flow window to let Flowproof exit. Not available for suites.
        #[arg(long, conflicts_with = "json")]
        keep_open: bool,
        /// Show Chromium during this run, overriding the (headless) default
        /// and any ambient FLOWPROOF_HEADED.
        #[arg(long, conflicts_with = "headless")]
        headed: bool,
        /// Force headless (hide Chromium). Already the default; this makes
        /// it explicit and overrides an ambient FLOWPROOF_HEADED.
        #[arg(long, conflicts_with_all = ["headed", "keep_open"])]
        headless: bool,
        /// Visual capture density: full, low, or off.
        #[arg(long, value_enum, default_value_t)]
        recording_detail: RecordingDetailArg,
        /// Assemble screenshot checkpoints into recording.gif (off by default).
        #[arg(long)]
        video: bool,
        /// Draw a visible cursor and prominent click halo into recordings.
        #[arg(long)]
        highlight_cursor: bool,
    },
    /// Internal: the stdio MCP stand-in the agent spawns as its server
    /// command. Not run by hand - flowproof injects
    /// `FLOWPROOF_MCP_SERVER_<NAME>` into the agent's environment pointing
    /// here, and reads its context from `FLOWPROOF_MCP_DIR` /
    /// `FLOWPROOF_MCP_MODE`. Bridges JSON-RPC over stdin/stdout: record
    /// forwards to the real server and captures, replay serves the recorded
    /// lane with no external process.
    #[command(hide = true)]
    McpStdio {
        /// The server name, matching its `<name>.plan.json` in the run dir.
        #[arg(long)]
        server: String,
    },
    /// Capture what a tool sends on the wire: a byte-fidelity HTTP endpoint
    /// for debugging serialization. Point a tool-under-test's HTTP connection
    /// here instead of its real target; every request is printed and saved
    /// (method, path, all headers, raw body as text AND hexdump, plus any SAP
    /// `/BA1/`-style namespace field names), and answered 200 so the send
    /// side completes. Binds all interfaces, unauthenticated - run it
    /// deliberately, Ctrl-C to stop.
    Capture {
        /// TCP port to listen on (binds 0.0.0.0).
        #[arg(long)]
        port: u16,
        /// Directory for the per-request `req-NNN.txt` files (created if
        /// missing).
        #[arg(long, default_value = "./captured")]
        out: PathBuf,
        /// Emit a structured JSON-Lines report on stdout, one object per
        /// request, instead of the human view.
        #[arg(long)]
        json: bool,
    },
    /// Fold a suite's control-bearing flows into a control-coverage report:
    /// one entry per `control:` block with its pass/fail/capability-error
    /// verdict, and for `assert_no_secret_leak` flows the secrets_checked /
    /// corpus / excluded fields. READS the persisted run record `flowproof
    /// run` wrote (never re-replays); emitted as YAML (default) or JSON. With
    /// `--since <run-id>` it diffs the latest record against that one instead.
    Audit {
        /// Directory of specs to audit as a suite.
        dir: PathBuf,
        /// Emit JSON instead of YAML.
        #[arg(long)]
        json: bool,
        /// Audit a specific run record by id, instead of the latest.
        #[arg(long)]
        run: Option<String>,
        /// Diff the latest run record against this earlier run-id: emit added,
        /// removed, and verdict-changed controls.
        #[arg(long, conflicts_with = "run")]
        since: Option<String>,
    },
    /// Check that an agent's model traffic reaches flowproof, that SAP
    /// GUI / Fiori is reachable, or that AI authoring config can call a
    /// model, before you write a spec or spend a key or credential on it.
    /// Exactly one of `--agent`, `--sap`, `--fiori`, `--ai`.
    ///
    /// `--agent` starts the proxy, runs the command once, and reports what
    /// ARRIVED — it deliberately does not tell you the wiring is correct,
    /// because an agent with two clients can reach the proxy with one and
    /// the real provider with the other. `--sap`/`--fiori` read whatever
    /// `flowproof config` seeded into the environment and report what they
    /// can reach: `--sap` only ever observes (never submits a credential —
    /// SAP already rejects a bad one on its own); `--fiori` also attempts a
    /// real login when `FIORI_USER`/`FIORI_PASSWORD` are both configured, so
    /// it must never run in CI or on any repeated trigger — see
    /// plans/002-sap-fiori-doctor.md.
    Doctor {
        /// The command that starts the agent, exactly as `agent.command:`
        /// would spell it.
        #[arg(long, conflicts_with_all = ["sap", "fiori", "ai"])]
        agent: Option<String>,
        /// Check SAP GUI: is SAP Logon reachable, and what does the
        /// configured connection (`SAP_CONNECTION`) currently show?
        /// Observation only — never submits a credential.
        #[arg(long, conflicts_with_all = ["fiori", "ai"])]
        sap: bool,
        /// Check Fiori: is the launchpad URL reachable, and — only when
        /// FIORI_USER/FIORI_PASSWORD are configured — does a real login
        /// work? NOT CI-safe: a wrong password is a real failed logon on a
        /// live system, so this is a manual, occasional check only.
        #[arg(long, conflicts_with = "ai")]
        fiori: bool,
        /// Check AI authoring config: provider, key presence, endpoint, and a
        /// tiny model call. Prints no API key material.
        #[arg(long)]
        ai: bool,
        /// Seconds to let the agent run before giving up (`--agent`), or the
        /// Fiori login attempt before giving up (`--fiori`). Unused by
        /// `--sap`, which never waits on anything external.
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// The task handed to the agent through FLOWPROOF_PROMPT
        /// (`--agent` only).
        #[arg(long, default_value = "Say hello.")]
        prompt: String,
    },
    /// EXPERIMENTAL: draft a `.flow.yaml` from a requirement/test-case
    /// document (PDF export from a test-management tool). DRAFT only —
    /// still needs a live `record` pass to resolve any flagged steps.
    AuthorFromDoc {
        /// Path to the test-case document (PDF).
        doc: PathBuf,
        /// Target app id (e.g. sap, web, calc).
        #[arg(long)]
        app: String,
        /// Flow name, written into the draft's `name:` field.
        #[arg(long)]
        name: String,
        #[arg(short, long)]
        out: PathBuf,
    },
    /// Re-author the flow against the live app and propose a reviewable
    /// trace diff. Never modifies the trace unless --apply is passed.
    Heal {
        /// Path to the YAML flow spec.
        spec: PathBuf,
        /// Trace file (default: the trace `record` wrote for this spec).
        #[arg(short, long)]
        trace: Option<PathBuf>,
        /// Replace the trace with the proposal (explicit opt-in).
        #[arg(long)]
        apply: bool,
        /// Emit the heal report as JSON on stdout (for programmatic callers).
        #[arg(long)]
        json: bool,
        /// Authoring backend: rules, llm, or auto (model intent with a visible no-model fallback).
        #[arg(long, value_enum, default_value_t)]
        author: AuthorArg,
    },
}

/// Default trace path for a spec: `calc.flow.yaml` → `calc.trace.jsonl`.
pub fn default_trace_path(spec: &Path) -> PathBuf {
    let stem = spec
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = stem.strip_suffix(".flow.yaml").unwrap_or_else(|| {
        stem.strip_suffix(".yaml")
            .or_else(|| stem.strip_suffix(".yml"))
            .unwrap_or(&stem)
    });
    spec.with_file_name(format!("{base}.trace.jsonl"))
}

pub fn default_values_path(spec: &Path) -> PathBuf {
    let stem = spec
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let base = stem.strip_suffix(".flow.yaml").unwrap_or_else(|| {
        stem.strip_suffix(".yaml")
            .or_else(|| stem.strip_suffix(".yml"))
            .unwrap_or(&stem)
    });
    spec.with_file_name(format!("{base}.values.yaml"))
}

#[derive(Debug, Clone, Default)]
struct ValuesArgs {
    vars_file: Option<PathBuf>,
    vars: Vec<String>,
}

/// Pick the driver implementation for an app id — the browser driver for
/// `web`, SAP GUI Scripting for `sap`, the platform UIA driver otherwise.
pub fn driver_for(app: &str) -> Result<Box<dyn AppDriver>, String> {
    if app == "web" {
        let driver = flowproof_adapters::WebAppDriver::new().map_err(|e| e.to_string())?;
        return Ok(Box::new(driver));
    }
    if app == "sap" {
        #[cfg(windows)]
        {
            let driver = flowproof_adapters::SapAppDriver::new().map_err(|e| e.to_string())?;
            return Ok(Box::new(driver));
        }
        #[cfg(not(windows))]
        return Err("app 'sap' needs SAP GUI Scripting (COM), which exists only on Windows".into());
    }
    if app == "vision" {
        #[cfg(windows)]
        {
            let driver = flowproof_adapters::VisionAppDriver::new().map_err(|e| e.to_string())?;
            return Ok(Box::new(driver));
        }
        #[cfg(not(windows))]
        return Err(
            "app 'vision' captures and injects input natively, which exists only on Windows \
             today"
                .into(),
        );
    }
    if app == "windows" {
        #[cfg(windows)]
        {
            let driver = UiaAppDriver::new().map_err(|e| e.to_string())?;
            return Ok(Box::new(driver));
        }
        #[cfg(not(windows))]
        return Err(
            "app: {command, window_title} drives a Windows program through UI Automation, \
             which exists only on Windows"
                .into(),
        );
    }
    if app == "api" {
        // No UI: out-of-band assertions run without a driver. Works on
        // every platform.
        return Ok(Box::new(flowproof_driver::NoOpDriver::new()));
    }
    let driver = UiaAppDriver::new().map_err(|e| e.to_string())?;
    Ok(Box::new(driver))
}

/// JSON rendering of a record failure for `--json` callers: a clarification
/// becomes a structured payload the driving agent can act on; every other
/// error stays a plain error (`None`).
fn record_failure_json(err: &flowproof_agent::RecordError) -> Option<serde_json::Value> {
    match err {
        flowproof_agent::RecordError::NeedsClarification(c) => {
            Some(serde_json::json!({ "needs_clarification": c }))
        }
        _ => None,
    }
}

struct RecordOptions {
    out: Option<PathBuf>,
    values: ValuesArgs,
    json: bool,
    author: AuthorArg,
    reuse: bool,
    verify: bool,
    recording: flowproof_driver::RecordingOptions,
}

fn cmd_record(spec_path: &Path, options: RecordOptions) -> Result<u8, String> {
    let RecordOptions {
        out,
        values,
        json,
        author,
        reuse,
        verify,
        recording,
    } = options;
    let mut spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    // The suite's data (env_from) and env govern recording too — the
    // ${VAR}s a spec references must resolve the same here as in `run`.
    let manifest = apply_suite_context(spec_path)?;
    let _values = apply_values_context(spec_path, &values)?;
    // Suite-level browser defaults apply only when the spec has none —
    // recording bakes the result into the trace header.
    if spec.browser.is_none() {
        spec.browser = manifest.as_ref().and_then(|m| m.browser.clone());
    }
    // A `session: <name>` ref is dereferenced against the suite's identities
    // now, so record copies the identity's inline setup into the trace.
    dereference_identity(&mut spec, manifest.as_ref())?;
    if let Some(reason) = spec.skip_reason() {
        if json {
            println!("{}", serde_json::json!({ "skipped": reason }));
        } else {
            println!("[SKIP] {} ({reason})", spec.name);
        }
        return Ok(EXIT_PASS);
    }
    let out = out.unwrap_or_else(|| default_trace_path(spec_path));

    if author == AuthorArg::Auto
        && spec.has_plain_steps()
        && matches!(
            flowproof_agent::HttpModelClient::from_env_result(),
            Ok(None)
        )
    {
        eprintln!(
            "WARNING: no authoring model is configured; plain steps will try deterministic grammar fallback"
        );
    }

    // An agent flow does not use the record/replay driver at all: its
    // trace is a cassette recorded at the model boundary.
    if spec.app.id() == "agent" {
        // The containment tier prints on EVERY agent run, on every platform,
        // pass or fail - computed before the run so it shows even when
        // recording errors out.
        let predicted = agent_flow::containment(&spec);
        if !json {
            println!("{}", predicted.report_line());
        }
        let (tier, outcome) = agent_flow::record(&spec, &out);
        // Reprinted only when the RUN decided a different tier than the probe
        // predicted. On Linux they agree by construction, so this is silent;
        // on Windows the run is the authority and the line above was a guess.
        if !json && tier != predicted {
            println!("{}", tier.report_line());
        }
        outcome?;
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "recorded": out,
                    "app": "agent",
                    "containment": tier.report_line(),
                    "contained": tier.is_enforced(),
                })
            );
        } else {
            println!("Recorded '{}' -> {}", spec.name, out.display());
        }
        return Ok(EXIT_PASS);
    }

    let mut driver = record_driver(&spec)?;
    // --reuse: consult the existing trace per step, re-authoring only
    // drift; the old steps come from the trace being replaced.
    let old_steps = if reuse {
        let (_, steps) = flowproof_replay::load_trace(&out)
            .map_err(|e| format!("--reuse needs an existing trace at {}: {e}", out.display()))?;
        Some(steps)
    } else {
        None
    };
    let result = match &old_steps {
        Some(steps) => flowproof_agent::record_incremental_with_options(
            &spec,
            &mut driver,
            &out,
            author.into(),
            steps,
            recording,
        ),
        None => flowproof_agent::record_with_author_and_options(
            &spec,
            &mut driver,
            &out,
            author.into(),
            recording,
        ),
    };
    let summary = match result {
        Ok(summary) => summary,
        Err(err) => {
            // A clarification is data, not just a message: with --json the
            // payload goes to stdout so the driving agent can enumerate the
            // live screen and rewrite the vague step before re-recording.
            if json {
                if let Some(payload) = record_failure_json(&err) {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
                    );
                    return Ok(EXIT_ERROR);
                }
            }
            return Err(err.to_string());
        }
    };
    if json {
        let payload = serde_json::json!({
            "trace_path": summary.trace_path,
            "steps": summary.steps,
            "reused_steps": summary.reused_steps,
            "routing": summary.routing,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        for decision in &summary.routing {
            println!(
                "  [AUTHOR {}] {}: {}",
                decision.route, decision.step, decision.intent
            );
            if let Some(warning) = &decision.warning {
                eprintln!("WARNING: step {}: {warning}", decision.step);
            }
        }
        let reused = if summary.reused_steps > 0 {
            format!(" ({} reused)", summary.reused_steps)
        } else {
            String::new()
        };
        println!(
            "Recorded '{}': {} steps{reused} -> {}",
            spec.name,
            summary.steps,
            summary.trace_path.display()
        );
    }
    if verify {
        return verify_recording(spec_path, &summary.trace_path, values, json, recording);
    }
    Ok(EXIT_PASS)
}

/// Replay a just-written trace once, and refuse the recording if it cannot
/// reproduce itself.
///
/// A recording is a claim that the flow can be performed again from the
/// trace alone. Nothing checked that claim: authoring succeeded when the
/// live app happened to cooperate, and a target that was merely reachable
/// at that moment — a button under a rotating carousel, a field beneath a
/// datepicker that had not opened yet — was written down as though it always
/// would be. The first person to learn otherwise was whoever ran the suite.
///
/// The trace is KEPT when the replay fails. It is the evidence for `heal`,
/// and deleting the artifact that explains the failure helps nobody; the
/// exit code and the message carry the verdict instead.
fn verify_recording(
    spec_path: &Path,
    trace_path: &Path,
    values: ValuesArgs,
    json: bool,
    recording: flowproof_driver::RecordingOptions,
) -> Result<u8, String> {
    if !json {
        println!("Verifying the recording by replaying it once...");
    }
    let replayed = cmd_run(
        spec_path,
        RunOptions {
            trace: Some(trace_path.to_path_buf()),
            json,
            retries: 0,
            missing: MissingTrace::Error,
            author: AuthorArg::Rules,
            values,
            recording,
        },
    )?;
    if replayed == EXIT_PASS {
        if !json {
            println!("Verified: the recording reproduces itself.");
        }
        return Ok(EXIT_PASS);
    }
    if !json {
        eprintln!(
            "RECORDING NOT REPRODUCIBLE: the flow was performed, but replaying its own trace \
             failed. The trace at {} is kept as evidence - inspect the run report above, then \
             `flowproof heal` it or re-record. A trace that cannot replay is not a recording.",
            trace_path.display()
        );
    }
    Ok(EXIT_ERROR)
}

/// Every `*.flow.yaml` under `dir`, recursively, in stable (sorted) order.
/// `.flowproof` artifact directories are skipped.
fn discover_specs(dir: &Path, found: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == ".flowproof") {
                continue;
            }
            discover_specs(&path, found)?;
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".flow.yaml"))
        {
            found.push(path);
        }
    }
    Ok(())
}

/// Replay a trace, re-running a FAILED attempt up to `retries` extra times
/// with a fresh driver each time. Deterministic replay should be stable,
/// but the infrastructure under it (a dropped CDP frame, a momentarily
/// slow backend) is not — a flow that passes on a second look should not
/// fail the suite.
///
/// The driver a REPLAY gets: the trace's single app's driver, or — for a
/// `multi` header — a `SurfaceRegistry` rebuilt from the header's own
/// surface map, so a replay needs the trace and nothing else. Config was
/// stored as WRITTEN; `${VAR}` urls and connections resolve here, fresh,
/// at every replay.
fn replay_driver(header: &flowproof_trace::Header) -> Result<Box<dyn AppDriver>, String> {
    if header.app.name != "multi" {
        return driver_for(&header.app.name);
    }
    let targets = replay_surface_targets(header)?;
    let surfaces: std::collections::BTreeMap<_, _> = header
        .apps
        .iter()
        .map(|(n, info)| (n.clone(), (info.name.clone(), info.browser.clone())))
        .collect();
    let factory: flowproof_driver::surface::SurfaceFactory = Box::new(move |name| {
        let (kind, browser) = surfaces.get(name).ok_or_else(|| {
            flowproof_driver::DriverError::Uia(format!("surface '{name}' is not in the header"))
        })?;
        let mut driver = driver_for(kind).map_err(flowproof_driver::DriverError::Uia)?;
        stage_surface_browser(driver.as_mut(), browser.as_ref())?;
        Ok(driver)
    });
    Ok(Box::new(flowproof_driver::surface::SurfaceRegistry::new(
        targets,
        factory,
        std::time::Duration::from_secs(15),
    )))
}

/// Each recorded surface's launch target, from the header alone —
/// `${VAR}` refs resolve NOW, so credentials and hosts come from this
/// run's environment exactly as a single-surface replay resolves its url.
fn replay_surface_targets(
    header: &flowproof_trace::Header,
) -> Result<Vec<(String, flowproof_driver::AppTarget)>, String> {
    header
        .apps
        .iter()
        .map(|(name, info)| {
            let resolve = |v: &Option<String>| -> Result<String, String> {
                match v {
                    Some(raw) => {
                        flowproof_trace::secret::resolve_refs(raw).map_err(|e| e.to_string())
                    }
                    None => Ok(String::new()),
                }
            };
            let target = match info.name.as_str() {
                "web" => {
                    if info.url.is_none() {
                        return Err(format!("web surface '{name}' has no url in the header"));
                    }
                    flowproof_driver::AppTarget {
                        command: resolve(&info.url)?,
                        window_name: String::new(),
                    }
                }
                "sap" => flowproof_driver::AppTarget {
                    command: resolve(&info.url)?,
                    window_name: "SAP".into(),
                },
                // Left RAW, unlike every other arm here: the header stores
                // `command`/`window_title` exactly as written (comment on
                // `AppInfo::command`), and a `${captured.x}` inside can only
                // resolve once the block that captures it has replayed —
                // `SurfaceRegistry::activate` resolves it, and any `${VAR}`
                // alongside it, fresh at the surface's actual activation.
                "windows" => flowproof_driver::AppTarget {
                    command: info.command.clone().unwrap_or_default(),
                    window_name: info.window_title.clone().unwrap_or_default(),
                },
                // Pixels mode re-attaches to the window the header recorded.
                "vision" => {
                    if info.window_title.is_none() {
                        return Err(format!(
                            "vision surface '{name}' has no window title in the header"
                        ));
                    }
                    flowproof_driver::AppTarget {
                        command: String::new(),
                        window_name: resolve(&info.window_title)?,
                    }
                }
                id => flowproof_driver::resolve_app(id)
                    .ok_or_else(|| format!("surface '{name}': unknown app '{id}'"))?,
            };
            Ok((name.clone(), target))
        })
        .collect()
}

/// Stage a surface's `browser:` config on its freshly built driver —
/// called by BOTH factories (record from the spec, replay from the
/// header), between the driver's construction and the launch its first
/// activation performs, which is the only window where staging can land.
fn stage_surface_browser(
    driver: &mut dyn AppDriver,
    browser: Option<&flowproof_trace::format::BrowserSetup>,
) -> Result<(), flowproof_driver::DriverError> {
    let Some(browser) = browser.filter(|b| !b.is_empty()) else {
        return Ok(());
    };
    // `downloads_dir` is the one field here resolved from `${VAR}` — see
    // `secret::resolve_downloads_dir`.
    let downloads_dir = flowproof_trace::secret::resolve_downloads_dir(&browser.downloads_dir)
        .map_err(|e| flowproof_driver::DriverError::Uia(e.to_string()))?;
    driver.stage_browser(flowproof_driver::WebBrowserConfig::from_setup_parts(
        browser
            .viewport
            .as_ref()
            .map(|v| (v.width, v.height, v.device_scale_factor, v.mobile, v.touch)),
        browser.user_agent.as_deref(),
        &browser.args,
        browser.clock.as_ref().map(|c| flowproof_driver::WebClock {
            at: c.at.clone(),
            timezone: c.timezone.clone(),
        }),
        browser
            .random
            .as_ref()
            .map(|r| flowproof_driver::WebRandom { seed: r.seed }),
        downloads_dir,
    ))
}

/// The driver a RECORDING run gets: the single app's driver, or — for a
/// multi-surface flow — a `SurfaceRegistry` over `driver_for`, one driver
/// per surface, launched lazily at each surface's first `in:` block.
fn record_driver(spec: &FlowSpec) -> Result<Box<dyn AppDriver>, String> {
    if spec.apps.is_empty() {
        return driver_for(spec.app.id());
    }
    let targets = flowproof_agent::surface_targets(spec).map_err(|e| e.to_string())?;
    let surfaces: std::collections::BTreeMap<_, _> = spec
        .apps
        .iter()
        .map(|(n, s)| (n.clone(), (s.app.id().to_string(), s.browser.clone())))
        .collect();
    let factory: flowproof_driver::surface::SurfaceFactory = Box::new(move |name| {
        let (kind, browser) = surfaces.get(name).ok_or_else(|| {
            flowproof_driver::DriverError::Uia(format!("surface '{name}' is not declared"))
        })?;
        let mut driver = driver_for(kind).map_err(flowproof_driver::DriverError::Uia)?;
        stage_surface_browser(driver.as_mut(), browser.as_ref())?;
        Ok(driver)
    });
    Ok(Box::new(flowproof_driver::surface::SurfaceRegistry::new(
        targets,
        factory,
        std::time::Duration::from_secs(15),
    )))
}

/// fail the suite. Returns the first passing report, else the last
/// failure, with the attempt count.
#[allow(clippy::too_many_arguments)] // internal plumbing fn; grouping would obscure it
fn replay_with_retries(
    trace_path: &Path,
    header: &flowproof_trace::Header,
    retries: u8,
    announce: bool,
    secret_scan: &flowproof_replay::SecretScan,
    recording: flowproof_driver::RecordingOptions,
    exports: &std::collections::BTreeMap<String, String>,
    login: Option<&flowproof_agent::LoginSpec>,
) -> Result<
    (
        flowproof_replay::RunReport,
        PathBuf,
        u32,
        flowproof_replay::ResolvedExports,
    ),
    String,
> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut driver = replay_driver(header)?;
        // Credentials are SPEC-driven, like the secret-leak scan: the
        // password is not a header field, so it cannot come from the trace,
        // and every `run` has the spec in hand. `${VAR}`s resolve here, on
        // this replay, not at record.
        if let Some(login) = login {
            driver
                .stage_credentials(login.resolved().map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
        }
        let (report, run_dir, resolved) = flowproof_replay::run_trace_with_exports(
            trace_path,
            &mut driver,
            secret_scan,
            recording,
            exports,
        )
        .map_err(|e| e.to_string())?;
        if report.passed || attempt > u32::from(retries) {
            return Ok((report, run_dir, attempt, resolved));
        }
        if announce {
            println!(
                "  retry {attempt}/{retries}: '{}' failed, re-running",
                report.name
            );
        }
    }
}

/// Export the manifest's `env` to the process (inherited by every flow and
/// hook). Values may carry `${VAR}` references, resolved from the ambient
/// environment — so a suite can re-map or compose existing variables.
///
/// Resolution is LAZY per entry: an unresolvable value is skipped with a
/// warning instead of aborting, so a suite-wide var one flow needs never
/// blocks a flow that doesn't reference it (an `app: api` spec needing
/// only ${DM_API} must run with ${DM_BASE_URL} unset). Flows that DO
/// reference the skipped key still fail at moment-of-use, naming the
/// variable — record and replay both resolve per-use.
fn apply_suite_env(manifest: &flowproof_agent::SuiteManifest) {
    for (key, value) in &manifest.env {
        match flowproof_trace::secret::resolve_refs(value) {
            Ok(resolved) => std::env::set_var(key, resolved),
            Err(e) => eprintln!(
                "warning: suite env `{key}` not set — {e}; \
                 flows that reference ${{{key}}} will fail when they use it"
            ),
        }
    }
}

#[derive(Default)]
struct EnvOverlay {
    previous: Vec<(String, Option<std::ffi::OsString>)>,
}

impl EnvOverlay {
    fn set(&mut self, name: &str, value: &str) {
        if !self.previous.iter().any(|(key, _)| key == name) {
            self.previous
                .push((name.to_string(), std::env::var_os(name)));
        }
        std::env::set_var(name, value);
    }
}

impl Drop for EnvOverlay {
    fn drop(&mut self) {
        for (name, previous) in self.previous.iter().rev() {
            match previous {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn value_file_scalar(path: &Path, key: &str, value: &serde_yaml::Value) -> Result<String, String> {
    match value {
        serde_yaml::Value::String(s) => Ok(s.clone()),
        serde_yaml::Value::Bool(b) => Ok(b.to_string()),
        serde_yaml::Value::Number(n) => Ok(n.to_string()),
        serde_yaml::Value::Null
        | serde_yaml::Value::Sequence(_)
        | serde_yaml::Value::Mapping(_)
        | serde_yaml::Value::Tagged(_) => Err(format!(
            "values file {} key `{key}` must be a string, number, or bool",
            path.display()
        )),
    }
}

fn apply_values_file(path: &Path, overlay: &mut EnvOverlay) -> Result<(), String> {
    let raw =
        std::fs::read_to_string(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let value: serde_yaml::Value =
        serde_yaml::from_str(&raw).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    let Some(mapping) = value.as_mapping() else {
        return Err(format!(
            "values file {} must be a YAML mapping",
            path.display()
        ));
    };
    for (key, value) in mapping {
        let Some(name) = key.as_str() else {
            return Err(format!(
                "values file {} has a non-string key",
                path.display()
            ));
        };
        if !valid_env_name(name) {
            return Err(format!(
                "values file {} key `{name}` is invalid \
                 (must match [A-Za-z_][A-Za-z0-9_]*)",
                path.display()
            ));
        }
        let scalar = value_file_scalar(path, name, value)?;
        overlay.set(name, &scalar);
    }
    Ok(())
}

fn parse_var_arg(raw: &str) -> Result<(&str, &str), String> {
    let Some((name, value)) = raw.split_once('=') else {
        return Err(format!("--var `{raw}` must be KEY=VALUE"));
    };
    if !valid_env_name(name) {
        return Err(format!(
            "--var `{raw}` has invalid key `{name}` \
             (must match [A-Za-z_][A-Za-z0-9_]*)"
        ));
    }
    Ok((name, value))
}

fn apply_values_context(spec_path: &Path, args: &ValuesArgs) -> Result<EnvOverlay, String> {
    let mut overlay = EnvOverlay::default();
    let sibling = default_values_path(spec_path);
    if args.vars_file.is_none() && sibling.exists() {
        apply_values_file(&sibling, &mut overlay)?;
    }
    if let Some(path) = &args.vars_file {
        apply_values_file(path, &mut overlay)?;
    }
    for raw in &args.vars {
        let (name, value) = parse_var_arg(raw)?;
        overlay.set(name, value);
    }
    Ok(overlay)
}

/// Parse a data command's stdout into env pairs. Dotenv-ish and strict:
/// blank lines and `#` comments are skipped; everything else must be
/// `NAME=VALUE` with a `${VAR}`-legal name; the value is taken verbatim
/// (no quote stripping). Anything else is an error naming the line —
/// running flows against half-seeded data is the failure mode to prevent.
fn parse_env_lines(stdout: &str) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            return Err(format!("env_from output line {} is not NAME=VALUE", i + 1));
        };
        let name = name.trim();
        if !valid_env_name(name) {
            return Err(format!(
                "env_from output line {} has invalid name '{name}' \
                 (must match [A-Za-z_][A-Za-z0-9_]*)",
                i + 1
            ));
        }
        pairs.push((name.to_string(), value.to_string()));
    }
    Ok(pairs)
}

/// Run the manifest's `env_from` command (if any) and export its stdout as
/// env vars — the bridge from an external data CLI (DataMaker minting test
/// data from SAP) into `${VAR}` references. Runs via `sh -c` from the
/// suite directory, with stdout captured (`.output()` — the one thing
/// `before_each` hooks structurally cannot do). Fails closed on a non-zero
/// exit or malformed output. Runs BEFORE `env:` so declared env can
/// compose/override captured values.
fn apply_env_from(manifest: &flowproof_agent::SuiteManifest, dir: &Path) -> Result<(), String> {
    let Some(command) = &manifest.env_from else {
        return Ok(());
    };
    // The data command SEES the suite's `env:`. Minting test data almost
    // always needs the suite's base URL and credentials, and before this
    // the command ran with none of them: a mint script reading $API_BASE
    // got an empty string and failed closed downstream, which cost real
    // diagnosis time in the field.
    //
    // Two orderings are easy to conflate, and only the second changes:
    //   1. which value wins for `${VAR}` at flow time - UNCHANGED, still
    //      process env < env_from output < `env:`;
    //   2. what the env_from CHILD PROCESS sees - now `env:` too.
    // Entries are resolved against the ambient process environment only,
    // and one that does not resolve yet is skipped rather than fatal: it
    // may reference this very command's output, and it gets its turn when
    // `env:` is applied afterwards.
    let mut child = std::process::Command::new("sh");
    child.arg("-c").arg(command).current_dir(dir);
    for (key, value) in &manifest.env {
        if let Ok(resolved) = flowproof_trace::secret::resolve_refs(value) {
            child.env(key, resolved);
        }
    }
    let output = child
        .output()
        .map_err(|e| format!("env_from command failed to start: {e}"))?;
    // The command's stderr is TEED, not swallowed and not merely inherited:
    // echoed so a mint script that explains itself is audible even when it
    // succeeds (half the field diagnosis cost was a script with no voice),
    // AND kept in the error below so a programmatic caller still gets the
    // reason. Inheriting alone would have made the failure message say only
    // "see above", which is useless to `--json`.
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        eprint!("{stderr}");
    }
    if !output.status.success() {
        return Err(format!(
            "env_from command exited with {}: {}",
            output.status.code().unwrap_or(-1),
            stderr.trim()
        ));
    }
    for (name, value) in parse_env_lines(&String::from_utf8_lossy(&output.stdout))? {
        std::env::set_var(name, value);
    }
    Ok(())
}

/// Apply the suite context governing a single spec: discover the nearest
/// `suite.yaml` walking up from the spec (nearest wins), run its
/// `env_from`, export its `env`. `record` and single-spec `run` call this
/// so a flow behaves the same alone as inside its suite — the data a
/// DataMaker CLI mints at suite level reaches `${VAR}` at record time AND
/// replay time. No manifest = no-op. Returns the manifest so callers can
/// apply its non-env defaults (e.g. `browser:`) to the spec.
///
/// Before any of that: seed `${VAR}`s from `flowproof config`'s file,
/// fill-gaps-only, so a bare single-flow run with no suite at all still has
/// something to fall back on (plans/001-credential-config.md, "How it
/// reaches the flow"). This runs unconditionally, ahead of the "no
/// suite.yaml" early return below, precisely because that's the case with
/// nothing else to seed from.
pub fn apply_suite_context(
    spec_path: &Path,
) -> Result<Option<flowproof_agent::SuiteManifest>, String> {
    config::seed_env();
    let Some((manifest, dir)) =
        flowproof_agent::SuiteManifest::discover(spec_path).map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    // Name the manifest so a surprising ancestor suite.yaml is visible.
    eprintln!(
        "using suite context from {}",
        dir.join("suite.yaml").display()
    );
    manifest.check_min_version(env!("CARGO_PKG_VERSION"))?;
    apply_env_from(&manifest, &dir)?;
    apply_suite_env(&manifest);
    Ok(Some(manifest))
}

/// Dereference a flow's `session: <name>` ref against its suite's
/// `identities:`, replacing it with the named identity's inline setup - a
/// load-time copy, so the trace stays self-contained. `manifest.is_some()`
/// is the "has a governing suite" signal: a bare name with no suite is a
/// load-time error naming the missing suite. A no-op for an inline `session:`
/// or a flow with no session.
fn dereference_identity(
    spec: &mut FlowSpec,
    manifest: Option<&flowproof_agent::SuiteManifest>,
) -> Result<(), String> {
    let empty = std::collections::BTreeMap::new();
    let identities = manifest.map(|m| &m.identities).unwrap_or(&empty);
    spec.dereference_session(identities, manifest.is_some())
}

/// Reorder discovered specs to honor the manifest's explicit `order`
/// (paths relative to the suite dir); unlisted specs keep their sorted
/// position, after the listed ones.
fn order_specs(specs: &mut [PathBuf], dir: &Path, order: &[String]) {
    if order.is_empty() {
        return;
    }
    let rank = |path: &PathBuf| -> usize {
        let rel = path.strip_prefix(dir).unwrap_or(path);
        order
            .iter()
            .position(|o| Path::new(o) == rel)
            .unwrap_or(order.len())
    };
    specs.sort_by(|a, b| rank(a).cmp(&rank(b)).then_with(|| a.cmp(b)));
}

/// Run a suite hook via `sh -c`, with the current spec path in
/// `FLOWPROOF_SPEC`. A non-zero exit aborts the suite: seed/cleanup that
/// silently failed is exactly the fragility the eval warned about.
fn run_hook(command: &str, spec_path: &Path, phase: &str) -> Result<(), String> {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("FLOWPROOF_SPEC", spec_path)
        .status()
        .map_err(|e| format!("{phase} hook failed to start: {e}"))?;
    if !status.success() {
        return Err(format!(
            "{phase} hook exited with {} for {}",
            status.code().unwrap_or(-1),
            spec_path.display()
        ));
    }
    Ok(())
}

/// What a suite run does with a spec whose trace was never recorded.
/// Adoption reality: a suite's specs land in review before their traces do
/// (37/38 in the first external consumer) — one traceless spec must not
/// hard-fail everyone by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MissingTrace {
    /// Report the flow as junit `skipped` with a reason (default).
    #[default]
    Skip,
    /// Record the missing trace first, then replay (`--record-missing`).
    Record,
    /// Hard error, pre-0.2.2 behavior (`--strict`).
    Error,
}

/// Record a spec in place (suite env already applied by the caller).
/// The core of `cmd_record` without its CLI rendering.
fn record_one(
    spec_path: &Path,
    out: &Path,
    manifest: &flowproof_agent::SuiteManifest,
    author: AuthorArg,
    recording: flowproof_driver::RecordingOptions,
) -> Result<flowproof_agent::RecordSummary, String> {
    let mut spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    if spec.browser.is_none() {
        spec.browser = manifest.browser.clone();
    }
    // A directory run is a suite context, so a `session: <name>` resolves
    // against the manifest's identities (empty if none declared).
    spec.dereference_session(&manifest.identities, true)?;
    let fallback = author == AuthorArg::Auto
        && spec.has_plain_steps()
        && matches!(
            flowproof_agent::HttpModelClient::from_env_result(),
            Ok(None)
        );
    let mut driver = record_driver(&spec)?;
    flowproof_agent::record_with_author_and_options(
        &spec,
        &mut driver,
        out,
        author.into(),
        recording,
    )
    .map_err(|e| {
        if fallback {
            format!(
                "no authoring model is configured; plain steps may use deterministic grammar fallback: {e}"
            )
        } else {
            e.to_string()
        }
    })
}

/// Run every recorded flow under `dir` as one suite: per-flow bundles as
/// usual, plus a merged `junit.xml` for CI, and a non-zero exit if ANY flow
/// fails. A failing flow does not stop the suite.
/// Record one flow as ERRORED and keep the suite going. A driver fault, an
/// unreadable trace or a failing seed hook is one flow's problem: before
/// this existed, the first such fault aborted the whole run and no merged
/// junit was written at all, so CI saw nothing (field report, round 3).
fn errored_flow(
    spec_path: &Path,
    name: &str,
    message: String,
    json: bool,
    flows: &mut Vec<serde_json::Value>,
    reports: &mut Vec<flowproof_replay::RunReport>,
) {
    let report = flowproof_replay::RunReport::errored(name, &message);
    if !json {
        println!("[ERROR] {name} ({message})");
    }
    flows.push(serde_json::json!({
        "spec": spec_path,
        "report": report,
        "report_path": null,
    }));
    reports.push(report);
}

/// Replay one agent flow inside a suite run, with the suite's hooks.
///
/// `Err` is a HARNESS fault (a failing seed or cleanup hook); the agent's own
/// verdict comes back as the inner `Result`, so a failing flow is a failing
/// flow rather than a broken suite. Cleanup runs whichever way replay went,
/// matching the ordering the step-replay path uses below.
fn run_agent_flow_in_suite(
    spec_path: &Path,
    spec: &FlowSpec,
    trace_path: &Path,
    manifest: &flowproof_agent::SuiteManifest,
    json: bool,
) -> Result<Result<(), String>, String> {
    if let Some(cmd) = &manifest.before_each {
        run_hook(cmd, spec_path, "before_each")?;
    }
    // The containment tier prints on every agent run, pass or fail - the
    // single-spec path does the same, and a suite must not hide it.
    let predicted = agent_flow::containment(spec);
    if !json {
        println!("{}", predicted.report_line());
    }
    let (tier, outcome) = agent_flow::replay(spec, trace_path);
    if !json && tier != predicted {
        println!("{}", tier.report_line());
    }
    if let Some(cmd) = &manifest.after_each {
        run_hook(cmd, spec_path, "after_each")?;
    }
    Ok(outcome)
}

pub fn run_suite(dir: &Path, json: bool, retries: u8, missing: MissingTrace) -> Result<u8, String> {
    run_suite_with_author(
        dir,
        json,
        retries,
        missing,
        AuthorArg::Auto,
        ValuesArgs::default(),
        flowproof_driver::RecordingOptions::default(),
    )
}

fn run_suite_with_author(
    dir: &Path,
    json: bool,
    retries: u8,
    missing: MissingTrace,
    author: AuthorArg,
    values: ValuesArgs,
    recording: flowproof_driver::RecordingOptions,
) -> Result<u8, String> {
    // Same fill-gaps-only seed `apply_suite_context` does for a single flow
    // (plans/001-credential-config.md, "How it reaches the flow") — this is
    // the other entry point that reaches a driver, and it built its own
    // manifest handling below without ever going through that chokepoint, so
    // the config file was invisible to every suite-mode `run`.
    config::seed_env();
    let mut specs = Vec::new();
    discover_specs(dir, &mut specs)?;
    if specs.is_empty() {
        return Err(format!("no *.flow.yaml specs under {}", dir.display()));
    }

    // An optional suite.yaml declares shared env and per-flow seed/cleanup
    // hooks — the sequencing a hand-written harness otherwise provides.
    let manifest = flowproof_agent::SuiteManifest::load_from_dir(dir)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    manifest.check_min_version(env!("CARGO_PKG_VERSION"))?;
    apply_env_from(&manifest, dir)?;
    apply_suite_env(&manifest);
    order_specs(&mut specs, dir, &manifest.order);

    // Control-id uniqueness is a suite-level property, enforced at load: two
    // flows sharing a control id would corrupt the audit coverage map. Only
    // parseable specs are considered here; a spec that will not parse is
    // reported as one broken flow in the loop below.
    let loaded: Vec<(String, FlowSpec)> = specs
        .iter()
        .filter_map(|p| FlowSpec::load(p).ok().map(|s| (p.display().to_string(), s)))
        .collect();
    flowproof_agent::check_control_ids(loaded.iter().map(|(p, s)| (p.as_str(), s)))?;

    let mut reports: Vec<flowproof_replay::RunReport> = Vec::new();
    let mut flows = Vec::new();
    for spec_path in &specs {
        let mut authoring = None;
        // The env-flag gate wins over everything (including --strict's
        // missing-trace error): a deliberately gated flow with no trace
        // is a skip, not a failure. Loading here also surfaces spec parse
        // errors for every suite member.
        let gated_spec = match FlowSpec::load(spec_path).map_err(|e| e.to_string()) {
            Ok(spec) => spec,
            // A spec that will not parse is one broken flow, not a broken
            // suite: record it and keep going.
            Err(e) => {
                errored_flow(
                    spec_path,
                    &spec_path.display().to_string(),
                    e,
                    json,
                    &mut flows,
                    &mut reports,
                );
                continue;
            }
        };
        let values_overlay = match apply_values_context(spec_path, &values) {
            Ok(overlay) => overlay,
            Err(e) => {
                errored_flow(
                    spec_path,
                    &gated_spec.name,
                    e,
                    json,
                    &mut flows,
                    &mut reports,
                );
                continue;
            }
        };
        if let Some(reason) = gated_spec.skip_reason() {
            let report = flowproof_replay::RunReport::skipped(&gated_spec.name, &reason);
            if !json {
                println!("[SKIP] {} ({reason})", report.name);
            }
            flows.push(serde_json::json!({
                "spec": spec_path,
                "report": report,
                "report_path": null,
            }));
            reports.push(report);
            continue;
        }
        let trace_path = default_trace_path(spec_path);
        if !trace_path.exists() {
            match missing {
                MissingTrace::Error => {
                    errored_flow(
                        spec_path,
                        &gated_spec.name,
                        format!(
                            "trace {} not found — run `flowproof record {}` first",
                            trace_path.display(),
                            spec_path.display()
                        ),
                        json,
                        &mut flows,
                        &mut reports,
                    );
                    continue;
                }
                MissingTrace::Record => {
                    if !json {
                        println!("[RECORD] {} (no trace yet)", spec_path.display());
                    }
                    match record_one(spec_path, &trace_path, &manifest, author, recording) {
                        Ok(summary) => {
                            if !json {
                                for decision in &summary.routing {
                                    println!(
                                        "  [AUTHOR {}] {}: {}",
                                        decision.route, decision.step, decision.intent
                                    );
                                    if let Some(warning) = &decision.warning {
                                        eprintln!("WARNING: step {}: {warning}", decision.step);
                                    }
                                }
                            }
                            authoring = Some(summary);
                        }
                        Err(e) => {
                            errored_flow(
                                spec_path,
                                &gated_spec.name,
                                e,
                                json,
                                &mut flows,
                                &mut reports,
                            );
                            continue;
                        }
                    }
                    // Fall through to the normal replay below.
                }
                MissingTrace::Skip => {
                    // The flow never ran: no hooks, no run bundle — just a
                    // visible skipped entry so coverage doesn't silently
                    // shrink.
                    let reason = format!(
                        "no trace recorded — flowproof record {}",
                        spec_path.display()
                    );
                    let report = flowproof_replay::RunReport::skipped(&gated_spec.name, &reason);
                    if !json {
                        println!("[SKIP] {} ({reason})", report.name);
                    }
                    flows.push(serde_json::json!({
                        "spec": spec_path,
                        "report": report,
                        "report_path": null,
                        "authoring": authoring,
                    }));
                    reports.push(report);
                    continue;
                }
            }
        }
        // Agent flows replay their CASSETTE, not the step trace, exactly as
        // the single-spec path at `run_one` does.
        //
        // Without this branch the suite fell through to `load_trace` below,
        // which parses a UI trace one JSON object per line. An agent cassette
        // is a single `{app, mocks, cassette}` document, so every agent flow
        // in a directory run errored with "invalid trace line" - traces that
        // `flowproof record` had just written, and that `flowproof run <spec>`
        // replayed fine one at a time. Directory mode is what a suite and CI
        // invoke, so agent flows were effectively unrunnable there.
        if gated_spec.app.id() == "agent" {
            let started = Instant::now();
            match run_agent_flow_in_suite(spec_path, &gated_spec, &trace_path, &manifest, json) {
                Ok(outcome) => {
                    let report = flowproof_replay::RunReport::agent(
                        &gated_spec.name,
                        outcome.as_ref().err().map(String::as_str),
                        started.elapsed().as_millis() as u64,
                    );
                    if !json {
                        match &outcome {
                            Ok(()) => {
                                println!("[PASS] {} ({} ms)", report.name, report.duration_ms)
                            }
                            Err(why) => println!("[FAIL] {} — {why}", report.name),
                        }
                    }
                    // Agent flows produce no run bundle, so there is no
                    // per-flow result path - the suite record below still
                    // carries the verdict.
                    flows.push(serde_json::json!({
                        "spec": spec_path,
                        "report": report,
                        "report_path": null,
                    }));
                    reports.push(report);
                }
                // A hook fault is a harness fault, not a verdict about the
                // agent: same treatment every other flow's hook failure gets.
                Err(e) => {
                    errored_flow(
                        spec_path,
                        &gated_spec.name,
                        e,
                        json,
                        &mut flows,
                        &mut reports,
                    );
                }
            }
            continue;
        }
        // Seed before the flow; a failing hook fails the flow, not the run.
        if let Some(cmd) = &manifest.before_each {
            if let Err(e) = run_hook(cmd, spec_path, "before_each") {
                errored_flow(
                    spec_path,
                    &gated_spec.name,
                    e,
                    json,
                    &mut flows,
                    &mut reports,
                );
                continue;
            }
        }
        // The secret-leak scan is spec-driven (the trace stores no secret-leak
        // steps): replay re-observes the corpus and scans the same names.
        let secret_scan = flowproof_replay::SecretScan {
            assertions: gated_spec.secret_leak_assertions(),
        };
        let replayed = flowproof_replay::load_trace(&trace_path)
            .map_err(|e| e.to_string())
            // A fresh driver per flow: full isolation, like Playwright
            // contexts. A driver fault here ends THIS flow only.
            .and_then(|(header, _)| {
                replay_with_retries(
                    &trace_path,
                    &header,
                    retries,
                    !json,
                    &secret_scan,
                    recording,
                    &gated_spec.exports,
                    gated_spec.login.as_ref(),
                )
            });
        // Cleanup always runs, pass, fail or error.
        let cleanup = match &manifest.after_each {
            Some(cmd) => run_hook(cmd, spec_path, "after_each"),
            None => Ok(()),
        };
        // Replay first, cleanup second, and only then decide the outcome:
        // whichever failed, the cleanup has already run.
        let (report, run_dir, attempts, exported) =
            match replayed.and_then(|tuple| cleanup.map(|()| tuple)) {
                Ok(tuple) => tuple,
                Err(e) => {
                    errored_flow(
                        spec_path,
                        &gated_spec.name,
                        e,
                        json,
                        &mut flows,
                        &mut reports,
                    );
                    continue;
                }
            };
        let result_path = match report.write_into(&run_dir).map_err(|e| e.to_string()) {
            Ok(path) => path,
            Err(e) => {
                errored_flow(
                    spec_path,
                    &gated_spec.name,
                    e,
                    json,
                    &mut flows,
                    &mut reports,
                );
                continue;
            }
        };
        // A passing flow's exports become environment variables for the
        // flows that follow — how a value minted on one surface (an order
        // number off SAP's status bar) reaches a spec driving another (the
        // portal that must show it). Process-local, gone when the run ends.
        // The visible line names WHAT was exported, never what it held: a
        // captured value stays out of CI logs the same way it stays out of
        // the trace.
        let export_names: Vec<&str> = exported.iter().map(|(n, _)| n.as_str()).collect();
        drop(values_overlay);
        if report.passed {
            for (name, value) in &exported {
                std::env::set_var(name, value);
            }
        }
        if !json {
            println!(
                "[{}] {} ({} ms){}{}",
                if report.passed { "PASS" } else { "FAIL" },
                report.name,
                report.duration_ms,
                if report.degraded { " DEGRADED" } else { "" },
                if attempts > 1 {
                    format!(" (after {attempts} attempts)")
                } else {
                    String::new()
                },
            );
            if report.passed && !export_names.is_empty() {
                println!("  [EXPORT] {}", export_names.join(", "));
            }
            if !report.passed {
                for step in report.steps.iter().filter(|s| s.detail.is_some()) {
                    println!(
                        "    [FAIL] {} {} — {}",
                        step.id,
                        step.intent,
                        step.detail.as_deref().unwrap_or("")
                    );
                }
            }
        }
        flows.push(serde_json::json!({
            "spec": spec_path,
            "report": report,
            "report_path": result_path,
            "authoring": authoring,
            // Names only, by the same rule as the [EXPORT] line.
            "exports": export_names,
        }));
        reports.push(report);
    }

    let junit_path = dir.join(".flowproof").join("suite-junit.xml");
    std::fs::create_dir_all(junit_path.parent().expect("suite dir has a parent"))
        .map_err(|e| e.to_string())?;
    std::fs::write(
        &junit_path,
        flowproof_replay::RunReport::suite_junit_xml(reports.iter()),
    )
    .map_err(|e| e.to_string())?;

    // The structured run record: every flow, folded with its control verdict,
    // read from the reports just produced (no re-replay). `flows` and `reports`
    // are pushed in lockstep, so they align 1:1; each flow's spec is looked up
    // for its `control:` block (a spec that would not parse has no control).
    let spec_by_path: std::collections::HashMap<&str, &FlowSpec> =
        loaded.iter().map(|(p, s)| (p.as_str(), s)).collect();
    let flow_records: Vec<flowproof_replay::FlowRecord> = flows
        .iter()
        .zip(&reports)
        .map(|(entry, report)| {
            let spec_path = entry
                .get("spec")
                .and_then(|v| v.as_str())
                .map(PathBuf::from)
                .unwrap_or_default();
            let spec = spec_by_path
                .get(spec_path.to_string_lossy().as_ref())
                .copied();
            flow_record_from_report(&spec_path, dir, spec, report)
        })
        .collect();
    write_run_record(dir, flow_records);

    let skipped = reports.iter().filter(|r| r.trace_id == "skipped").count();
    let errored = reports.iter().filter(|r| r.trace_id == "errored").count();
    let passed = reports.iter().filter(|r| r.passed).count() - skipped;
    let ran = reports.len() - skipped;
    let all_passed = reports.iter().all(|r| r.passed);
    if json {
        let payload = serde_json::json!({
            "flows": flows,
            "passed": all_passed,
            "skipped": skipped,
            "errored": errored,
            "junit_path": junit_path,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        println!(
            "{}: {passed}/{ran} flows passed{}{} -> {}",
            if all_passed { "PASS" } else { "FAIL" },
            if skipped > 0 {
                format!(", {skipped} skipped")
            } else {
                String::new()
            },
            if errored > 0 {
                format!(", {errored} errored")
            } else {
                String::new()
            },
            junit_path.display()
        );
        if reports.iter().any(|r| r.degraded) {
            println!("DEGRADED: fallback selectors were needed in some flows — heal them");
        }
    }
    Ok(if errored > 0 {
        EXIT_ERROR
    } else if all_passed {
        EXIT_PASS
    } else {
        EXIT_FAIL
    })
}

/// How many run records to keep under a suite's `.flowproof/runs/`. Older
/// records are pruned after each new one, so the history stays bounded while
/// leaving a `--since` window to diff against.
const RUN_RECORD_RETENTION: usize = 10;

/// Mint a run-id and its RFC3339 start stamp. The run-id leads with a
/// filesystem-safe RFC3339 timestamp (colons rewritten to dashes) so a plain
/// string sort is chronological, and ends with a short suffix that
/// disambiguates same-second runs. The real clock is fine here: the record is
/// an OUTPUT artifact, not a trace input, so it never touches replay
/// determinism.
fn mint_run_id() -> (String, String) {
    let now = chrono::Utc::now();
    let started_at = now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let stamp = now.format("%Y-%m-%dT%H-%M-%SZ");
    // A cheap, dependency-free suffix: the pid mixed with the sub-second
    // nanos. It only needs to break ties between runs in the same second.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let suffix = (std::process::id() ^ nanos) & 0xffff;
    (format!("{stamp}-{suffix:04x}"), started_at)
}

/// A path relative to the suite directory, for the record's stable, portable
/// references (the flow path, the evidence trace pointer).
fn rel(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir).unwrap_or(path).display().to_string()
}

/// The suite directory containing a single spec, for a single-spec run's
/// record. `.` when the spec has no parent component.
fn spec_dir(spec_path: &Path) -> PathBuf {
    spec_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Build a flow's `control:` row for the run record from a control verdict
/// already read (from the flow's replay report or an agent flow's outcome).
/// Returns `None` for a flow with no `control:` block. Everything else - the
/// lanes, the secret names, the corpus/exclusion descriptors, the evidence
/// pointers - comes from the spec, never a resolved value.
fn build_control_record(
    spec_path: &Path,
    dir: &Path,
    spec: &FlowSpec,
    verdict: flowproof_replay::ControlVerdict,
    reason: Option<String>,
    // The tier the run ACHIEVED, where a run decided one. `None` for a flow
    // that never ran an agent - a step-engine flow has no agent run to ask.
    achieved: Option<&flowproof_adapters::Containment>,
) -> Option<flowproof_replay::ControlRecord> {
    let control = spec.control.as_ref()?;
    let secrets_checked = spec.secret_leak_selectors();
    // The corpus + exclusions describe ONLY a flow that actually ran a
    // secret-leak scan, exactly as the live audit renderer always has.
    let (corpus, excluded) = if secrets_checked.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        secret_scan_corpus_report(spec.app.id())
    };
    let mut lanes = Vec::new();
    if agent_flow::engages_egress(spec) {
        lanes.push("egress".to_string());
    }
    if !secrets_checked.is_empty() {
        lanes.push("secret_leak".to_string());
    }
    let trace_path = default_trace_path(spec_path);
    // The containment tier THIS host actually ran under. A flow that engages
    // no egress claims no tier, so the field stays absent rather than
    // recording a "not contained" that was never in question.
    //
    // The RUN's answer where it decided one, and only the probe's prediction
    // where it did not. This field is read twice: once as the record's own
    // containment line, and once below to decide whether the blocked lane is
    // evidence at all. So a predicted "not contained" over a run that WAS
    // contained does not merely mislabel the record - it discards the
    // destinations that run actually refused.
    let containment = agent_flow::engages_egress(spec)
        .then(|| {
            achieved
                .cloned()
                .unwrap_or_else(|| agent_flow::containment(spec))
        })
        .filter(|_| spec.app.id() == "agent");
    // The egress blocked lane is an agent-flow concept; it is a value-free
    // audit descriptor, safe to read off any agent trace.
    //
    // It is read from the RECORDED trace, so it is only evidence about THIS
    // run when this run was itself contained. A Linux recording replayed on
    // a host without containment would otherwise present destinations some
    // other machine blocked, on some other day, as proof for an uncontained
    // run - the record's most misleading possible sentence.
    let blocked = match (spec.app.id(), &containment) {
        ("agent", Some(tier)) if tier.is_enforced() => agent_flow::egress_blocked(&trace_path),
        _ => Vec::new(),
    };
    Some(flowproof_replay::ControlRecord {
        id: control.id.clone(),
        title: control.title.clone(),
        verdict,
        reason,
        lanes,
        containment: containment.as_ref().map(agent_flow::containment_tag),
        evidence: flowproof_replay::Evidence {
            trace: rel(dir, &trace_path),
            blocked,
        },
        secrets_checked,
        corpus,
        excluded,
    })
}

/// Fold one flow's replay report into its record row, reading the control
/// verdict from the report (never re-replaying). `spec` is `None` for a flow
/// whose spec would not parse.
fn flow_record_from_report(
    spec_path: &Path,
    dir: &Path,
    spec: Option<&FlowSpec>,
    report: &flowproof_replay::RunReport,
) -> flowproof_replay::FlowRecord {
    let control = match spec {
        Some(spec) if spec.control.is_some() => {
            let (verdict, reason) = flowproof_replay::ControlVerdict::from_run_report(report);
            build_control_record(spec_path, dir, spec, verdict, reason, None)
        }
        _ => None,
    };
    flowproof_replay::FlowRecord {
        flow: rel(dir, spec_path),
        status: flowproof_replay::FlowStatus::from_run_report(report),
        degraded: report.degraded,
        control,
    }
}

/// Write a run record under `dir/.flowproof/runs/<run-id>/report.json`, then
/// prune the history to the most recent [`RUN_RECORD_RETENTION`]. Best-effort:
/// the record is an output, so a write failure warns but never changes the
/// run's exit code. Pruning is logged, never silent.
fn write_run_record(dir: &Path, flows: Vec<flowproof_replay::FlowRecord>) {
    let (run_id, started_at) = mint_run_id();
    let record = flowproof_replay::RunRecord {
        run_id,
        started_at,
        flowproof_version: env!("CARGO_PKG_VERSION").to_string(),
        env: flowproof_replay::RunEnv::current(),
        flows,
    };
    match record.write(dir, RUN_RECORD_RETENTION) {
        Ok((path, pruned)) => {
            eprintln!("wrote run record -> {}", path.display());
            if !pruned.is_empty() {
                eprintln!(
                    "pruned {} old run record(s), keeping the most recent \
                     {RUN_RECORD_RETENTION}: {}",
                    pruned.len(),
                    pruned.join(", ")
                );
            }
        }
        Err(e) => eprintln!(
            "warning: could not write run record under {}: {e}",
            dir.display()
        ),
    }
}

struct RunOptions {
    trace: Option<PathBuf>,
    json: bool,
    retries: u8,
    missing: MissingTrace,
    author: AuthorArg,
    values: ValuesArgs,
    recording: flowproof_driver::RecordingOptions,
}

fn cmd_run(spec_path: &Path, options: RunOptions) -> Result<u8, String> {
    let RunOptions {
        trace,
        json,
        retries,
        missing,
        author,
        values,
        recording,
    } = options;
    if spec_path.is_dir() {
        return run_suite_with_author(spec_path, json, retries, missing, author, values, recording);
    }
    // A single flow gets its suite's env/data too — replay resolves ${VAR}
    // at moment-of-use, so the same values must be present as at record.
    // And its HOOKS: running one spec to debug it has to put the app in
    // the same state the suite would, or the spec fails for a reason that
    // has nothing to do with the spec. A field run found this the
    // expensive way - the second consecutive single-spec run failed on
    // state the first had left behind, while the suite passed.
    let manifest = apply_suite_context(spec_path)?;
    let _values = apply_values_context(spec_path, &values)?;
    // Load the spec for its gate (this also surfaces spec parse errors on
    // single runs, deliberately — a typo'd spec should not replay).
    let mut spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    // Surface a bad `session: <name>` the same way record would: a bare name
    // with no governing suite is a load-time error naming the missing suite.
    dereference_identity(&mut spec, manifest.as_ref())?;
    if let Some(reason) = spec.skip_reason() {
        let report = flowproof_replay::RunReport::skipped(&spec.name, &reason);
        // A skip is still a run: record it, so audit sees the flow's control
        // as a capability-error (it never ran) rather than nothing at all.
        let dir = spec_dir(spec_path);
        let flow = flow_record_from_report(spec_path, &dir, Some(&spec), &report);
        write_run_record(&dir, vec![flow]);
        if json {
            let payload = serde_json::json!({
                "report": report,
                "report_path": null,
                "skipped": reason,
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
            );
        } else {
            println!("[SKIP] {} ({reason})", report.name);
        }
        return Ok(EXIT_PASS);
    }
    let trace_path = trace.unwrap_or_else(|| default_trace_path(spec_path));

    // Agent flows replay their cassette through the model-boundary proxy,
    // not the step-replay engine. Suite hooks still apply - the same
    // before/after contract every single-spec run gets.
    if spec.app.id() == "agent" {
        if !trace_path.exists() {
            return Err(format!(
                "trace {} not found — run `flowproof record {}` first",
                trace_path.display(),
                spec_path.display()
            ));
        }
        if let Some(cmd) = manifest.as_ref().and_then(|m| m.before_each.as_ref()) {
            run_hook(cmd, spec_path, "before_each")?;
        }
        // The containment tier prints on EVERY agent run, pass or fail.
        let predicted = agent_flow::containment(&spec);
        if !json {
            println!("{}", predicted.report_line());
        }
        let (tier, outcome) = agent_flow::replay(&spec, &trace_path);
        if !json && tier != predicted {
            println!("{}", tier.report_line());
        }
        if let Some(cmd) = manifest.as_ref().and_then(|m| m.after_each.as_ref()) {
            run_hook(cmd, spec_path, "after_each")?;
        }
        // Fold this agent flow into a run record before reporting. Agent flows
        // never reach the step engine, so the verdict is read from the replay
        // OUTCOME rather than a step report.
        let dir = spec_dir(spec_path);
        let (verdict, reason) = flowproof_replay::ControlVerdict::from_outcome(&outcome);
        let control = if spec.control.is_some() {
            build_control_record(spec_path, &dir, &spec, verdict, reason, Some(&tier))
        } else {
            None
        };
        let flow = flowproof_replay::FlowRecord {
            flow: rel(&dir, spec_path),
            status: if outcome.is_ok() {
                flowproof_replay::FlowStatus::Pass
            } else {
                flowproof_replay::FlowStatus::Fail
            },
            degraded: false,
            control,
        };
        write_run_record(&dir, vec![flow]);
        return match outcome {
            Ok(()) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "passed": true,
                            "app": "agent",
                            "containment": tier.report_line(),
                            "contained": tier.is_enforced(),
                        })
                    );
                } else {
                    println!("PASS: {}", spec.name);
                }
                Ok(EXIT_PASS)
            }
            Err(why) => {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "passed": false,
                            "app": "agent",
                            "error": why,
                            "containment": tier.report_line(),
                            "contained": tier.is_enforced(),
                        })
                    );
                } else {
                    println!("FAIL: {} — {why}", spec.name);
                }
                Ok(EXIT_FAIL)
            }
        };
    }

    if !trace_path.exists() {
        return Err(format!(
            "trace {} not found — run `flowproof record {}` first",
            trace_path.display(),
            spec_path.display()
        ));
    }
    // Seed before the flow, exactly as `run_suite` does.
    if let Some(cmd) = manifest.as_ref().and_then(|m| m.before_each.as_ref()) {
        run_hook(cmd, spec_path, "before_each")?;
    }
    // Peek the header to pick the right driver for the recorded app.
    let (header, _) = flowproof_replay::load_trace(&trace_path).map_err(|e| e.to_string())?;
    // The secret-leak scan is driven by the SPEC (the trace stores no
    // secret-leak steps; the feature is additive): replay re-observes the
    // corpus and scans the same names as record.
    let secret_scan = flowproof_replay::SecretScan {
        assertions: spec.secret_leak_assertions(),
    };
    // Exports resolve here too — a single run has no downstream flow to
    // hand them to, but the VERDICT must not depend on suite vs single
    // invocation: an export that cannot resolve fails the flow either way.
    let replayed = replay_with_retries(
        &trace_path,
        &header,
        retries,
        !json,
        &secret_scan,
        recording,
        &spec.exports,
        spec.login.as_ref(),
    );
    // Cleanup always runs, pass, fail or error - the suite's rule, and the
    // reason it exists is that a flow which errors is exactly when a left
    // -behind fixture hurts most.
    let cleanup = match manifest.as_ref().and_then(|m| m.after_each.as_ref()) {
        Some(cmd) => run_hook(cmd, spec_path, "after_each"),
        None => Ok(()),
    };
    let (report, run_dir, _attempts, _exported) = replayed?;
    cleanup?;

    let result_path = report.write_into(&run_dir).map_err(|e| e.to_string())?;

    // Fold this single flow into a run record beside the spec, so `flowproof
    // audit <dir>` can read its control verdict without re-replaying.
    let dir = spec_dir(spec_path);
    let flow = flow_record_from_report(spec_path, &dir, Some(&spec), &report);
    write_run_record(&dir, vec![flow]);

    if json {
        // The human-readable lines below are a rendering of this same
        // structure — the JSON is the primary output.
        let payload = serde_json::json!({
            "report": report,
            "report_path": result_path,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        for step in &report.steps {
            let (mark, mut suffix) = match step.status {
                StepStatus::Passed => ("PASS", String::new()),
                StepStatus::Failed => (
                    "FAIL",
                    step.detail
                        .as_deref()
                        .map(step_detail_suffix)
                        .unwrap_or_default(),
                ),
                StepStatus::Skipped => ("SKIP", String::new()),
                StepStatus::Errored => (
                    "ERROR",
                    step.detail
                        .as_deref()
                        .map(step_detail_suffix)
                        .unwrap_or_default(),
                ),
            };
            if step.degraded {
                let tier = step.selector_tier.as_deref().unwrap_or("fallback");
                suffix.push_str(&format!(" (matched via {tier} fallback)"));
            }
            println!("  [{mark}] {} {}{suffix}", step.id, step.intent);
        }
        // Point a HUMAN at the human artifact. `result.json` is the machine
        // surface and stays the `--json` payload's `report_path`; the person
        // reading a terminal wants the rendering with the step table, the
        // frames and the recording — and will not find it by guessing, since
        // the bundle sits under a dot-directory Finder hides by default.
        println!(
            "{}: {} ({} ms) -> {}",
            if report.passed { "PASS" } else { "FAIL" },
            report.name,
            report.duration_ms,
            run_dir.join("report.html").display()
        );
        if report.degraded {
            println!(
                "DEGRADED: fallback selectors were needed — the app drifted; \
                 run `flowproof heal {}`",
                spec_path.display()
            );
        }
    }
    Ok(if report.passed { EXIT_PASS } else { EXIT_FAIL })
}

fn step_detail_suffix(detail: &str) -> String {
    let mut suffix = format!(" — {detail}");
    if let Some(command) = config_command_for_missing_secret(detail) {
        suffix.push_str(&format!("\n         Run `{command}` to set it."));
    }
    suffix
}

fn config_command_for_missing_secret(detail: &str) -> Option<&'static str> {
    let var = detail
        .strip_prefix("secret ${")?
        .strip_suffix("} is not set in the environment")?;
    if var.starts_with("SAP_") {
        Some("flowproof config sap")
    } else if var.starts_with("FIORI_") {
        Some("flowproof config fiori")
    } else {
        None
    }
}

/// One control's row in the audit report. A rendering of the persisted run
/// record's control row, so the output shape is stable across the record and
/// the CLI.
#[derive(Debug, serde::Serialize)]
struct AuditControl {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    flow: String,
    verdict: flowproof_replay::ControlVerdict,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Which control lanes the flow asserted (`egress`, `secret_leak`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    lanes: Vec<String>,
    /// The containment tier the run actually ran under. An `egress` lane
    /// says what was ASSERTED; this says what was ENFORCED, and without it
    /// the two are indistinguishable in the artifact auditors read.
    #[serde(skip_serializing_if = "Option::is_none")]
    containment: Option<String>,
    /// Where the control's proof lives: the trace pointer and any blocked
    /// egress destinations.
    evidence: flowproof_replay::Evidence,
    /// The `${VAR}` names an `assert_no_secret_leak` flow checked - NAMES,
    /// never values. Empty for a flow with no secret-leak assertion.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    secrets_checked: Vec<String>,
    /// What the secret scan actually covered, so nobody mistakes it for a
    /// proof about channels the engine never saw.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    corpus: Vec<String>,
    /// The corpus exclusions, echoed so the report is honest about its gaps.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    excluded: Vec<String>,
}

/// The rendered audit report: a suite's control-bearing flows folded into a
/// stable, diffable coverage map.
#[derive(Debug, serde::Serialize)]
struct AuditReport {
    suite: String,
    run: String,
    controls: Vec<AuditControl>,
}

/// The `corpus` and `excluded` audit lines for a secret-leak scan on `app`.
/// Each flow kind names exactly what it scanned and what it could not, so the
/// report never overstates its reach. The web exclusions are part of the
/// corpus definition, echoed exactly as the OCR exclusion is.
fn secret_scan_corpus_report(app: &str) -> (Vec<String>, Vec<String>) {
    match app {
        "web" => (
            vec![
                "surface text at each step boundary".to_string(),
                "assert_api response bodies".to_string(),
            ],
            vec![
                "transient text between step boundaries (capture is per-step, not continuous)"
                    .to_string(),
                "page source not read as text (hidden fields, HTML comments, data- attributes)"
                    .to_string(),
            ],
        ),
        "api" => (
            vec!["assert_api response bodies".to_string()],
            vec!["channels the engine never observed (server logs, third-party sinks)".to_string()],
        ),
        // agent (and any future corpus-bearing kind): the model boundary.
        _ => (
            vec![
                "model-boundary trajectory (cassette request and response bodies)".to_string(),
                "MCP lanes".to_string(),
            ],
            vec!["channels the engine never observed (server logs, third-party sinks)".to_string()],
        ),
    }
}

/// The suite's display name: the directory's file name (canonicalized so a
/// trailing `.` or `..` resolves), falling back to the path as given.
fn suite_name(dir: &Path) -> String {
    dir.canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| dir.display().to_string())
}

/// The error shown when audit finds no run record: point the user at `run`
/// rather than silently re-replaying.
fn no_record_error(dir: &Path) -> String {
    format!(
        "no run record under {}/.flowproof/runs - run `flowproof run {}` first, \
         then audit reads the record it wrote",
        dir.display(),
        dir.display()
    )
}

/// Render a persisted run record as the audit control map: the same output
/// shape audit has always emitted, now sourced from the record instead of a
/// re-replay. Only control-bearing flows appear.
fn audit_report_from_record(dir: &Path, record: &flowproof_replay::RunRecord) -> AuditReport {
    let controls = record
        .controls()
        .map(|(flow, control)| AuditControl {
            id: control.id.clone(),
            title: control.title.clone(),
            flow: flow.flow.clone(),
            verdict: control.verdict,
            reason: control.reason.clone(),
            lanes: control.lanes.clone(),
            containment: control.containment.clone(),
            evidence: control.evidence.clone(),
            secrets_checked: control.secrets_checked.clone(),
            corpus: control.corpus.clone(),
            excluded: control.excluded.clone(),
        })
        .collect();
    AuditReport {
        suite: suite_name(dir),
        run: record.run_id.clone(),
        controls,
    }
}

/// `flowproof audit <dir> --since <run-id>`: diff the latest run record
/// against an earlier one, by `control.id`. Emits controls added, removed
/// (present in the older record, gone in the newer - coverage that shrank),
/// and verdict-changed (old -> new). Exits non-zero on a regression.
fn cmd_audit_diff(dir: &Path, json: bool, base_id: &str) -> Result<u8, String> {
    let head = flowproof_replay::RunRecord::latest(dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| no_record_error(dir))?;
    let base = flowproof_replay::RunRecord::load(dir, base_id).map_err(|e| {
        format!(
            "no run record '{base_id}' under {}/.flowproof/runs: {e}",
            dir.display()
        )
    })?;
    let diff = flowproof_replay::RunDiff::between(&base, &head);
    let rendered = if json {
        serde_json::to_string_pretty(&diff).map_err(|e| e.to_string())?
    } else {
        serde_yaml::to_string(&diff).map_err(|e| e.to_string())?
    };
    println!("{rendered}");
    // A shrunk coverage map or a control that regressed to `fail` is a CI
    // failure; other changes (new controls, a fixed control) are informational.
    Ok(if diff.is_regression() {
        EXIT_FAIL
    } else {
        EXIT_PASS
    })
}

/// `flowproof audit <dir>`: render a suite's control-coverage map by READING
/// the persisted run record `flowproof run` wrote (never re-replaying). The
/// latest record by default, a specific one with `--run <id>`, or a cross-run
/// diff with `--since <run-id>`. Emitted as YAML (default) or JSON. When no
/// record exists, a clear error points the user at `run` rather than silently
/// re-replaying.
fn cmd_audit(
    dir: &Path,
    json: bool,
    run: Option<String>,
    since: Option<String>,
) -> Result<u8, String> {
    if !dir.is_dir() {
        return Err(format!(
            "audit runs over a suite directory; {} is not a directory",
            dir.display()
        ));
    }
    if let Some(base_id) = since {
        return cmd_audit_diff(dir, json, &base_id);
    }

    let record = match &run {
        Some(id) => flowproof_replay::RunRecord::load(dir, id).map_err(|e| {
            format!(
                "no run record '{id}' under {}/.flowproof/runs: {e}",
                dir.display()
            )
        })?,
        None => flowproof_replay::RunRecord::latest(dir)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| no_record_error(dir))?,
    };

    let report = audit_report_from_record(dir, &record);
    let any_failed = report
        .controls
        .iter()
        .any(|c| c.verdict == flowproof_replay::ControlVerdict::Fail);
    let rendered = if json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        serde_yaml::to_string(&report).map_err(|e| e.to_string())?
    };
    println!("{rendered}");
    Ok(if any_failed { EXIT_FAIL } else { EXIT_PASS })
}

fn cmd_author_from_doc(
    doc: PathBuf,
    app: String,
    name: String,
    out: PathBuf,
) -> Result<u8, String> {
    let opts = flowproof_agent::doc_author::DocAuthorOptions {
        doc,
        app,
        name,
        out,
    };
    match flowproof_agent::doc_author::author_from_doc(&opts) {
        Ok(result) => {
            println!(
                "draft spec written to {} — DRAFT; review every step (a flagged one needs \
                 the live app to resolve it, an assert is a light translation of the \
                 document's own wording), then `flowproof record`",
                result.flow.display()
            );
            if let Some(values) = result.values {
                println!(
                    "business-data values written to {} — review them; secrets still belong in `flowproof config`",
                    values.display()
                );
            }
            Ok(EXIT_PASS)
        }
        Err(e) => Err(e.to_string()),
    }
}

fn cmd_heal(
    spec_path: &Path,
    trace: Option<PathBuf>,
    apply: bool,
    json: bool,
    author: AuthorArg,
) -> Result<u8, String> {
    let spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    let trace_path = trace.unwrap_or_else(|| default_trace_path(spec_path));
    if author == AuthorArg::Auto
        && spec.has_plain_steps()
        && matches!(
            flowproof_agent::HttpModelClient::from_env_result(),
            Ok(None)
        )
    {
        eprintln!(
            "WARNING: no authoring model is configured; plain steps will try deterministic grammar fallback"
        );
    }
    // Healing re-records the spec against the live app and diffs — so a
    // multi-surface flow heals with the same registry recording uses.
    let mut driver = record_driver(&spec)?;
    let mut report =
        match flowproof_agent::heal_with_author(&spec, &mut driver, &trace_path, author.into()) {
            Ok(report) => report,
            Err(flowproof_agent::HealError::Record(err)) if json => {
                if let Some(payload) = record_failure_json(&err) {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
                    );
                    return Ok(EXIT_ERROR);
                }
                return Err(err.to_string());
            }
            Err(err) => return Err(err.to_string()),
        };

    let mut applied = false;
    if apply && report.changed {
        if let Some(proposal) = &report.proposed_path {
            std::fs::copy(proposal, &trace_path).map_err(|e| e.to_string())?;
            std::fs::remove_file(proposal).map_err(|e| e.to_string())?;
            report.proposed_path = None;
            applied = true;
        }
    }

    if json {
        let payload = serde_json::json!({ "report": report, "applied": applied });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
        for decision in &report.routing {
            println!(
                "  [AUTHOR {}] {}: {}",
                decision.route, decision.step, decision.intent
            );
            if let Some(warning) = &decision.warning {
                eprintln!("WARNING: step {}: {warning}", decision.step);
            }
        }
        if !report.changed {
            println!("HEALTHY: {} — trace matches the live app", spec.name);
        } else {
            for change in &report.steps_changed {
                println!(
                    "  [CHANGED] {} {} ({})",
                    change.id,
                    change.intent,
                    change.fields.join(", ")
                );
            }
            if report.steps_added > 0 || report.steps_removed > 0 {
                println!(
                    "  steps added: {}, removed: {}",
                    report.steps_added, report.steps_removed
                );
            }
            if let Some(page) = &report.diff_html {
                println!("REVIEW: {} (before/after with frames)", page.display());
            }
            if applied {
                println!("APPLIED: {} updated in place", trace_path.display());
            } else if let Some(proposal) = &report.proposed_path {
                println!(
                    "PROPOSED: review {} then re-run with --apply",
                    proposal.display()
                );
            }
        }
    }
    Ok(if !report.changed || applied {
        EXIT_PASS
    } else {
        EXIT_FAIL
    })
}

/// Run the CLI against `args` (excluding the program name) and return the
/// process exit code. Never panics on user error.
pub fn run_cli<I, T>(args: I) -> u8
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    let cli = match Cli::try_parse_from(
        std::iter::once(std::ffi::OsString::from("flowproof"))
            .chain(args.into_iter().map(Into::into)),
    ) {
        Ok(cli) => cli,
        Err(e) => {
            // Clap handles --help/--version as "errors" with exit code 0.
            let code = if e.use_stderr() {
                EXIT_ERROR
            } else {
                EXIT_PASS
            };
            let _ = e.print();
            return code;
        }
    };

    let result = match cli.command {
        Command::Config { action } => match action {
            ConfigAction::Sap {
                user,
                password,
                client,
                language,
                connection,
            } => config::cmd_sap(
                config::SharedArgs {
                    user,
                    password,
                    client,
                    language,
                },
                connection,
            ),
            ConfigAction::Fiori {
                user,
                password,
                client,
                language,
                base_url,
            } => config::cmd_fiori(
                config::SharedArgs {
                    user,
                    password,
                    client,
                    language,
                },
                base_url,
            ),
            ConfigAction::Ai {
                provider,
                api_key,
                model,
                clear_api_key,
                clear_model,
            } => config::cmd_ai(config::AiArgs {
                provider,
                api_key,
                model,
                clear_api_key,
                clear_model,
            }),
            ConfigAction::Show => config::cmd_show(),
            ConfigAction::Path => config::cmd_path(),
            ConfigAction::Skill {
                claude,
                agents,
                dir,
                force,
            } => config::cmd_skill(claude, agents, dir, force),
        },
        Command::Record {
            spec,
            out,
            vars,
            var,
            json,
            author,
            reuse,
            verify,
            keep_open,
            headed,
            headless,
            recording_detail,
            video,
            highlight_cursor,
        } => with_headed_mode(resolve_headed(headed, headless, true), || {
            with_keep_browser_open(keep_open, || {
                cmd_record(
                    &spec,
                    RecordOptions {
                        out,
                        values: ValuesArgs {
                            vars_file: vars,
                            vars: var,
                        },
                        json,
                        author,
                        reuse,
                        verify,
                        recording: recording_options(recording_detail, video, highlight_cursor),
                    },
                )
            })
        }),
        Command::Run {
            spec,
            trace,
            vars,
            var,
            json,
            retries,
            record_missing,
            author,
            strict,
            keep_open,
            headed,
            headless,
            recording_detail,
            video,
            highlight_cursor,
        } => {
            if keep_open && spec.is_dir() {
                Err("--keep-open accepts one flow, not a suite directory".to_string())
            } else if keep_open && retries > 0 {
                Err("--keep-open cannot be combined with --retries; inspect one final run at a time"
                    .to_string())
            } else {
                let missing = if record_missing {
                    MissingTrace::Record
                } else if strict {
                    MissingTrace::Error
                } else {
                    MissingTrace::Skip
                };
                with_headed_mode(resolve_headed(headed, headless, false), || {
                    with_keep_browser_open(keep_open, || {
                        cmd_run(
                            &spec,
                            RunOptions {
                                trace,
                                json,
                                retries,
                                missing,
                                author,
                                values: ValuesArgs {
                                    vars_file: vars,
                                    vars: var,
                                },
                                recording: recording_options(
                                    recording_detail,
                                    video,
                                    highlight_cursor,
                                ),
                            },
                        )
                    })
                })
            }
        }
        Command::Audit {
            dir,
            json,
            run,
            since,
        } => cmd_audit(&dir, json, run, since),
        Command::Capture { port, out, json } => capture::cmd_capture(port, Some(out), json),
        Command::Doctor {
            agent,
            sap,
            fiori,
            ai,
            timeout,
            prompt,
        } => match (agent, sap, fiori, ai) {
            (Some(agent), false, false, false) => {
                agent_flow::cmd_doctor_agent(&agent, timeout, &prompt)
            }
            (None, true, false, false) => doctor::cmd_doctor_sap(),
            (None, false, true, false) => doctor::cmd_doctor_fiori(timeout),
            (None, false, false, true) => doctor::cmd_doctor_ai(),
            (None, false, false, false) => {
                Err("specify one of --agent <command>, --sap, --fiori, or --ai".to_string())
            }
            _ => unreachable!(
                "clap's conflicts_with_all on --agent/--sap/--fiori/--ai already rejects any other combination"
            ),
        },
        Command::AuthorFromDoc {
            doc,
            app,
            name,
            out,
        } => cmd_author_from_doc(doc, app, name, out),
        Command::Heal {
            spec,
            trace,
            apply,
            json,
            author,
        } => cmd_heal(&spec, trace, apply, json, author),
        // The stand-in speaks JSON-RPC on stdout, so it must print NOTHING
        // else there; any error goes to stderr and a non-zero exit, which
        // the orchestrator sees as a missing/short out file.
        Command::McpStdio { server } => {
            flowproof_adapters::mcp_stdio::run_stand_in(&server).map(|()| EXIT_PASS)
        }
    };
    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            EXIT_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A surface's `browser:` config stages on its freshly built driver —
    /// the window between construction and first-activation launch is the
    /// only place staging can land, and BOTH factories (record and replay)
    /// go through this one helper, so the two executions stage identically.
    #[test]
    fn a_surfaces_browser_config_stages_before_its_launch() {
        let setup: flowproof_trace::format::BrowserSetup = serde_yaml::from_str(
            "viewport:\n  width: 390\n  height: 844\n  mobile: true\n  touch: true\nuser_agent: fp-test\n",
        )
        .expect("setup parses");
        let mut mock = flowproof_driver::mock::MockAppDriver::new(&[]);
        stage_surface_browser(&mut mock, Some(&setup)).expect("stages");
        let staged = mock.staged_browser.expect("browser staged");
        let vp = staged.viewport.expect("viewport staged");
        assert_eq!((vp.width, vp.height), (390, 844));
        assert!(vp.mobile && vp.touch);
        assert_eq!(staged.user_agent.as_deref(), Some("fp-test"));
        // No config, no call: an empty setup must not disturb the driver.
        let mut untouched = flowproof_driver::mock::MockAppDriver::new(&[]);
        stage_surface_browser(&mut untouched, None).expect("no-op");
        assert!(untouched.staged_browser.is_none());
    }

    /// A multi-surface replay rebuilds its launch targets from the header
    /// alone — `${VAR}` config resolves at replay time from THIS run's
    /// environment, and a header missing what a surface needs errors
    /// naming the surface, not a bare unwrap.
    #[test]
    fn replay_surface_targets_resolve_from_the_header_alone() {
        let header: flowproof_trace::Header = serde_json::from_str(
            r#"{"format":"flowproof-trace","version":1,"trace_id":"t","recorded_at":"2026-08-05T00:00:00Z",
                "app":{"name":"multi","adapter":"multi"},
                "apps":{"gui":{"name":"sap","adapter":"sap-com","url":"${FP_TEST_SAP_CONN}"},
                        "portal":{"name":"web","adapter":"web","url":"https://portal.test/orders"}},
                "env":{"os":"macos","resolution":[1,1]}}"#,
        )
        .expect("header parses");
        std::env::set_var("FP_TEST_SAP_CONN", "S4 DEV");
        let targets = replay_surface_targets(&header).expect("targets resolve");
        assert_eq!(targets[0].0, "gui");
        assert_eq!(targets[0].1.command, "S4 DEV", "the ${{VAR}} resolved NOW");
        assert_eq!(targets[1].1.command, "https://portal.test/orders");

        let mut broken = header.clone();
        broken.apps.get_mut("portal").expect("portal").url = None;
        let err = replay_surface_targets(&broken).expect_err("web needs a url");
        std::env::remove_var("FP_TEST_SAP_CONN");
        assert!(
            err.contains("portal") && err.contains("no url"),
            "names the surface and the gap: {err}"
        );
    }

    /// A flow that engages egress records the tier it ACTUALLY ran under.
    /// Without this, an `egress` lane on a host with no containment reads
    /// exactly like one that was enforced and certified - the record would
    /// imply a certification the run never made, which is the whole reason
    /// the field exists.
    #[test]
    fn a_control_record_states_the_containment_tier_it_ran_under() {
        let dir = std::env::temp_dir().join("flowproof-containment-record-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec_path = dir.join("egress.flow.yaml");
        let spec = FlowSpec::parse(
            "name: contained\napp: agent\nagent:\n  command: ./agent\n  \
             allow_egress:\n    - api.example.com:443\n\
             control:\n  id: sec.egress.declared\n\
             steps:\n  - prompt: fetch the invoice\n",
        )
        .expect("spec parses");
        let record = build_control_record(
            &spec_path,
            &dir,
            &spec,
            flowproof_replay::ControlVerdict::Pass,
            None,
            None,
        )
        .expect("a control-bearing flow has a record");

        assert!(
            record.lanes.contains(&"egress".to_string()),
            "the flow asserted the egress lane: {record:?}"
        );
        let tier = record
            .containment
            .as_deref()
            .expect("a flow that engages egress must state its tier");
        // Whatever this host can do, the record says which it was, and the
        // wording matches the trace lane's tag exactly.
        if cfg!(target_os = "linux") {
            assert_eq!(tier, "enforced (linux seccomp)", "{record:?}");
        } else if cfg!(target_os = "windows") {
            assert!(
                tier.starts_with("not contained ("),
                "the pre-run Windows record must not predict enforcement: {tier}"
            );
            assert!(
                tier.contains("on Windows the tier is decided by the run"),
                "the record must explain that Windows reports the achieved run tier: {tier}"
            );
        } else {
            assert!(
                tier.starts_with("not contained (") && tier.contains("Linux"),
                "an unsupported host must say it is not contained and name Linux support: {tier}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Blocked destinations are read from the RECORDED trace, so they are
    /// evidence about THIS run only when this run was contained. A Linux
    /// recording replayed on a host without containment must not present
    /// destinations another machine blocked as proof for an uncontained run.
    #[test]
    fn blocked_evidence_is_dropped_when_this_run_was_not_contained() {
        let dir = std::env::temp_dir().join("flowproof-blocked-attribution-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec_path = dir.join("egress.flow.yaml");
        // A trace carrying a blocked lane, exactly as a contained Linux run
        // would have written it.
        let trace = default_trace_path(&spec_path);
        std::fs::write(
            &trace,
            serde_json::json!({
                "app": "agent",
                "mocks": {},
                "cassette": {"turns": []},
                "egress": {
                    "containment": "enforced (linux seccomp)",
                    "blocked": [{
                        "destination": "evil.example.com:443",
                        "protocol": "tcp",
                        "at_ms": 12
                    }]
                }
            })
            .to_string(),
        )
        .expect("trace written");
        let spec = FlowSpec::parse(
            "name: contained\napp: agent\nagent:\n  command: ./agent\n  \
             allow_egress:\n    - api.example.com:443\n\
             control:\n  id: sec.egress.declared\n\
             steps:\n  - prompt: fetch the invoice\n",
        )
        .expect("spec parses");
        // Prove the FIXTURE first. The platform assertion below is
        // "empty" off Linux, which a malformed trace would satisfy for the
        // wrong reason - and did: a missing `at_ms` made the lane fail to
        // parse, so this test passed on macOS while proving nothing.
        assert!(
            !agent_flow::egress_blocked(&trace).is_empty(),
            "the fixture must actually carry a blocked lane, or this test proves nothing"
        );

        let record = build_control_record(
            &spec_path,
            &dir,
            &spec,
            flowproof_replay::ControlVerdict::Pass,
            None,
            None,
        )
        .expect("a control-bearing flow has a record");

        if cfg!(target_os = "linux") {
            assert!(
                record
                    .evidence
                    .blocked
                    .iter()
                    .any(|b| b.contains("evil.example.com")),
                "a contained run keeps its blocked evidence: {record:?}"
            );
        } else {
            assert!(
                record.evidence.blocked.is_empty(),
                "an uncontained run must claim no blocked evidence: {record:?}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// The record follows the RUN, and getting that wrong costs evidence
    /// rather than only a label.
    ///
    /// `containment` is read twice in `build_control_record`: once as the
    /// record's own tier line, and once to decide whether the blocked lane is
    /// evidence at all. So the two tiers disagreeing does not merely mislabel
    /// the record - it decides whether the destinations a run refused are
    /// carried or discarded.
    ///
    /// # Why this asserts the PESSIMISTIC direction
    ///
    /// The interesting case in production is the opposite one: a Windows run
    /// that WAS contained, over a probe that predicted otherwise. That case
    /// cannot be made falsifiable here. This suite runs on Linux, where the
    /// probe already answers `Enforced`, so a test asserting "the achieved
    /// Enforced won" passes identically when the achieved tier is ignored
    /// altogether - it was mutation-checked, and it survived the mutation.
    /// Shipping it would have been a green tick that was never asked a
    /// question.
    ///
    /// So it runs the other way: an achieved `NotContained` over a probe
    /// saying `Enforced`. On Linux those genuinely differ, so passing can only
    /// mean the run's answer was preferred - and it pins the safety-critical
    /// half, which is that an uncontained run must not inherit an optimistic
    /// probe's evidence.
    #[test]
    fn a_run_that_was_not_contained_keeps_no_evidence_from_an_optimistic_probe() {
        let dir = std::env::temp_dir().join("flowproof-achieved-tier-record");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec_path = dir.join("contained.flow.yaml");
        let trace = default_trace_path(&spec_path);
        std::fs::write(
            &trace,
            serde_json::json!({
                "app": "agent",
                "mocks": {},
                "cassette": {"turns": []},
                "egress": {
                    "containment": "enforced (linux seccomp)",
                    "blocked": [{
                        "destination": "evil.example.com:443",
                        "protocol": "tcp",
                        "at_ms": 12
                    }]
                }
            })
            .to_string(),
        )
        .expect("trace written");
        let spec = FlowSpec::parse(
            "name: contained\napp: agent\nagent:\n  command: ./agent\n  \
             allow_egress:\n    - api.example.com:443\n\
             control:\n  id: sec.egress.declared\n\
             steps:\n  - prompt: fetch the invoice\n",
        )
        .expect("spec parses");
        assert!(
            !agent_flow::egress_blocked(&trace).is_empty(),
            "the fixture must actually carry a blocked lane, or this test proves nothing"
        );

        let achieved =
            flowproof_adapters::Containment::NotContained("the filters never installed".into());
        // The probe and the RUN must DISAGREE, or passing proves nothing.
        //
        // This used to require the probe to say `Enforced`, which is true
        // only on Linux - so the test failed outright on every other host
        // and made `cargo test --workspace` red before anyone had touched
        // anything. The disagreement is what matters, not which side of it
        // this machine happens to be on: off Linux the probe still says
        // "not contained", but for a DIFFERENT reason, and the record must
        // carry the run's reason rather than the probe's.
        assert_ne!(
            agent_flow::containment(&spec).report_line(),
            achieved.report_line(),
            "the probe and the run must differ, or this test proves nothing"
        );
        let record = build_control_record(
            &spec_path,
            &dir,
            &spec,
            flowproof_replay::ControlVerdict::Pass,
            None,
            Some(&achieved),
        )
        .expect("a control-bearing flow has a record");

        assert_eq!(
            record.containment.as_deref(),
            Some("not contained (the filters never installed)"),
            "the record must carry the tier the RUN achieved, not the one this \
             host would have predicted: {record:?}"
        );
        assert!(
            record.evidence.blocked.is_empty(),
            "a run that was NOT contained must claim no blocked evidence, however \
             optimistic the probe was: {record:?}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A flow that engages no egress claims no tier at all, rather than
    /// recording a "not contained" that was never in question.
    #[test]
    fn a_flow_that_engages_no_egress_claims_no_tier() {
        let dir = std::env::temp_dir().join("flowproof-no-tier-test");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec_path = dir.join("plain.flow.yaml");
        let spec = FlowSpec::parse(
            "name: plain\napp: agent\nagent:\n  command: ./agent\n\
             control:\n  id: sec.plain\n\
             steps:\n  - prompt: summarise\n",
        )
        .expect("spec parses");
        let record = build_control_record(
            &spec_path,
            &dir,
            &spec,
            flowproof_replay::ControlVerdict::Pass,
            None,
            None,
        )
        .expect("a control-bearing flow has a record");
        assert_eq!(record.containment, None, "{record:?}");
        assert!(!record.lanes.contains(&"egress".to_string()), "{record:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }

    #[test]
    fn default_values_path_mirrors_the_trace_path_convention() {
        let spec = Path::new("/tmp/example/display.flow.yaml");
        assert_eq!(
            default_values_path(spec),
            PathBuf::from("/tmp/example/display.values.yaml")
        );
    }

    #[test]
    fn values_context_loads_sibling_file_and_inline_overrides() {
        let dir =
            std::env::temp_dir().join(format!("flowproof-values-context-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec = dir.join("case.flow.yaml");
        std::fs::write(&spec, "name: case\napp: api\nsteps: []\n").expect("spec");
        std::fs::write(
            dir.join("case.values.yaml"),
            "FP_VALUES_A_MATERIAL: M-10092\nFP_VALUES_A_SUPPLIER: 45000031\nFP_VALUES_A_ACTIVE: true\n",
        )
        .expect("values");
        std::env::remove_var("FP_VALUES_A_MATERIAL");
        std::env::remove_var("FP_VALUES_A_SUPPLIER");
        std::env::remove_var("FP_VALUES_A_ACTIVE");
        {
            let _overlay = apply_values_context(
                &spec,
                &ValuesArgs {
                    vars_file: None,
                    vars: vec!["FP_VALUES_A_MATERIAL=M-99999".into()],
                },
            )
            .expect("values apply");
            assert_eq!(
                std::env::var("FP_VALUES_A_MATERIAL").as_deref(),
                Ok("M-99999")
            );
            assert_eq!(
                std::env::var("FP_VALUES_A_SUPPLIER").as_deref(),
                Ok("45000031")
            );
            assert_eq!(std::env::var("FP_VALUES_A_ACTIVE").as_deref(), Ok("true"));
        }
        assert!(
            std::env::var_os("FP_VALUES_A_MATERIAL").is_none(),
            "values overlay restores the caller env"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn explicit_values_file_overrides_the_default_sibling_file() {
        let dir =
            std::env::temp_dir().join(format!("flowproof-values-explicit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("temp dir");
        let spec = dir.join("case.flow.yaml");
        let explicit = dir.join("qa.values.yaml");
        std::fs::write(&spec, "name: case\napp: api\nsteps: []\n").expect("spec");
        std::fs::write(
            dir.join("case.values.yaml"),
            "FP_VALUES_B_MATERIAL: DEFAULT\n",
        )
        .expect("default");
        std::fs::write(&explicit, "FP_VALUES_B_MATERIAL: QA\n").expect("explicit");
        std::env::remove_var("FP_VALUES_B_MATERIAL");
        {
            let _overlay = apply_values_context(
                &spec,
                &ValuesArgs {
                    vars_file: Some(explicit),
                    vars: Vec::new(),
                },
            )
            .expect("values apply");
            assert_eq!(std::env::var("FP_VALUES_B_MATERIAL").as_deref(), Ok("QA"));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_sap_secret_detail_names_the_sap_config_command() {
        let suffix = step_detail_suffix("secret ${SAP_USER} is not set in the environment");
        assert!(
            suffix.contains("Run `flowproof config sap` to set it."),
            "{suffix}"
        );
        assert!(
            !suffix.contains("flowproof config fiori"),
            "SAP_* must not point at the Fiori profile: {suffix}"
        );
    }

    #[test]
    fn missing_fiori_secret_detail_names_the_fiori_config_command() {
        let suffix = step_detail_suffix("secret ${FIORI_USER} is not set in the environment");
        assert!(
            suffix.contains("Run `flowproof config fiori` to set it."),
            "{suffix}"
        );
        assert!(
            !suffix.contains("flowproof config sap"),
            "FIORI_* must not point at the SAP profile: {suffix}"
        );
    }

    #[test]
    fn unrelated_missing_secret_detail_gets_no_config_command() {
        let suffix = step_detail_suffix("secret ${MATERIAL} is not set in the environment");
        assert!(
            !suffix.contains("flowproof config"),
            "suite-minted data must not get credential-profile advice: {suffix}"
        );
    }

    #[test]
    fn keep_open_is_explicit_on_record_and_run() {
        let record =
            Cli::try_parse_from(["flowproof", "record", "insurance.flow.yaml", "--keep-open"])
                .expect("record flag parses");
        assert!(matches!(
            record.command,
            Command::Record {
                keep_open: true,
                ..
            }
        ));

        let run = Cli::try_parse_from(["flowproof", "run", "insurance.flow.yaml", "--keep-open"])
            .expect("run flag parses");
        assert!(matches!(
            run.command,
            Command::Run {
                keep_open: true,
                ..
            }
        ));

        let default = Cli::try_parse_from(["flowproof", "record", "insurance.flow.yaml"])
            .expect("default parses");
        assert!(matches!(
            default.command,
            Command::Record {
                keep_open: false,
                ..
            }
        ));

        assert!(
            Cli::try_parse_from([
                "flowproof",
                "record",
                "insurance.flow.yaml",
                "--keep-open",
                "--json",
            ])
            .is_err(),
            "interactive waiting and machine-readable output must not mix"
        );
    }

    #[test]
    fn keep_open_rejects_suites_and_retries_before_execution() {
        let dir = std::env::temp_dir().join("flowproof-keep-open-suite-test");
        std::fs::create_dir_all(&dir).expect("temp directory");
        assert_eq!(
            run_cli(["run", &dir.to_string_lossy(), "--keep-open"]),
            EXIT_ERROR,
            "a suite must not pause once per flow"
        );
        std::fs::remove_dir_all(&dir).ok();

        assert_eq!(
            run_cli([
                "run",
                "insurance.flow.yaml",
                "--keep-open",
                "--retries",
                "1",
            ]),
            EXIT_ERROR,
            "inspection is one final attempt, not every retry"
        );
    }

    #[test]
    fn record_defaults_headed_run_defaults_headless() {
        const KEY: &str = "FLOWPROOF_HEADED";
        let restore = std::env::var_os(KEY);
        std::env::remove_var(KEY);

        assert!(
            resolve_headed(false, false, true),
            "record's default is headed"
        );
        assert!(
            !resolve_headed(false, false, false),
            "run's default stays headless"
        );

        match restore {
            Some(value) => std::env::set_var(KEY, value),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn explicit_flag_overrides_ambient_env_and_default() {
        const KEY: &str = "FLOWPROOF_HEADED";
        let restore = std::env::var_os(KEY);
        std::env::set_var(KEY, "1");

        assert!(
            !resolve_headed(false, true, true),
            "--headless must win over an ambient FLOWPROOF_HEADED and a headed default"
        );
        assert!(
            resolve_headed(true, false, false),
            "--headed must win even against run's headless default"
        );

        match restore {
            Some(value) => std::env::set_var(KEY, value),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn headed_and_headless_flags_are_mutually_exclusive() {
        assert!(
            Cli::try_parse_from([
                "flowproof",
                "record",
                "insurance.flow.yaml",
                "--headed",
                "--headless",
            ])
            .is_err(),
            "record must reject contradictory visibility flags"
        );
        assert!(
            Cli::try_parse_from([
                "flowproof",
                "run",
                "insurance.flow.yaml",
                "--headed",
                "--headless",
            ])
            .is_err(),
            "run must reject contradictory visibility flags"
        );
    }

    #[test]
    fn headless_conflicts_with_keep_open() {
        assert!(
            Cli::try_parse_from([
                "flowproof",
                "run",
                "insurance.flow.yaml",
                "--headless",
                "--keep-open",
            ])
            .is_err(),
            "keep-open implies a visible browser, so --headless contradicts it"
        );
    }

    /// `--agent`, `--sap`, `--fiori`, and `--ai` are mutually exclusive on
    /// `doctor`, enforced by clap before any handler runs.
    #[test]
    fn doctor_agent_conflicts_with_sap() {
        assert!(
            Cli::try_parse_from(["flowproof", "doctor", "--agent", "./start-agent", "--sap"])
                .is_err(),
            "--agent and --sap answer different questions and must not combine"
        );
    }

    #[test]
    fn doctor_agent_conflicts_with_fiori() {
        assert!(Cli::try_parse_from([
            "flowproof",
            "doctor",
            "--agent",
            "./start-agent",
            "--fiori"
        ])
        .is_err());
    }

    #[test]
    fn doctor_sap_conflicts_with_fiori() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--sap", "--fiori"]).is_err());
    }

    #[test]
    fn doctor_ai_conflicts_with_the_other_modes() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--ai", "--sap"]).is_err());
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--ai", "--fiori"]).is_err());
        assert!(
            Cli::try_parse_from(["flowproof", "doctor", "--ai", "--agent", "./start-agent"])
                .is_err()
        );
    }

    /// The documented onboarding invocation
    /// (`docs/agent-testing.md:139`) must keep parsing exactly as it always
    /// has - `--agent` moved from required to optional to make room for
    /// `--sap`/`--fiori`, and that move must not disturb this.
    #[test]
    fn doctor_agent_alone_still_parses() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--agent", "./start-agent"]).is_ok());
    }

    #[test]
    fn doctor_sap_alone_parses() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--sap"]).is_ok());
    }

    #[test]
    fn doctor_fiori_alone_parses() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--fiori"]).is_ok());
    }

    #[test]
    fn doctor_ai_alone_parses() {
        assert!(Cli::try_parse_from(["flowproof", "doctor", "--ai"]).is_ok());
    }

    /// clap accepts `doctor` with none of the four - that's deliberate
    /// (there is no natural default among them), so the "pick one" error is
    /// `run_cli`'s job, not clap's. Covered separately below.
    #[test]
    fn doctor_with_none_of_the_four_parses_but_run_cli_rejects_it() {
        assert!(Cli::try_parse_from(["flowproof", "doctor"]).is_ok());
        assert_eq!(
            run_cli(["doctor"]),
            EXIT_ERROR,
            "run_cli must name the missing choice rather than panic or silently no-op"
        );
    }

    #[test]
    fn headed_mode_environment_is_scoped_to_the_command() {
        const KEY: &str = "FLOWPROOF_HEADED";
        let restore = std::env::var_os(KEY);

        std::env::remove_var(KEY);
        let visible_inside = with_headed_mode(true, || std::env::var_os(KEY).is_some());
        assert!(visible_inside, "the adapter sees the scoped flag");
        assert!(
            std::env::var_os(KEY).is_none(),
            "the flag is restored after"
        );

        std::env::set_var(KEY, "1");
        let hidden_inside = with_headed_mode(false, || std::env::var_os(KEY).is_some());
        assert!(
            !hidden_inside,
            "an explicit headless resolution must force-remove an ambient flag"
        );
        assert_eq!(
            std::env::var_os(KEY).as_deref(),
            Some(std::ffi::OsStr::new("1")),
            "the prior ambient value is restored after"
        );

        match restore {
            Some(value) => std::env::set_var(KEY, value),
            None => std::env::remove_var(KEY),
        }
    }

    #[test]
    fn keep_open_environment_is_scoped_to_the_command() {
        const KEY: &str = "FLOWPROOF_KEEP_BROWSER_OPEN";
        let restore = std::env::var_os(KEY);
        std::env::remove_var(KEY);
        let visible_inside = with_keep_browser_open(true, || std::env::var_os(KEY).is_some());
        assert!(visible_inside, "the adapter sees the scoped flag");
        assert!(
            std::env::var_os(KEY).is_none(),
            "the flag is restored after"
        );
        if let Some(value) = restore {
            std::env::set_var(KEY, value);
        }
    }

    #[test]
    fn cli_accepts_opt_in_video_with_low_detail() {
        let cli = Cli::try_parse_from([
            "flowproof",
            "run",
            "demo.flow.yaml",
            "--recording-detail",
            "low",
            "--video",
            "--highlight-cursor",
        ])
        .expect("recording controls parse");
        match cli.command {
            Command::Run {
                recording_detail,
                video,
                highlight_cursor,
                ..
            } => {
                assert_eq!(recording_detail, RecordingDetailArg::Low);
                assert!(video);
                assert!(highlight_cursor);
            }
            _ => panic!("expected run command"),
        }

        let default = Cli::try_parse_from(["flowproof", "run", "demo.flow.yaml"])
            .expect("default recording controls parse");
        assert!(matches!(default.command, Command::Run { video: false, .. }));
    }

    #[test]
    fn parse_env_lines_is_dotenv_ish_and_strict() {
        let pairs = parse_env_lines(
            "# minted by datamaker\nMATERIAL=100-100\n\nNET_PRICE=123.45\n  PLANT=1010\n",
        )
        .expect("well-formed output parses");
        assert_eq!(
            pairs,
            vec![
                ("MATERIAL".to_string(), "100-100".to_string()),
                ("NET_PRICE".to_string(), "123.45".to_string()),
                ("PLANT".to_string(), "1010".to_string()),
            ]
        );
        // Values are verbatim — an equals sign inside the value survives.
        let pairs = parse_env_lines("QUERY=a=b\n").expect("parses");
        assert_eq!(pairs[0].1, "a=b");

        let err = parse_env_lines("MATERIAL=1\nnot key value\n").expect_err("malformed fails");
        assert!(err.contains("line 2"), "names the line: {err}");
        let err = parse_env_lines("2BAD=x\n").expect_err("bad name fails");
        assert!(err.contains("invalid name"), "{err}");
    }

    #[test]
    fn record_failure_json_shapes_only_clarifications() {
        let c = flowproof_agent::Clarification {
            step: "make required field changes".into(),
            step_index: 3,
            stage: flowproof_agent::ClarifyStage::NoModel,
            reason: "no model backend".into(),
            capture_reference: None,
            capture_candidates: vec![],
            rules_error: Some("no rule matches".into()),
            completed_steps: vec![],
            scene: vec![],
            hint: flowproof_agent::Clarification::HINT.into(),
        };
        let err = flowproof_agent::RecordError::NeedsClarification(Box::new(c));
        let payload = record_failure_json(&err).expect("clarification is structured");
        assert_eq!(
            payload["needs_clarification"]["step"],
            "make required field changes"
        );
        assert_eq!(payload["needs_clarification"]["stage"], "no_model");

        let other = flowproof_agent::RecordError::UnknownApp("oracle".into());
        assert!(record_failure_json(&other).is_none());
    }

    #[test]
    fn order_specs_honors_the_manifest_then_falls_back_to_sorted() {
        let dir = Path::new("/suite");
        let mut specs = vec![
            PathBuf::from("/suite/z/last.flow.yaml"),
            PathBuf::from("/suite/a/unlisted.flow.yaml"),
            PathBuf::from("/suite/smoke/login.flow.yaml"),
        ];
        order_specs(
            &mut specs,
            dir,
            &[
                "smoke/login.flow.yaml".to_string(),
                "z/last.flow.yaml".to_string(),
            ],
        );
        assert_eq!(
            specs,
            vec![
                PathBuf::from("/suite/smoke/login.flow.yaml"), // listed 1st
                PathBuf::from("/suite/z/last.flow.yaml"),      // listed 2nd
                PathBuf::from("/suite/a/unlisted.flow.yaml"),  // unlisted, sorted after
            ]
        );
    }

    #[test]
    fn order_specs_is_a_noop_without_an_order() {
        let mut specs = vec![
            PathBuf::from("/s/b.flow.yaml"),
            PathBuf::from("/s/a.flow.yaml"),
        ];
        let before = specs.clone();
        order_specs(&mut specs, Path::new("/s"), &[]);
        assert_eq!(specs, before);
    }

    #[test]
    fn default_trace_path_strips_flow_suffix() {
        assert_eq!(
            default_trace_path(Path::new("flows/calc.flow.yaml")),
            PathBuf::from("flows/calc.trace.jsonl")
        );
        assert_eq!(
            default_trace_path(Path::new("other.yaml")),
            PathBuf::from("other.trace.jsonl")
        );
    }

    #[test]
    fn missing_trace_is_a_clean_error() {
        let code = run_cli(["run", "/nonexistent/calc.flow.yaml"]);
        assert_eq!(code, EXIT_ERROR);
    }

    #[test]
    fn help_exits_zero() {
        assert_eq!(run_cli(["--help"]), EXIT_PASS);
    }
}
