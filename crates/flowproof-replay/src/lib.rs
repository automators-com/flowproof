//! Deterministic replay of recorded traces. No LLM calls happen here, ever:
//! replay walks the selector ladder recorded in the trace and fails with a
//! structured report when a step cannot be resolved. Healing (which may call
//! a model) is a separate, explicit workflow that produces a reviewable diff.

use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("replay is not implemented yet")]
    NotImplemented,
    #[error("trace error: {0}")]
    Trace(String),
    #[error("driver error: {0}")]
    Driver(#[from] flowproof_driver::DriverError),
}

/// Outcome of a replay run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayReport {
    pub steps_total: usize,
    pub steps_passed: usize,
}

/// Deterministic executor for a single trace file.
#[derive(Debug, Default)]
pub struct Replayer {
    _private: (),
}

impl Replayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay the trace at `path` against the live application.
    pub fn run(&mut self, path: &Path) -> Result<ReplayReport, ReplayError> {
        let _ = path;
        Err(ReplayError::NotImplemented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_is_stubbed() {
        let mut replayer = Replayer::new();
        let result = replayer.run(Path::new("does-not-exist.jsonl"));
        assert!(matches!(result, Err(ReplayError::NotImplemented)));
    }
}
