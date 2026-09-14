//! Durable local claim journal; suite policy and input pinning are separate.
use flowproof_replay::{ResolvedExports, RunReport};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirmed {
    spec: String,
    report: RunReport,
    exports: ResolvedExports,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u8,
    fingerprint: String,
    started: Option<String>,
    confirmed: Vec<Confirmed>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    state: State,
    checksum: String,
}
pub(crate) fn checksum(value: &impl Serialize) -> Result<String, String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}
fn private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| format!("creating {}: {e}", path.display()))
}
struct Lock(PathBuf);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) struct Journal {
    path: PathBuf,
    specs: Vec<String>,
    export_names: BTreeMap<String, BTreeSet<String>>,
    saved: Saved,
    _lock: Lock,
}
impl Journal {
    pub fn open(
        path: &Path,
        resume: bool,
        fingerprint: String,
        specs: Vec<String>,
        export_names: BTreeMap<String, BTreeSet<String>>,
    ) -> Result<Self, String> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        // An existing private directory is deliberate: never create a public
        // export archive, and never follow a checkpoint or lock symlink.
        let path = parent
            .canonicalize()
            .map_err(|e| e.to_string())?
            .join(path.file_name().ok_or("checkpoint needs a file name")?);
        let mut lock_name = path.as_os_str().to_owned();
        lock_name.push(".lock");
        let lock_path = PathBuf::from(lock_name);
        let mut file = private_file(&lock_path).map_err(|e| format!("checkpoint is locked or unavailable ({e}); verify no run is active before removing a stale lock"))?;
        let lock = Lock(lock_path);
        writeln!(file, "{}", std::process::id())
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        let saved = if resume {
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if !meta.is_file() {
                return Err("checkpoint must be a regular local file".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o077 != 0 {
                    return Err(
                        "checkpoint contains exports and must have private permissions (chmod 600)"
                            .into(),
                    );
                }
            }
            let saved: Saved =
                serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                    .map_err(|e| format!("invalid checkpoint: {e}"))?;
            if saved.state.schema != 1
                || checksum(&saved.state)? != saved.checksum
                || saved.state.fingerprint != fingerprint
            {
                return Err("checkpoint integrity or inputs changed; suite, specs, traces, engine, values and referenced environment must match".into());
            }
            if let Some(started) = &saved.state.started {
                return Err(format!("checkpoint flow `{started}` has an uncertain outcome; reconcile the system and its evidence before a new reviewed run; resume will not repeat it"));
            }
            for (i, confirmed) in saved.state.confirmed.iter().enumerate() {
                let actual: BTreeSet<_> = confirmed
                    .exports
                    .iter()
                    .map(|(name, _)| name.clone())
                    .collect();
                if specs.get(i) != Some(&confirmed.spec)
                    || !confirmed.report.passed
                    || matches!(confirmed.report.trace_id.as_str(), "skipped" | "errored")
                    || actual.len() != confirmed.exports.len()
                    || export_names.get(&confirmed.spec) != Some(&actual)
                {
                    return Err(
                        "checkpoint confirmed stages or exports do not match the selected suite"
                            .into(),
                    );
                }
            }
            saved
        } else {
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err("checkpoint already exists; use --resume to continue it, or a new path for an intentionally new run".into());
            }
            Saved {
                state: State {
                    schema: 1,
                    fingerprint,
                    started: None,
                    confirmed: Vec::new(),
                },
                checksum: String::new(),
            }
        };
        let mut journal = Self {
            path,
            specs,
            export_names,
            saved,
            _lock: lock,
        };
        if !resume {
            journal.save()?;
        }
        Ok(journal)
    }
    pub fn completed(&self, spec: &str) -> Option<&RunReport> {
        self.saved
            .state
            .confirmed
            .iter()
            .find(|c| c.spec == spec)
            .map(|c| &c.report)
    }
    pub fn restore_exports(&self, spec: &str) {
        if let Some(confirmed) = self.saved.state.confirmed.iter().find(|c| c.spec == spec) {
            for (name, value) in &confirmed.exports {
                std::env::set_var(name, value);
            }
        }
    }
    pub fn begin(&mut self, spec: &str) -> Result<(), String> {
        let key = spec.to_string();
        if self.saved.state.started.is_some()
            || self.specs.get(self.saved.state.confirmed.len()) != Some(&key)
        {
            return Err("checkpoint can only start the next unstarted flow".into());
        }
        self.saved.state.started = Some(key);
        self.save()
    }
    pub fn complete(
        &mut self,
        spec: &str,
        report: &RunReport,
        exports: &ResolvedExports,
    ) -> Result<(), String> {
        let key = spec.to_string();
        let actual: BTreeSet<_> = exports.iter().map(|(name, _)| name.clone()).collect();
        if self.saved.state.started.as_ref() != Some(&key)
            || !report.passed
            || actual.len() != exports.len()
            || self.export_names.get(&key) != Some(&actual)
        {
            return Err("cannot confirm checkpoint: stage or exports do not match".into());
        }
        self.saved.state.confirmed.push(Confirmed {
            spec: key,
            report: report.clone(),
            exports: exports.clone(),
        });
        self.saved.state.started = None;
        self.save()
    }
    fn save(&mut self) -> Result<(), String> {
        self.saved.checksum = checksum(&self.saved.state)?;
        let tmp = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let bytes = serde_json::to_vec_pretty(&self.saved).map_err(|e| e.to_string())?;
        let mut file = private_file(&tmp)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        File::open(self.path.parent().expect("canonical parent"))
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn durable_claims_exports_and_lock_survive_reopening() {
        let dir =
            std::env::temp_dir().join(format!("flowproof-journal-unit-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("fixture directory");
        let path = dir.join("progress.json");
        let open = |resume| {
            Journal::open(
                &path,
                resume,
                "reviewed-inputs".into(),
                vec!["create".into()],
                BTreeMap::from([(
                    "create".into(),
                    BTreeSet::from(["JOURNAL_ORDER_UNIT".into()]),
                )]),
            )
        };
        let mut journal = open(false).expect("new journal");
        assert!(open(false)
            .err()
            .expect("exclusive lock")
            .contains("locked"));
        assert!(journal.begin("other").is_err());
        journal.begin("create").expect("durable claim");
        drop(journal);
        assert!(open(true)
            .err()
            .expect("uncertain outcome")
            .contains("uncertain outcome"));
        std::fs::remove_file(&path).expect("reset isolated fixture");
        let mut journal = open(false).expect("new isolated run");
        journal.begin("create").expect("claim");
        let report = RunReport::agent("create", None, 1);
        assert!(
            journal.complete("create", &report, &vec![]).is_err(),
            "missing exports cannot confirm a stage"
        );
        journal
            .complete(
                "create",
                &report,
                &vec![("JOURNAL_ORDER_UNIT".into(), "0010".into())],
            )
            .expect("confirmed report and ID");
        drop(journal);
        assert!(open(false)
            .err()
            .expect("no overwrite")
            .contains("already exists"));
        let journal = open(true).expect("resume confirmed journal");
        assert!(journal.completed("create").is_some());
        journal.restore_exports("create");
        assert_eq!(std::env::var("JOURNAL_ORDER_UNIT").as_deref(), Ok("0010"));
        std::env::remove_var("JOURNAL_ORDER_UNIT");
        drop(journal);
        let saved = std::fs::read_to_string(&path).expect("saved bytes");
        std::fs::write(&path, saved.replace("0010", "corrupted")).expect("corrupt fixture");
        assert!(open(true)
            .err()
            .expect("integrity failure")
            .contains("integrity"));
        std::fs::remove_dir_all(dir).expect("cleanup");
    }
}
