//! Run results and the JSON artifact written for each replay.

use std::path::{Path, PathBuf};

use flowproof_trace::format::Step;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Passed,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepResult {
    pub id: String,
    pub intent: String,
    pub status: StepStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub duration_ms: u64,
}

impl StepResult {
    pub fn passed(step: &Step, duration_ms: u64) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Passed,
            detail: None,
            duration_ms,
        }
    }

    pub fn failed(step: &Step, duration_ms: u64, reason: String) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Failed,
            detail: Some(reason),
            duration_ms,
        }
    }

    pub fn skipped(step: &Step) -> Self {
        Self {
            id: step.id.clone(),
            intent: step.intent.clone(),
            status: StepStatus::Skipped,
            detail: Some("previous step failed".into()),
            duration_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunReport {
    pub name: String,
    pub trace_id: String,
    pub passed: bool,
    pub steps: Vec<StepResult>,
    pub duration_ms: u64,
}

impl RunReport {
    /// Write `result.json` into a fresh run directory under
    /// `<base>/.flowproof/runs/<timestamp>/` and return the file path.
    pub fn write(&self, base: &Path) -> std::io::Result<PathBuf> {
        let run_id = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let dir = base.join(".flowproof").join("runs").join(run_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("result.json");
        std::fs::write(&path, serde_json::to_string_pretty(self)?)?;
        Ok(path)
    }
}
