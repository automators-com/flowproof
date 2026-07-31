//! The spike's only real output: a greppable block of observed facts.
//!
//! Every line is prefixed `SPIKE|` so a CI log can be reduced to the evidence
//! with one grep. Nothing here decides anything — an assertion records what was
//! *observed* alongside what was *expected*, and the reader compares them. That
//! separation is deliberate: honesty rule 8 says report the tier, never infer
//! it, and a helper that collapsed observation into a boolean would be exactly
//! the inference being banned.

use std::fmt::Display;
use std::io::Write;

/// Write one evidence line straight to the stderr file descriptor.
///
/// **Not `println!`.** `cargo test` captures the `print!`/`eprint!` macro path
/// for a test that passes, and this spike's test always passes on purpose — so
/// the first Windows CI run produced `test windows_egress_containment_spike ...
/// ok` and not one line of the evidence it exists to produce. libtest's capture
/// works by swapping the sink those macros consult; a handle obtained from
/// `io::stderr()` writes to the descriptor directly and is not intercepted.
///
/// The alternatives were worse: `--nocapture` and `RUST_TEST_NOCAPTURE` both
/// live in `.github/workflows/`, which this spike may not modify, and setting
/// the variable in `.cargo/config.toml` would make every other crate's tests
/// noisy to fix one crate's problem.
pub fn emit(line: &str) {
    let mut e = std::io::stderr();
    let _ = e.write_all(line.as_bytes());
    let _ = e.write_all(b"\n");
    let _ = e.flush();
}

/// One recorded observation.
pub struct Obs {
    pub id: String,
    pub expected: String,
    pub observed: String,
    /// `None` when the probe never ran, which is distinct from failing.
    pub met: Option<bool>,
}

#[derive(Default)]
pub struct Report {
    obs: Vec<Obs>,
}

impl Report {
    pub fn new() -> Self {
        Self::default()
    }

    /// A free-form fact worth having in the log but not itself an assertion —
    /// an error code, a SID, a filter id. These are what make the *next*
    /// iteration cheap, since a missing log line costs a full CI cycle.
    pub fn note(&self, key: impl Display, value: impl Display) {
        emit(&format!("SPIKE|NOTE|{key}|{value}"));
    }

    /// Record an assertion. `met` is computed by the caller from a value only a
    /// real result could produce, never from "the API returned success".
    pub fn assert_obs(
        &mut self,
        id: impl Display,
        expected: impl Display,
        observed: impl Display,
        met: bool,
    ) {
        let o = Obs {
            id: id.to_string(),
            expected: expected.to_string(),
            observed: observed.to_string(),
            met: Some(met),
        };
        emit(&format!(
            "SPIKE|ASSERT|{}|{}|expected={}|observed={}",
            o.id,
            if met { "MET" } else { "NOT-MET" },
            o.expected,
            o.observed
        ));
        self.obs.push(o);
    }

    /// The probe could not run. Recorded as its own outcome so a skipped probe
    /// can never be read as a pass.
    pub fn not_run(&mut self, id: impl Display, expected: impl Display, why: impl Display) {
        let o = Obs {
            id: id.to_string(),
            expected: expected.to_string(),
            observed: format!("NOT RUN: {why}"),
            met: None,
        };
        emit(&format!(
            "SPIKE|ASSERT|{}|NOT-RUN|expected={}|observed={}",
            o.id, o.expected, o.observed
        ));
        self.obs.push(o);
    }

    pub fn summary(&self) {
        let met = self.obs.iter().filter(|o| o.met == Some(true)).count();
        let unmet = self.obs.iter().filter(|o| o.met == Some(false)).count();
        let skipped = self.obs.iter().filter(|o| o.met.is_none()).count();
        emit(&format!(
            "SPIKE|SUMMARY|met={met}|not_met={unmet}|not_run={skipped}"
        ));
        for o in &self.obs {
            let tag = match o.met {
                Some(true) => "MET",
                Some(false) => "NOT-MET",
                None => "NOT-RUN",
            };
            emit(&format!(
                "SPIKE|SUMMARY-ROW|{}|{}|{}",
                o.id, tag, o.observed
            ));
        }
    }
}
