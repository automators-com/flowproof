//! flowproof — Windows egress containment feasibility spike.
//!
//! **This is not shipping code and must not become any.** It exists to produce
//! the evidence in `spike/windows-containment/LOG.md`, and should be deleted
//! along with its workspace-members entry once the verdict is taken.
//!
//! The question: on Linux, flowproof contains an agent-under-test with an
//! unprivileged default-deny seccomp user-notification filter, so a flow can
//! DECLARE the network it may touch and CERTIFY it touched nothing else. The
//! surfaces that differentiate the product — SAP GUI, Windows desktop, Citrix —
//! are Windows-hosted, where none of that exists. This spike asks whether the
//! two can be fused.
//!
//! It lives in the workspace for one reason: the Windows CI job runs
//! `cargo test --workspace --all-features`, and `.github/workflows/` is a
//! constitution-protected path a loop may not modify. There is no local
//! Windows, so workspace membership is the only route onto a `windows-latest`
//! runner.

pub mod canary;
pub mod oracle;
pub mod report;
pub mod tee;

#[cfg(windows)]
pub mod win;
