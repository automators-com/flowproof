//! `flowproof` — record a flow, replay it deterministically, or heal a
//! broken trace with a reviewable diff. All logic lives in the library so
//! the Python entry point shares it.

use std::process::ExitCode;

fn main() -> ExitCode {
    ExitCode::from(flowproof_cli::run_cli(std::env::args_os().skip(1)))
}
