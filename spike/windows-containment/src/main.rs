//! Entry point for both halves of the spike.
//!
//! One binary plays three roles, because `CreateProcessAsUserW` needs an image
//! the run identity can execute and re-using this one avoids shipping a second
//! artifact into a directory whose ACLs would then also need getting right:
//!
//!   * `run-all`    — the whole spike; what the test invokes as a subprocess
//!   * `canary`     — runs inside the boundary and probes the network
//!   * `hold`       — sets a boundary up and blocks, to be killed abruptly
//!   * `gui-inside` — days 7–9: drives a GUI app from inside the boundary

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("canary") => wfp_spike::canary::run(&args[1..]),
        #[cfg(windows)]
        Some("run-all") => wfp_spike::win::harness::run_all(),
        #[cfg(windows)]
        Some("hold") => wfp_spike::win::harness::hold(),
        #[cfg(windows)]
        Some("gui-inside") => wfp_spike::win::gui::inside(&args[1..]),
        other => {
            eprintln!("wfp-spike: unknown mode {other:?}; expected run-all|canary|hold|gui-inside");
            2
        }
    };
    std::process::exit(code);
}
