//! Filesystem observation, the cross-platform half: what the seccomp
//! supervisor SAW a run do to the filesystem. The mechanism is in
//! `egress_linux`, sharing the one filter the child installs.
//!
//! The vocabulary is deliberately disjoint from `egress`'s. That module says
//! `containment` and `enforced`, because it stops things. This one says
//! `observation` and `observed`, because it stops nothing: every trap replies
//! CONTINUE and the syscall runs. Sharing a word would let an auditor read
//! "the filesystem was contained" off a run where nothing of the sort
//! happened.

use std::collections::BTreeSet;

/// One destructive filesystem syscall the supervisor watched go past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsEvent {
    /// The syscall by name: `unlinkat`, `truncate`, `openat`.
    pub op: String,
    /// What it acted on, absolute where the supervisor could resolve one. The
    /// rename family names both, `source -> destination`.
    pub path: Option<String>,
    /// Why `path` is missing or unresolved. Weaker evidence, labelled as
    /// such: the TRAP is what proves the syscall happened, and traps fire on
    /// syscall number, which nothing can race.
    pub path_note: Option<String>,
    /// The flags worth naming, for the calls that carry them:
    /// `O_WRONLY|O_TRUNC`, `AT_REMOVEDIR`.
    pub flags: Option<String>,
    pub at_ms: u64,
}

impl FsEvent {
    /// The one-line rendering used in the report.
    pub fn line(&self) -> String {
        let flags = match &self.flags {
            Some(f) => format!(" [{f}]"),
            None => String::new(),
        };
        let note = match &self.path_note {
            Some(n) => format!(" ({n})"),
            None => String::new(),
        };
        let path = self.path.as_deref().unwrap_or("<path unknown>");
        format!("{}{flags} {path}{note} at {}ms", self.op, self.at_ms)
    }
}

/// What the supervisor observed the filesystem take, split the way
/// [`EgressLog`] is - and for the same reason.
///
/// [`EgressLog`]: crate::egress::EgressLog
#[derive(Debug, Clone, Default)]
pub struct FsLog {
    /// Every destructive syscall observed, in order.
    ///
    /// ATTEMPTS, precisely. The reply is CONTINUE, which is sent BEFORE the
    /// kernel runs the call, so an `rmdir` of a directory that was not there
    /// appears here exactly like one that removed a tree. Learning the outcome
    /// would mean performing the syscall ourselves - which is prevention, and
    /// the thing this feature deliberately does not do.
    pub destructive: Vec<FsEvent>,
    /// Every supervisor FAULT: a trapped syscall whose DESTRUCTIVENESS could
    /// not be adjudicated at all - an `openat2` whose `open_how` was
    /// unreadable, a trapped number with no handler.
    ///
    /// A finer line than egress draws, because most of the evidence here is
    /// trap-borne: a path the supervisor could not read still leaves an
    /// event, since the notification alone proves the syscall happened. Only
    /// "we cannot say whether this destroyed anything" belongs in here.
    pub faults: Vec<String>,
}

impl FsLog {
    /// Nothing destructive was observed AND every trapped syscall was
    /// adjudicated. Both halves, mirroring [`EgressLog::is_clean`]: an empty
    /// `destructive` list under a blind supervisor is silence, not evidence.
    ///
    /// [`EgressLog::is_clean`]: crate::egress::EgressLog::is_clean
    pub fn is_clean(&self) -> bool {
        self.destructive.is_empty() && self.faults.is_empty()
    }

    /// The distinct faults, deduped and in first-seen order. One broken
    /// mechanism produces a fault per trapped syscall, and a hundred copies of
    /// the same message is one finding.
    pub fn distinct_faults(&self) -> Vec<String> {
        let mut seen = BTreeSet::new();
        self.faults
            .iter()
            .filter(|f| seen.insert((*f).clone()))
            .cloned()
            .collect()
    }

    /// The report block, or nothing at all when there is nothing to say.
    ///
    /// Silent on a clean run deliberately: this feature asserts nothing, so a
    /// line on every green build is noise nobody reads, and the containment
    /// tier already prints unconditionally to say the mechanism was there.
    /// What it exists to do is make destruction VISIBLE when it happens.
    pub fn report_lines(&self) -> Vec<String> {
        if self.is_clean() {
            return Vec::new();
        }
        let mut out = vec![format!(
            "filesystem observation: observed (linux seccomp); {} destructive syscall(s)",
            self.destructive.len()
        )];
        out.extend(self.destructive.iter().map(|e| format!("  {}", e.line())));
        out.extend(
            self.distinct_faults()
                .iter()
                .map(|f| format!("  fault: {f}")),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(op: &str, path: Option<&str>) -> FsEvent {
        FsEvent {
            op: op.into(),
            path: path.map(Into::into),
            path_note: None,
            flags: None,
            at_ms: 412,
        }
    }

    #[test]
    fn a_run_that_destroyed_nothing_says_nothing() {
        assert!(FsLog::default().is_clean());
        assert!(FsLog::default().report_lines().is_empty());
    }

    #[test]
    fn the_report_names_the_op_the_flags_and_the_path() {
        let mut trunc = event("openat", Some("/home/u/db.sqlite"));
        trunc.flags = Some("O_WRONLY|O_TRUNC".into());
        let log = FsLog {
            destructive: vec![event("unlinkat", Some("/home/u/exports/2025.csv")), trunc],
            faults: Vec::new(),
        };
        assert!(!log.is_clean());
        let lines = log.report_lines();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("2 destructive syscall(s)"));
        assert!(
            lines[0].contains("observed"),
            "never `enforced`: {}",
            lines[0]
        );
        assert!(
            !lines[0].contains("contain"),
            "never containment vocabulary"
        );
        assert_eq!(lines[1], "  unlinkat /home/u/exports/2025.csv at 412ms");
        assert_eq!(
            lines[2],
            "  openat [O_WRONLY|O_TRUNC] /home/u/db.sqlite at 412ms"
        );
    }

    /// An unreadable path weakens the evidence; it does not remove it. The
    /// event still appears, because the trap already proved the syscall
    /// happened - only the string naming the victim is missing.
    #[test]
    fn an_unresolved_path_still_leaves_an_event() {
        let mut e = event("unlinkat", None);
        e.path_note = Some("unresolved: process_vm_readv: EPERM".into());
        let log = FsLog {
            destructive: vec![e],
            faults: Vec::new(),
        };
        assert!(!log.is_clean());
        assert_eq!(
            log.report_lines()[1],
            "  unlinkat <path unknown> (unresolved: process_vm_readv: EPERM) at 412ms"
        );
    }

    /// The fault half, mirroring the egress log's: a supervisor that could not
    /// adjudicate observed nothing, which is not the same as observing that
    /// nothing happened.
    #[test]
    fn a_fault_alone_is_not_a_clean_run() {
        let log = FsLog {
            destructive: Vec::new(),
            faults: vec![
                "openat2: could not read open_how: EPERM".into(),
                "openat2: could not read open_how: EPERM".into(),
            ],
        };
        assert!(log.destructive.is_empty());
        assert!(!log.is_clean(), "a blind supervisor observed nothing");
        assert_eq!(log.distinct_faults().len(), 1, "one mechanism, one finding");
        let lines = log.report_lines();
        assert_eq!(lines.len(), 2);
        assert!(lines[1].starts_with("  fault: openat2"));
    }
}
