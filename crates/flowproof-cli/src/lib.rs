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
    },
    /// Deterministically replay a recorded flow (zero LLM calls).
    Run {
        /// Path to the YAML flow spec the trace was recorded from.
        spec: PathBuf,
        /// Trace file (default: the trace `record` wrote for this spec).
        #[arg(short, long)]
        trace: Option<PathBuf>,
        /// Emit the full report as JSON on stdout (for programmatic callers).
        #[arg(long)]
        json: bool,
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
/// `web`, the platform UIA driver otherwise.
pub fn driver_for(app: &str) -> Result<Box<dyn AppDriver>, String> {
    if app == "web" {
        let driver = flowproof_adapters::WebAppDriver::new().map_err(|e| e.to_string())?;
        Ok(Box::new(driver))
    } else {
        let driver = UiaAppDriver::new().map_err(|e| e.to_string())?;
        Ok(Box::new(driver))
    }
}

fn cmd_record(
    spec_path: &Path,
    out: Option<PathBuf>,
    json: bool,
    author: AuthorArg,
) -> Result<u8, String> {
    let spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    let out = out.unwrap_or_else(|| default_trace_path(spec_path));
    let mut driver = driver_for(&spec.app)?;
    let summary = flowproof_agent::record_with_author(&spec, &mut driver, &out, author.into())
        .map_err(|e| e.to_string())?;
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
        println!(
            "Recorded '{}': {} steps -> {}",
            spec.name,
            summary.steps,
            summary.trace_path.display()
        );
    }
    Ok(EXIT_PASS)
}

fn cmd_run(spec_path: &Path, trace: Option<PathBuf>, json: bool) -> Result<u8, String> {
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
    let mut driver = driver_for(&header.app.name)?;
    let (report, run_dir) =
        flowproof_replay::run_trace(&trace_path, &mut driver).map_err(|e| e.to_string())?;

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
            let (mark, suffix) = match step.status {
                StepStatus::Passed => ("PASS", String::new()),
                StepStatus::Failed => (
                    "FAIL",
                    step.detail
                        .as_deref()
                        .map(|d| format!(" — {d}"))
                        .unwrap_or_default(),
                ),
                StepStatus::Skipped => ("SKIP", String::new()),
            };
            println!("  [{mark}] {} {}{suffix}", step.id, step.intent);
        }
        println!(
            "{}: {} ({} ms) -> {}",
            if report.passed { "PASS" } else { "FAIL" },
            report.name,
            report.duration_ms,
            result_path.display()
        );
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
    let mut driver = driver_for(&spec.app)?;
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
        } => cmd_record(&spec, out, json, author),
        Command::Run { spec, trace, json } => cmd_run(&spec, trace, json),
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
