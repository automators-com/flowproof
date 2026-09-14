//! Explicit suite selection and prerequisite policy. Legacy discovery is unchanged.
use std::path::{Component, Path, PathBuf};

use flowproof_agent::SuiteManifest;
use flowproof_replay::RunReport;

fn relative_path(value: &str) -> Result<&Path, String> {
    let path = Path::new(value);
    if value.is_empty()
        || !value.ends_with(".flow.yaml")
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || value.contains('\\')
    {
        return Err(format!(
            "suite flow `{value}` must be a relative *.flow.yaml path without traversal"
        ));
    }
    Ok(path)
}

pub(crate) fn select_specs(
    specs: &mut Vec<PathBuf>,
    dir: &Path,
    manifest: &SuiteManifest,
) -> Result<(), String> {
    if let Some(flows) = &manifest.flows {
        if flows.is_empty() || !manifest.order.is_empty() {
            return Err(
                "suite `flows` must be nonempty and cannot be combined with `order`".into(),
            );
        }
        let root = dir.canonicalize().map_err(|e| e.to_string())?;
        let mut selected = Vec::new();
        let mut identities = std::collections::BTreeSet::new();
        for value in flows {
            let path = dir.join(relative_path(value)?);
            if !specs.contains(&path)
                || !path
                    .canonicalize()
                    .map_err(|e| e.to_string())?
                    .starts_with(&root)
            {
                return Err(format!(
                    "selected flow `{value}` is missing or outside the suite"
                ));
            }
            if !identities.insert(path.canonicalize().map_err(|e| e.to_string())?) {
                return Err(format!(
                    "suite flow `{value}` is selected more than once (including aliases)"
                ));
            }
            selected.push(path);
        }
        *specs = selected;
    }
    let index = |name: &str| -> Result<usize, String> {
        let path = dir.join(relative_path(name)?);
        specs
            .iter()
            .position(|p| *p == path)
            .ok_or_else(|| format!("dependency flow `{name}` is not selected in this suite"))
    };
    for (flow, dependencies) in &manifest.depends_on {
        let position = index(flow)?;
        for dependency in dependencies {
            if index(dependency)? >= position {
                return Err(format!("dependency `{dependency}` must precede `{flow}`; reorder the suite and remove cycles"));
            }
        }
    }
    Ok(())
}

pub(crate) fn blocked_reason(
    spec: &Path,
    dir: &Path,
    manifest: &SuiteManifest,
    specs: &[PathBuf],
    reports: &[RunReport],
) -> Option<String> {
    if manifest.stop_on_failure && reports.iter().any(|r| !r.passed) {
        return Some("suite stopped after a previous flow failed".into());
    }
    let relative = spec.strip_prefix(dir).ok()?;
    let dependencies = manifest
        .depends_on
        .iter()
        .find(|(name, _)| Path::new(name) == relative)?
        .1;
    for dependency in dependencies {
        let passed = specs.iter().zip(reports).any(|(path, report)| {
            *path == dir.join(dependency) && report.passed && report.trace_id != "skipped"
        });
        if !passed {
            return Some(format!(
                "prerequisite `{dependency}` did not pass; dependent flow was not executed"
            ));
        }
    }
    None
}
