//! The spike's only real output: a greppable block of observed facts.
//!
//! Every line is prefixed `SPIKE|` so a CI log can be reduced to the evidence
//! with one grep. Nothing here decides anything — an assertion records what was
//! *observed* alongside what was *expected*, and the reader compares them. That
//! separation is deliberate: honesty rule 8 says report the tier, never infer
//! it, and a helper that collapsed observation into a boolean would be exactly
//! the inference being banned.

use std::fmt::Display;

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
    pub fn note(&self, key: &str, value: impl Display) {
        println!("SPIKE|NOTE|{key}|{value}");
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
        println!(
            "SPIKE|ASSERT|{}|{}|expected={}|observed={}",
            o.id,
            if met { "MET" } else { "NOT-MET" },
            o.expected,
            o.observed
        );
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
        println!(
            "SPIKE|ASSERT|{}|NOT-RUN|expected={}|observed={}",
            o.id, o.expected, o.observed
        );
        self.obs.push(o);
    }

    pub fn summary(&self) {
        let met = self.obs.iter().filter(|o| o.met == Some(true)).count();
        let unmet = self.obs.iter().filter(|o| o.met == Some(false)).count();
        let skipped = self.obs.iter().filter(|o| o.met.is_none()).count();
        println!("SPIKE|SUMMARY|met={met}|not_met={unmet}|not_run={skipped}");
        for o in &self.obs {
            let tag = match o.met {
                Some(true) => "MET",
                Some(false) => "NOT-MET",
                None => "NOT-RUN",
            };
            println!("SPIKE|SUMMARY-ROW|{}|{}|{}", o.id, tag, o.observed);
        }
    }
}
