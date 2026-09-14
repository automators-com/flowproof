//! Conservative suite recovery combines policy, reviewed inputs and a durable journal.
use crate::{suite_inputs, suite_journal::Journal, MissingTrace, ValuesArgs};
use flowproof_agent::{FlowSpec, SuiteManifest};
use flowproof_replay::{ResolvedExports, RunReport};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, clap::Args)]
pub(crate) struct RecoveryArgs {
    /// Set by the embedding library only; never a CLI override.
    #[arg(skip)]
    pub engine_path: Option<PathBuf>,
    /// Suite only: persist confirmed stages and exports to a private local file.
    #[arg(long)]
    pub checkpoint: Option<PathBuf>,
    /// Continue a checkpoint's unstarted stages; never retry an uncertain stage.
    #[arg(long, requires = "checkpoint")]
    pub resume: bool,
    /// Pause after this selected flow passes (relative *.flow.yaml path).
    #[arg(long, requires = "checkpoint", value_name = "FLOW")]
    pub stop_after: Option<String>,
}

pub(crate) fn validate_mode(
    args: &RecoveryArgs,
    manifest: &SuiteManifest,
    retries: u8,
    missing: MissingTrace,
    trace: Option<&Path>,
) -> Result<(), String> {
    if args.checkpoint.is_none() {
        return Ok(());
    }
    if manifest.flows.is_none() || !manifest.stop_on_failure {
        return Err(
            "checkpoint runs require explicit suite `flows` and `stop_on_failure: true`".into(),
        );
    }
    if retries != 0 || missing == MissingTrace::Record || trace.is_some() {
        return Err("checkpoint runs refuse retries, record-missing, and trace overrides; record and review each flow separately".into());
    }
    if manifest.env_from.is_some()
        || manifest.before_each.is_some()
        || manifest.after_each.is_some()
    {
        return Err("checkpoint runs refuse env_from and shell hooks: their effects cannot be resumed safely; prepare values with --vars first".into());
    }
    if let Some(stop) = &args.stop_after {
        if !manifest
            .flows
            .as_ref()
            .is_some_and(|flows| flows.contains(stop))
        {
            return Err(format!("--stop-after `{stop}` is not a selected flow"));
        }
    }
    Ok(())
}

pub(crate) struct Checkpoint {
    dir: PathBuf,
    journal: Journal,
    pinned: suite_inputs::PinnedInputs,
}
impl Checkpoint {
    pub fn open(
        dir: &Path,
        specs: &[PathBuf],
        manifest: &SuiteManifest,
        values: &ValuesArgs,
        args: &RecoveryArgs,
    ) -> Result<Option<Self>, String> {
        let Some(path) = &args.checkpoint else {
            return Ok(None);
        };
        let pinned =
            suite_inputs::fingerprint(dir, specs, manifest, values, args.engine_path.as_deref())?;
        let names: Vec<_> = specs
            .iter()
            .map(|p| {
                p.strip_prefix(dir)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        let export_names: BTreeMap<String, BTreeSet<String>> = specs
            .iter()
            .zip(&names)
            .map(|(path, name)| {
                FlowSpec::load(path)
                    .map(|spec| (name.clone(), spec.exports.into_keys().collect()))
                    .map_err(|e| e.to_string())
            })
            .collect::<Result<_, _>>()?;
        let journal = Journal::open(
            path,
            args.resume,
            pinned.digest.clone(),
            names,
            export_names,
        )?;
        if args.resume
            && args
                .stop_after
                .as_ref()
                .is_some_and(|stop| journal.completed(stop).is_some())
        {
            return Err("--stop-after boundary is already confirmed; choose a pending flow or omit the boundary".into());
        }
        Ok(Some(Self {
            dir: dir.to_path_buf(),
            journal,
            pinned,
        }))
    }
    fn key(&self, path: &Path) -> String {
        path.strip_prefix(&self.dir)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned()
    }
    pub fn completed(&self, spec: &Path) -> Option<&RunReport> {
        self.journal.completed(&self.key(spec))
    }
    pub fn restore_exports(&self, spec: &Path) {
        self.journal.restore_exports(&self.key(spec));
    }
    pub fn begin(&mut self, spec: &Path) -> Result<(), String> {
        self.pinned.verify_stage(spec)?;
        self.journal.begin(&self.key(spec))
    }
    pub fn complete(
        &mut self,
        spec: &Path,
        report: &RunReport,
        exports: &ResolvedExports,
    ) -> Result<(), String> {
        self.journal.complete(&self.key(spec), report, exports)
    }
}
