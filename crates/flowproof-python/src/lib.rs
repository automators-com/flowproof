//! The `flowproof._native` Python extension module. Thin bindings over the
//! Rust engine: the CLI entry point plus programmatic record/run.

use std::path::PathBuf;

use pyo3::exceptions::PyRuntimeError;
use pyo3::prelude::*;

use flowproof_agent::FlowSpec;
use flowproof_driver::UiaAppDriver;

fn runtime_err(message: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(message.to_string())
}

/// Run the flowproof CLI with `args` (excluding the program name) and
/// return the process exit code (0 pass, 1 fail, 2 error).
#[pyfunction]
fn cli_main(py: Python<'_>, args: Vec<String>) -> PyResult<u8> {
    // The engine drives a UI and blocks; let other Python threads run.
    Ok(py.detach(|| flowproof_cli::run_cli(args)))
}

/// Record `spec` and return the trace path.
#[pyfunction]
#[pyo3(signature = (spec, out=None))]
fn record(py: Python<'_>, spec: PathBuf, out: Option<PathBuf>) -> PyResult<PathBuf> {
    py.detach(|| {
        let parsed = FlowSpec::load(&spec).map_err(runtime_err)?;
        let out = out.unwrap_or_else(|| flowproof_cli::default_trace_path(&spec));
        let mut driver = UiaAppDriver::new().map_err(runtime_err)?;
        let summary = flowproof_agent::record(&parsed, &mut driver, &out).map_err(runtime_err)?;
        Ok(summary.trace_path)
    })
}

/// Replay the trace recorded for `spec`; returns True on pass, False on
/// fail. Raises RuntimeError when the run cannot execute at all.
#[pyfunction]
#[pyo3(signature = (spec, trace=None))]
fn run(py: Python<'_>, spec: PathBuf, trace: Option<PathBuf>) -> PyResult<bool> {
    py.detach(|| {
        let trace_path = trace.unwrap_or_else(|| flowproof_cli::default_trace_path(&spec));
        let mut driver = UiaAppDriver::new().map_err(runtime_err)?;
        let report = flowproof_replay::run_trace(&trace_path, &mut driver).map_err(runtime_err)?;
        if let Some(base) = trace_path.parent() {
            report.write(base).map_err(runtime_err)?;
        }
        Ok(report.passed)
    })
}

#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(cli_main, m)?)?;
    m.add_function(wrap_pyfunction!(record, m)?)?;
    m.add_function(wrap_pyfunction!(run, m)?)?;
    m.add("__engine_version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
