//! `flowproof` CLI logic, exposed as a library so both the Rust binary and
//! the Python entry point (via PyO3) share one implementation.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use flowproof_agent::FlowSpec;
use flowproof_driver::UiaAppDriver;
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
    /// Propose a reviewable fix for a trace that no longer replays.
    Heal {
        /// Path to the YAML flow spec.
        spec: PathBuf,
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

fn cmd_record(spec_path: &Path, out: Option<PathBuf>, json: bool) -> Result<u8, String> {
    let spec = FlowSpec::load(spec_path).map_err(|e| e.to_string())?;
    let out = out.unwrap_or_else(|| default_trace_path(spec_path));
    let mut driver = UiaAppDriver::new().map_err(|e| e.to_string())?;
    let summary = flowproof_agent::record(&spec, &mut driver, &out).map_err(|e| e.to_string())?;
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
    let mut driver = UiaAppDriver::new().map_err(|e| e.to_string())?;
    let report =
        flowproof_replay::run_trace(&trace_path, &mut driver).map_err(|e| e.to_string())?;

    let artifacts_base = trace_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let result_path = report.write(&artifacts_base).map_err(|e| e.to_string())?;

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
        Command::Record { spec, out, json } => cmd_record(&spec, out, json),
        Command::Run { spec, trace, json } => cmd_run(&spec, trace, json),
        Command::Heal { spec } => Err(format!(
            "`flowproof heal` is not implemented yet (spec: {})",
            spec.display()
        )),
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
