//! `flowproof` CLI logic, exposed as a library so both the Rust binary and
//! the Python entry point (via PyO3) share one implementation.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use flowproof_agent::FlowSpec;
use flowproof_driver::{AppDriver, UiaAppDriver};
use flowproof_replay::StepStatus;

/// Process exit codes: 0 = pass, 1 = test failure, 2 = error.
pub const EXIT_PASS: u8 = 0;
pub const EXIT_FAIL: u8 = 1;
pub const EXIT_ERROR: u8 = 2;

#[derive(Parser)]
#[command(name = "flowproof", version, about)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Authoring backend selection for record/heal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
enum AuthorArg {
    /// Rules first, model fallback for steps the rules cannot resolve.
    #[default]
    Auto,
    /// Deterministic rules only.
    Rules,
    /// Model for every step.
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

#[derive(Subcommand)]
enum Command {
    /// Record a flow from a YAML spec: perform it once against the live app
    /// and write a deterministic trace next to the spec.
    Record {
        /// Path to the YAML flow spec (e.g. calc.flow.yaml).
        spec: PathBuf,
        /// Output trace file (default: <spec>.trace.jsonl next to the spec).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Emit the result as JSON on stdout (for programmatic callers).
        #[arg(long)]
        json: bool,
        /// Authoring backend: rules, llm, or auto (rules with llm fallback).
        #[arg(long, value_enum, default_value_t)]
        author: AuthorArg,
        /// Incremental re-record: reuse every old step whose target still
        /// resolves; re-author only what drifted (needs an existing trace).
        #[arg(long)]
        reuse: bool,
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
        /// Suite runs only: a missing trace is a hard error (pre-0.2.2
        /// behavior) instead of a skipped flow. For CI that must not let
        /// coverage silently shrink. Single-spec runs always error.
        #[arg(long, conflicts_with = "record_missing")]
        strict: bool,
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
        /// Authoring backend: rules, llm, or auto (rules with llm fallback).
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

fn cmd_record(
    spec_path: &Path,
    out: Option<PathBuf>,
    json: bool,
    author: AuthorArg,
    reuse: bool,
) -> Result<u8, String> {
    let mut spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    // The suite's data (env_from) and env govern recording too — the
    // ${VAR}s a spec references must resolve the same here as in `run`.
    let manifest = apply_suite_context(spec_path)?;
    // Suite-level browser defaults apply only when the spec has none —
    // recording bakes the result into the trace header.
    if spec.browser.is_none() {
        spec.browser = manifest.and_then(|m| m.browser);
    }
    if let Some(reason) = spec.skip_reason() {
        if json {
            println!("{}", serde_json::json!({ "skipped": reason }));
        } else {
            println!("[SKIP] {} ({reason})", spec.name);
        }
        return Ok(EXIT_PASS);
    }
    let out = out.unwrap_or_else(|| default_trace_path(spec_path));
    let mut driver = driver_for(spec.app.id())?;
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
        Some(steps) => {
            flowproof_agent::record_incremental(&spec, &mut driver, &out, author.into(), steps)
        }
        None => flowproof_agent::record_with_author(&spec, &mut driver, &out, author.into()),
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
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).map_err(|e| e.to_string())?
        );
    } else {
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
    Ok(EXIT_PASS)
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
/// fail the suite. Returns the first passing report, else the last
/// failure, with the attempt count.
fn replay_with_retries(
    trace_path: &Path,
    app_name: &str,
    retries: u8,
    announce: bool,
) -> Result<(flowproof_replay::RunReport, PathBuf, u32), String> {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut driver = driver_for(app_name)?;
        let (report, run_dir) =
            flowproof_replay::run_trace(trace_path, &mut driver).map_err(|e| e.to_string())?;
        if report.passed || attempt > u32::from(retries) {
            return Ok((report, run_dir, attempt));
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

/// Parse a data command's stdout into env pairs. Dotenv-ish and strict:
/// blank lines and `#` comments are skipped; everything else must be
/// `NAME=VALUE` with a `${VAR}`-legal name; the value is taken verbatim
/// (no quote stripping). Anything else is an error naming the line —
/// running flows against half-seeded data is the failure mode to prevent.
fn parse_env_lines(stdout: &str) -> Result<Vec<(String, String)>, String> {
    let valid_name = |name: &str| {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
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
        if !valid_name(name) {
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
pub fn apply_suite_context(
    spec_path: &Path,
) -> Result<Option<flowproof_agent::SuiteManifest>, String> {
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
    suite_browser: Option<&flowproof_trace::format::BrowserSetup>,
) -> Result<(), String> {
    let mut spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    if spec.browser.is_none() {
        spec.browser = suite_browser.cloned();
    }
    let mut driver = driver_for(spec.app.id())?;
    flowproof_agent::record(&spec, &mut driver, out)
        .map(|_| ())
        .map_err(|e| e.to_string())
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

pub fn run_suite(dir: &Path, json: bool, retries: u8, missing: MissingTrace) -> Result<u8, String> {
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

    let mut reports: Vec<flowproof_replay::RunReport> = Vec::new();
    let mut flows = Vec::new();
    for spec_path in &specs {
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
                    if let Err(e) = record_one(spec_path, &trace_path, manifest.browser.as_ref()) {
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
                    }));
                    reports.push(report);
                    continue;
                }
            }
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
        let replayed = flowproof_replay::load_trace(&trace_path)
            .map_err(|e| e.to_string())
            // A fresh driver per flow: full isolation, like Playwright
            // contexts. A driver fault here ends THIS flow only.
            .and_then(|(header, _)| {
                replay_with_retries(&trace_path, &header.app.name, retries, !json)
            });
        // Cleanup always runs, pass, fail or error.
        let cleanup = match &manifest.after_each {
            Some(cmd) => run_hook(cmd, spec_path, "after_each"),
            None => Ok(()),
        };
        // Replay first, cleanup second, and only then decide the outcome:
        // whichever failed, the cleanup has already run.
        let (report, run_dir, attempts) = match replayed.and_then(|triple| cleanup.map(|()| triple))
        {
            Ok(triple) => triple,
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

fn cmd_run(
    spec_path: &Path,
    trace: Option<PathBuf>,
    json: bool,
    retries: u8,
    missing: MissingTrace,
) -> Result<u8, String> {
    if spec_path.is_dir() {
        return run_suite(spec_path, json, retries, missing);
    }
    // A single flow gets its suite's env/data too — replay resolves ${VAR}
    // at moment-of-use, so the same values must be present as at record.
    apply_suite_context(spec_path)?;
    // Load the spec for its gate (this also surfaces spec parse errors on
    // single runs, deliberately — a typo'd spec should not replay).
    let spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    if let Some(reason) = spec.skip_reason() {
        let report = flowproof_replay::RunReport::skipped(&spec.name, &reason);
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
    if !trace_path.exists() {
        return Err(format!(
            "trace {} not found — run `flowproof record {}` first",
            trace_path.display(),
            spec_path.display()
        ));
    }
    // Peek the header to pick the right driver for the recorded app.
    let (header, _) = flowproof_replay::load_trace(&trace_path).map_err(|e| e.to_string())?;
    let (report, run_dir, _attempts) =
        replay_with_retries(&trace_path, &header.app.name, retries, !json)?;

    let result_path = report.write_into(&run_dir).map_err(|e| e.to_string())?;

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
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default(),
                ),
                StepStatus::Skipped => ("SKIP", String::new()),
                StepStatus::Errored => (
                    "ERROR",
                    step.detail
                        .as_deref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default(),
                ),
            };
            if step.degraded {
                let tier = step.selector_tier.as_deref().unwrap_or("fallback");
                suffix.push_str(&format!(" (matched via {tier} fallback)"));
            }
            println!("  [{mark}] {} {}{suffix}", step.id, step.intent);
        }
        println!(
            "{}: {} ({} ms) -> {}",
            if report.passed { "PASS" } else { "FAIL" },
            report.name,
            report.duration_ms,
            result_path.display()
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

fn cmd_heal(
    spec_path: &Path,
    trace: Option<PathBuf>,
    apply: bool,
    json: bool,
    author: AuthorArg,
) -> Result<u8, String> {
    let spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    let trace_path = trace.unwrap_or_else(|| default_trace_path(spec_path));
    let mut driver = driver_for(spec.app.id())?;
    let mut report =
        flowproof_agent::heal_with_author(&spec, &mut driver, &trace_path, author.into())
            .map_err(|e| e.to_string())?;

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
    } else if !report.changed {
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
        Command::Record {
            spec,
            out,
            json,
            author,
            reuse,
        } => cmd_record(&spec, out, json, author, reuse),
        Command::Run {
            spec,
            trace,
            json,
            retries,
            record_missing,
            strict,
        } => {
            let missing = if record_missing {
                MissingTrace::Record
            } else if strict {
                MissingTrace::Error
            } else {
                MissingTrace::Skip
            };
            cmd_run(&spec, trace, json, retries, missing)
        }
        Command::Heal {
            spec,
            trace,
            apply,
            json,
            author,
        } => cmd_heal(&spec, trace, apply, json, author),
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

    #[test]
    fn cli_definition_is_valid() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
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
