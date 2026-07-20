//! The `flowproof._native` Python extension module. Thin bindings over the
//! Rust engine.
//!
//! Every function returns structured data as a JSON string that the Python
//! layer parses into typed objects — the caller is expected to be a program
//! (usually an agent), never a human parsing stdout. `cli_main` reuses the
//! same library code for the `flowproof` console script.

use std::path::PathBuf;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use flowproof_agent::FlowSpec;

fn runtime_err(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}

fn to_json(value: &serde_json::Value) -> PyResult<String> {
    serde_json::to_string(value).map_err(runtime_err)
}

/// Run the flowproof CLI with `args` (excluding the program name) and
/// return the process exit code (0 pass, 1 fail, 2 error).
#[pyfunction]
fn cli_main(py: Python<'_>, args: Vec<String>) -> PyResult<u8> {
    // The engine drives a UI and blocks; let other Python threads run.
    Ok(py.detach(|| flowproof_cli::run_cli(args)))
}

/// Record `spec`. Returns JSON: `{"trace_path": …, "steps": …}` on success,
/// or `{"needs_clarification": …}` when a step could not be authored — the
/// payload carries the stuck step and the live-screen inventory so the
/// calling agent can rewrite the step and re-record. Only genuine execution
/// errors raise.
#[pyfunction]
#[pyo3(signature = (spec, out=None))]
fn record(py: Python<'_>, spec: PathBuf, out: Option<PathBuf>) -> PyResult<String> {
    py.detach(|| {
        let parsed = FlowSpec::load(&spec).map_err(runtime_err)?;
        let out = out.unwrap_or_else(|| flowproof_cli::default_trace_path(&spec));
        let mut driver = flowproof_cli::driver_for(&parsed.app).map_err(runtime_err)?;
        match flowproof_agent::record(&parsed, &mut driver, &out) {
            Ok(summary) => to_json(&serde_json::json!({
                "trace_path": summary.trace_path,
                "steps": summary.steps,
            })),
            Err(flowproof_agent::RecordError::NeedsClarification(c)) => {
                // Ambiguity is data for the driving agent, not an exception.
                to_json(&serde_json::json!({ "needs_clarification": c }))
            }
            Err(err) => Err(runtime_err(err)),
        }
    })
}

/// Replay the trace recorded for `spec`. Returns JSON:
/// `{"report": <RunReport>, "report_path": …}`. Raises RuntimeError only
/// when the run cannot execute at all; test failures are data, not errors.
#[pyfunction]
#[pyo3(signature = (spec, trace=None))]
fn run(py: Python<'_>, spec: PathBuf, trace: Option<PathBuf>) -> PyResult<String> {
    py.detach(|| {
        let trace_path = trace.unwrap_or_else(|| flowproof_cli::default_trace_path(&spec));
        let (header, _) = flowproof_replay::load_trace(&trace_path).map_err(runtime_err)?;
        let mut driver = flowproof_cli::driver_for(&header.app.name).map_err(runtime_err)?;
        let (report, run_dir) =
            flowproof_replay::run_trace(&trace_path, &mut driver).map_err(runtime_err)?;
        let report_path = report.write_into(&run_dir).map_err(runtime_err)?;
        to_json(&serde_json::json!({
            "report": report,
            "report_path": report_path,
        }))
    })
}

/// Load a recorded trace for inspection. `path` may be the flow spec (the
/// default trace next to it is used) or a `.jsonl` trace file directly.
/// Returns JSON: `{"header": …, "steps": […]}`.
#[pyfunction]
fn get_trace(py: Python<'_>, path: PathBuf) -> PyResult<String> {
    py.detach(|| {
        let trace_path = if path.extension().is_some_and(|e| e == "jsonl") {
            path
        } else {
            flowproof_cli::default_trace_path(&path)
        };
        let (header, steps) = flowproof_replay::load_trace(&trace_path).map_err(runtime_err)?;
        to_json(&serde_json::json!({
            "header": header,
            "steps": steps,
        }))
    })
}

/// Re-author the flow and propose a trace diff. Returns JSON:
/// `{"report": <HealReport>, "applied": bool}`. Only replaces the trace
/// when `apply` is explicitly true and changes were found.
#[pyfunction]
#[pyo3(signature = (spec, trace=None, apply=false))]
fn heal(py: Python<'_>, spec: PathBuf, trace: Option<PathBuf>, apply: bool) -> PyResult<String> {
    py.detach(|| {
        let parsed = FlowSpec::load(&spec).map_err(runtime_err)?;
        let trace_path = trace.unwrap_or_else(|| flowproof_cli::default_trace_path(&spec));
        let mut driver = flowproof_cli::driver_for(&parsed.app).map_err(runtime_err)?;
        let mut report =
            flowproof_agent::heal(&parsed, &mut driver, &trace_path).map_err(runtime_err)?;
        let mut applied = false;
        if apply && report.changed {
            if let Some(proposal) = &report.proposed_path {
                std::fs::copy(proposal, &trace_path).map_err(runtime_err)?;
                std::fs::remove_file(proposal).map_err(runtime_err)?;
                report.proposed_path = None;
                applied = true;
            }
        }
        to_json(&serde_json::json!({
            "report": report,
            "applied": applied,
        }))
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    m.add_function(wrap_pyfunction!(record, m)?)?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add_function(wrap_pyfunction!(get_trace, m)?)?;
    m.add_function(wrap_pyfunction!(heal, m)?)?;
    m.add("__engine_version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
