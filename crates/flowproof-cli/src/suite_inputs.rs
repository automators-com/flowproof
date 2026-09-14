//! Pin inputs without persisting their plaintext in checkpoint metadata.
use crate::{apply_values_context, default_trace_path, default_values_path, ValuesArgs};
use flowproof_agent::{FlowSpec, SuiteManifest};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub(crate) fn fingerprint(
    dir: &Path,
    paths: &[PathBuf],
    manifest: &SuiteManifest,
    values: &ValuesArgs,
    engine_path: Option<&Path>,
) -> Result<PinnedInputs, String> {
    let mut files = BTreeMap::new();
    for path in paths
        .iter()
        .flat_map(|path| {
            [
                path.clone(),
                default_trace_path(path),
                values
                    .vars_file
                    .clone()
                    .unwrap_or_else(|| default_values_path(path)),
            ]
        })
        .chain([dir.join("suite.yaml")])
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let absolute = parent.canonicalize().map_err(|e| e.to_string())?.join(
            path.file_name()
                .ok_or("checkpoint input needs a file name")?,
        );
        files.insert(absolute, file_digest(&path)?);
    }
    let mut stages = BTreeMap::new();
    let mut parts: Vec<Vec<u8>> = vec![
        dir.canonicalize()
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .as_bytes()
            .to_vec(),
        serde_json::to_vec(manifest).map_err(|e| e.to_string())?,
        std::fs::read(dir.join("suite.yaml")).map_err(|e| e.to_string())?,
    ];
    // The release version alone does not identify an unreleased binary.
    // Python embeddings supply the actual loaded extension image, not the
    // Python host executable. This identity is pinned on every invocation;
    // the running image does not need to be rehashed at each stage boundary.
    let engine_path = match engine_path {
        Some(path) => path.to_path_buf(),
        None => std::env::current_exe().map_err(|e| e.to_string())?,
    };
    let mut binary = File::open(engine_path).map_err(|e| e.to_string())?;
    let mut engine = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = binary.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        engine.update(&buffer[..count]);
    }
    parts.push(engine.finalize().to_vec());
    let specs = paths
        .iter()
        .map(|path| FlowSpec::load(path).map_err(|e| e.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let mut producers = BTreeMap::new();
    for (path, spec) in paths.iter().zip(&specs) {
        for name in spec.exports.keys() {
            if producers
                .insert(
                    name.clone(),
                    path.strip_prefix(dir)
                        .unwrap_or(path)
                        .to_string_lossy()
                        .into_owned(),
                )
                .is_some()
            {
                return Err(format!(
                    "checkpoint export `{name}` has more than one producer"
                ));
            }
        }
    }
    let mut produced = BTreeSet::new();
    for (path, spec) in paths.iter().zip(&specs) {
        if spec.app.id() == "agent" || !spec.apps.is_empty() {
            return Err("checkpoint recovery currently supports single-surface recorded flows, not external agent processes or multi-surface flows".into());
        }
        let trace_path = default_trace_path(path);
        let (header, steps) = flowproof_replay::load_trace(&trace_path).map_err(|e| {
            format!(
                "checkpoint requires a valid recorded trace for {}: {e}",
                path.display()
            )
        })?;
        if steps.is_empty() || header.app.name != spec.app.id() || header.app.command.is_some() {
            return Err("checkpoint requires a nonempty matching single-surface trace without an external launch command".into());
        }
        let spec_json = serde_json::to_string(&spec).map_err(|e| e.to_string())?;
        let trace = std::fs::read_to_string(&trace_path).map_err(|e| e.to_string())?;
        let overlay = apply_values_context(path, values)?;
        if spec.skip_reason().is_some() {
            return Err(format!("checkpoint flow {} is gated; enable its required environment before starting the run", path.display()));
        }
        if producers.keys().any(|name| {
            manifest.env.contains_key(name) || overlay.previous.iter().any(|(key, _)| key == name)
        }) {
            return Err("checkpoint exports must not be overwritten by suite env or values".into());
        }
        let mut refs: BTreeSet<String> = spec.skip_unless_env.iter().cloned().collect();
        for text in [&spec_json, &trace] {
            for after in text.split("${").skip(1) {
                if let Some((name, _)) = after.split_once('}') {
                    if crate::valid_env_name(name) {
                        refs.insert(name.into());
                    }
                }
            }
        }
        let relative = path.strip_prefix(dir).unwrap_or(path).to_string_lossy();
        for name in &refs {
            if let Some(producer) = producers.get(name) {
                if !produced.contains(name) || !depends_on(manifest, &relative, producer) {
                    return Err(format!(
                        "checkpoint input `{name}` must come from an earlier declared prerequisite"
                    ));
                }
            }
        }
        let export_names = producers.keys().cloned().collect();
        let env = environment(&refs, &export_names)?;
        stages.insert(
            path.clone(),
            StageInputs {
                refs,
                export_names,
                environment: crate::suite_journal::checksum(&env)?,
            },
        );
        produced.extend(spec.exports.keys().cloned());
        parts.push(
            path.strip_prefix(dir)
                .unwrap_or(path)
                .to_string_lossy()
                .as_bytes()
                .to_vec(),
        );
        parts.push(std::fs::read(path).map_err(|e| e.to_string())?);
        parts.push(spec_json.into_bytes());
        parts.push(trace.into_bytes());
        parts.push(serde_json::to_vec(&env).map_err(|e| e.to_string())?);
        let values_path = values
            .vars_file
            .clone()
            .unwrap_or_else(|| default_values_path(path));
        parts.push(if values_path.exists() {
            std::fs::read(values_path).map_err(|e| e.to_string())?
        } else {
            Vec::new()
        });
        parts.push(serde_json::to_vec(&values.vars).map_err(|e| e.to_string())?);
    }
    parts.push(serde_json::to_vec(&files).map_err(|e| e.to_string())?);
    let mut digest = Sha256::new();
    for part in parts {
        digest.update((part.len() as u64).to_le_bytes());
        digest.update(part);
    }
    let pinned = PinnedInputs {
        digest: format!("{:x}", digest.finalize()),
        files,
        stages,
    };
    pinned.verify_files()?;
    Ok(pinned)
}

fn depends_on(manifest: &SuiteManifest, flow: &str, prerequisite: &str) -> bool {
    manifest.depends_on.get(flow).is_some_and(|parents| {
        parents
            .iter()
            .any(|parent| parent == prerequisite || depends_on(manifest, parent, prerequisite))
    })
}

struct StageInputs {
    refs: BTreeSet<String>,
    export_names: BTreeSet<String>,
    environment: String,
}
pub(crate) struct PinnedInputs {
    pub digest: String,
    files: BTreeMap<PathBuf, Option<String>>,
    stages: BTreeMap<PathBuf, StageInputs>,
}
impl PinnedInputs {
    fn verify_files(&self) -> Result<(), String> {
        for (path, expected) in &self.files {
            if &file_digest(path)? != expected {
                return Err(format!("checkpoint input file {} changed during the run; no next operation was started", path.display()));
            }
        }
        Ok(())
    }
    pub fn verify_stage(&self, path: &Path) -> Result<(), String> {
        self.verify_files()?;
        let stage = self
            .stages
            .get(path)
            .ok_or("checkpoint stage was not pinned")?;
        if crate::suite_journal::checksum(&environment(&stage.refs, &stage.export_names)?)?
            != stage.environment
        {
            return Err(
                "checkpoint effective inputs changed during the run; no next operation was started"
                    .into(),
            );
        }
        Ok(())
    }
}
fn file_digest(path: &Path) -> Result<Option<String>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("reading checkpoint input {}: {e}", path.display())),
    };
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(Some(format!("{:x}", digest.finalize())))
}
fn environment(
    refs: &BTreeSet<String>,
    producers: &BTreeSet<String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut env: BTreeMap<_, _> = refs
        .iter()
        .filter(|name| !producers.contains(*name))
        .map(|name| {
            std::env::var(name)
                .map(|value| (name.clone(), value))
                .map_err(|_| format!("checkpoint input `{name}` is missing or not Unicode"))
        })
        .collect::<Result<_, _>>()?;
    // Destination/config knobs can affect replay without ${VAR} text.
    env.extend(std::env::vars().filter(|(name, _)| {
        !producers.contains(name)
            && (name.starts_with("FLOWPROOF_")
                || name.starts_with("FIORI_")
                || name.starts_with("SAP_")
                || matches!(
                    name.as_str(),
                    "CHROME"
                        | "HTTP_PROXY"
                        | "HTTPS_PROXY"
                        | "ALL_PROXY"
                        | "NO_PROXY"
                        | "SSL_CERT_FILE"
                        | "http_proxy"
                        | "https_proxy"
                        | "all_proxy"
                        | "no_proxy"
                ))
    }));
    Ok(env)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pins_absent_files_and_effective_values_before_a_stage_starts() {
        if std::env::var("FLOWPROOF_PIN_UNIT_CHILD").as_deref() != Ok("1") {
            let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args(["--exact", "suite_inputs::tests::pins_absent_files_and_effective_values_before_a_stage_starts"])
                .env("FLOWPROOF_PIN_UNIT_CHILD", "1").status().expect("isolated input-pin test");
            assert!(status.success());
            return;
        }
        let dir =
            std::env::temp_dir().join(format!("flowproof-input-pin-unit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("fixture directory");
        std::fs::write(
            dir.join("suite.yaml"),
            "flows: [case.flow.yaml]\nstop_on_failure: true\n",
        )
        .expect("manifest");
        let spec = dir.join("case.flow.yaml");
        std::fs::write(&spec, "name: unit\napp: api\nsteps:\n  - assert_api:\n      request: GET http://localhost/${PIN_INPUT_UNIT}\n      status: 200\n").expect("spec");
        // A synthetic non-executed trace tests identity pinning only; full
        // CLI regressions separately record and replay real API fixtures.
        let header = serde_json::json!({"format":"flowproof-trace","version":1,"trace_id":"unit","recorded_at":"2026-09-14T12:00:00Z","app":{"name":"api","adapter":"api"},"env":{"os":"unit","resolution":[1,1]}});
        let step = serde_json::json!({"id":"s1","intent":"unit","action":{"type":"wait","params":{}},"selectors":[],"sync":{"pre":[],"post":[]},"artifacts":{}});
        std::fs::write(default_trace_path(&spec), format!("{header}\n{step}\n"))
            .expect("synthetic identity fixture");
        let manifest = SuiteManifest::load_from_dir(&dir)
            .expect("manifest parses")
            .expect("manifest present");
        let values = ValuesArgs {
            vars_file: None,
            vars: vec!["PIN_INPUT_UNIT=0010".into()],
        };
        let pinned = fingerprint(&dir, std::slice::from_ref(&spec), &manifest, &values, None)
            .expect("preflight");
        assert_eq!(pinned.digest.len(), 64);
        let engine = dir.join("embedded-engine");
        std::fs::write(&engine, "first native image").expect("engine fixture");
        let first = fingerprint(
            &dir,
            std::slice::from_ref(&spec),
            &manifest,
            &values,
            Some(&engine),
        )
        .expect("embedded identity");
        std::fs::write(&engine, "changed native image").expect("updated engine fixture");
        let changed = fingerprint(
            &dir,
            std::slice::from_ref(&spec),
            &manifest,
            &values,
            Some(&engine),
        )
        .expect("new embedded identity");
        assert_ne!(
            first.digest, changed.digest,
            "an updated Python extension invalidates resume even when Python itself is unchanged"
        );
        let _overlay = apply_values_context(&spec, &values).expect("same overlay replay uses");
        pinned.verify_stage(&spec).expect("unchanged inputs");
        std::env::set_var("PIN_INPUT_UNIT", "0099");
        assert!(pinned
            .verify_stage(&spec)
            .expect_err("changed value")
            .contains("effective inputs changed"));
        std::env::set_var("PIN_INPUT_UNIT", "0010");
        std::fs::write(default_values_path(&spec), "PIN_INPUT_UNIT: '0099'\n")
            .expect("new sibling values");
        assert!(pinned
            .verify_stage(&spec)
            .expect_err("new file")
            .contains("changed during the run"));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
