//! The on-trace side-effect record, shared by the lane builder
//! (`flowproof-cli`) and the capture side (`flowproof-adapters`) so the
//! two never drift on what a record means.
//!
//! A record is an observed ATTEMPT, never an outcome claim. The fs
//! observation replies CONTINUE before the kernel runs the call, so an
//! `unlinkat` of a file that was not there reads identically to one that
//! destroyed data. An `http_request` record is a connect/send the
//! supervisor itself performed on the child's behalf - stronger evidence,
//! but still the destination, not the bytes.

use serde::{Deserialize, Serialize};

/// A destructive filesystem syscall observed by the seccomp supervisor.
pub const KIND_FS_WRITE: &str = "fs_write";

/// A non-loopback connect/send the supervisor performed for the child.
pub const KIND_HTTP_REQUEST: &str = "http_request";

/// The kinds the capture mechanism can actually produce. Spec validation
/// and capture both read THIS list, so grammar and mechanism cannot
/// drift; the reserved kinds (`db_change`, `sap_transaction` - in the
/// schema's enum, never emitted in Phase A) are deliberately not in it.
pub fn capturable_kinds() -> &'static [&'static str] {
    &[KIND_FS_WRITE, KIND_HTTP_REQUEST]
}

/// One observed side effect, recorded into the trace's `side_effects`
/// lane. `at_ms` is monotonic milliseconds since agent spawn - NEVER wall
/// clock, so a re-record does not churn the lane on timing alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SideEffect {
    /// [`KIND_FS_WRITE`] or [`KIND_HTTP_REQUEST`]. A `String` rather than
    /// an enum so a reader tolerates a kind it predates - the forward
    /// posture a cassette turn's `protocol` takes.
    pub kind: String,
    /// fs: the hygiene-processed workspace-relative path, `./`-prefixed
    /// and byte-for-byte the NAME the syscall used minus the workspace
    /// prefix - never a resolution claim, since a symlinked intermediate
    /// component can carry the actual victim elsewhere. A rename renders
    /// `"<src> -> <dst>"`. http: `ip:port`, or the UNRESOLVED `${VAR}`
    /// allow-entry spelling. Absent when redacted or unreadable -
    /// `target_note` says why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Why `target` is absent or weakened: the trap proved the syscall,
    /// the string naming the victim is weaker evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_note: Option<String>,
    /// fs: the syscall name. http: the transport (`tcp`/`udp`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    /// fs only: the closed flag renderings, e.g. `O_WRONLY|O_TRUNC`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flags: Option<String>,
    /// Monotonic milliseconds since the agent was spawned.
    pub at_ms: u64,
    /// RESERVED, never emitted in Phase A: a before-image would require
    /// performing or delaying the syscall, which is prevention, not
    /// observation. Declared now so a later phase adds no format change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    /// RESERVED, same reasoning as `before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// RESERVED, same reasoning as `before`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capturable_kinds_exclude_the_reserved_ones() {
        assert_eq!(capturable_kinds(), &[KIND_FS_WRITE, KIND_HTTP_REQUEST]);
        assert!(!capturable_kinds().contains(&"db_change"));
        assert!(!capturable_kinds().contains(&"sap_transaction"));
    }
}
