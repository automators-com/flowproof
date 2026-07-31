# Windows egress containment — feasibility spike log

**Status: in progress.** This file is the deliverable. Someone must be able to
read it and reach the same verdict without rerunning anything.

The question: on Linux, flowproof contains an agent-under-test with an
unprivileged default-deny seccomp user-notification filter, so a flow can
DECLARE the network it may touch (`allow_egress`) and CERTIFY it touched nothing
else (`assert_no_egress`). The surfaces that differentiate the product — SAP GUI,
Windows desktop, Citrix — are Windows-hosted, where none of that exists. Can the
two be fused into:

> this agent drove SAP GUI and provably touched nothing else

A prior research pass concluded **viable-with-named-limits**, at ~90% confidence
on the WFP mechanism and ~40% on the fused claim surviving contact with SAP GUI.
That is a hypothesis to falsify, not a plan to execute.

---

## Iteration 1 — environment, harness, and the CI route onto Windows

**Date:** 2026-07-31. Branch `spike/windows-containment`.

### The environment problem, resolved — and a constraint found the hard way

There is no local Windows. The dev box is Linux (Hetzner, 7 GB, no swap); WFP,
`CreateProcessAsUser` and AppContainer cannot be exercised on it at all. The
decided environment is GitHub Actions `windows-latest`.

**Finding: `.github/workflows/ci.yml` has no `workflow_dispatch` trigger.** It
fires on push to `main`, on pull requests, and on a nightly schedule. There is
therefore *no* way to drive an arbitrary command on a Windows runner on demand,
and `.github/workflows/` is a constitution-protected path this spike may not
modify (`CLAUDE.md`, "The autonomous loops").

The route that does not need a workflow change:

```
windows job → cargo build --workspace --all-features
            → cargo test  --workspace --all-features
```

Both are `--workspace`. So a spike crate that is a **workspace member** is built
and tested on `windows-latest` with no workflow edit at all. That is the
iteration lever this spike runs on, and it is why `spike/windows-containment`
appears in the root `Cargo.toml` `members` list.

Consequences, all deliberate:

* the crate is named **`wfp-spike`**, not `flowproof-*`. The `versions agree`
  CI job greps `^name = "flowproof` out of `Cargo.lock` and requires every match
  to equal the workspace version. A `flowproof-`prefixed spike crate at `0.0.0`
  would have failed that job for a reason having nothing to do with the spike.
* `publish = false`, and the crate lives under `spike/`, not in
  `crates/flowproof-adapters`. `CHARTER.md` §3's rule against unplanned scope
  applies to this spike: a spike merged into the adapter is a feature nobody
  decided to ship.
* the Windows job is off the PR path. It needs the **`full-ci` label** on the
  pull request (or a push to `main`, which this must not have).

### Second finding: the iteration loop is much faster than "push and wait"

`cargo check --target x86_64-pc-windows-msvc` typechecks the entire Windows code
path **on the Linux box**, in seconds, with no linker and no Windows. `rustup
target add x86_64-pc-windows-msvc` is the only setup. The `windows` crate's
`raw-dylib` linkage means nothing needs importing libraries at check time.

This collapses the expensive part of the loop. Win32 signature errors — the bulk
of what goes wrong in this kind of code — are now caught locally instead of
costing a full CI cycle each. What still needs a real runner is only what needs
a real kernel: whether the filters actually block.

Recorded because it changes the plan's economics: the brief assumed "minutes per
iteration, batch aggressively". Batching is still right for *runtime* questions;
it is no longer necessary for *compile* questions.

### What was built

`spike/windows-containment/`, a supervisor plus a canary in one binary:

| File | What it does |
|---|---|
| `src/win/identity.rs` | creates/deletes the per-run local user; SID lookup; elevation check |
| `src/win/wfp.rs` | dynamic engine session, private sublayer, permit/block filters at `ALE_AUTH_CONNECT_V4/V6`, promiscuous block at `ALE_RESOURCE_ASSIGNMENT_V4` |
| `src/win/launch.rs` | window-station/desktop DACL grant, `LogonUserW`, `CreateProcessAsUserW` into a kill-on-close job, stdout redirected to an inheritable handle |
| `src/win/netevents.rs` | `FwpmNetEventSubscribe4` live capture **and** `FwpmNetEventEnum5` readback |
| `src/win/gui.rs` | days 7–9: UIA from inside the boundary, own-launched and foreign windows |
| `src/win/harness.rs` | stage orchestration, assertions, teardown checks |
| `src/oracle.rs` | destination-side listeners — the independent oracle |
| `src/canary.rs` | the probes that run inside the boundary |
| `src/report.rs` | the `SPIKE|` evidence block |

Design decisions worth having written down, because each was a choice:

* **`FWPM_CONDITION_ALE_USER_ID` takes a security descriptor**, and the right
  is `FWP_ACTRL_MATCH_FILTER` = `0x1`, which in SDDL is `CC`
  (`SDDL_CREATE_CHILD`). So the condition value is built from
  `O:SYG:SYD:(A;;CC;;;<sid>)`. This mapping is not obvious and getting it wrong
  produces a filter that adds successfully and silently matches nothing —
  exactly the failure honesty rule 8 is about.
* **There is no `FWPM_NET_EVENT_KEYWORD_CLASSIFY_DROP`.** WFP has no such
  keyword; classify-drops are collected whenever `FWPM_ENGINE_COLLECT_NET_EVENTS`
  is on, and the keyword set only adds optional categories. The harness requests
  `CLASSIFY_ALLOW` so the *declared* connection also shows up as an explicit
  allow — positive evidence that the permit filter matched, rather than the mere
  absence of a drop.
* **Three independent evidence layers per probe**, because honesty rule 2 says a
  client-side error is not enough: (1) the client's raw OS error number, (2) the
  destination's own accept log, (3) the kernel's net-event drop record. A
  client-side error alone is equally consistent with containment and with a
  typo in the harness.
* **Two destination oracles** — one on `127.0.0.1`, one on the runner's primary
  NIC address. If they disagree that is a finding, not a nuisance: an agent that
  can reach a loopback service the flow never declared is not contained.
* **The exe is copied to `C:\fp-spike-<pid>` and `icacls`-granted to the run
  identity.** A freshly created local user has no access to the runner's cargo
  target directory, so the child would die at image load with a symptom that
  looks nothing like a permissions problem.
* **The child's stdout goes to an inherited file handle**, not
  `CREATE_NEW_CONSOLE`. The canary's output is the entire evidence base and
  `CREATE_NEW_CONSOLE` throws it away.
* **The test never fails.** A red job stops at the first interesting finding and
  hides everything after it. Negative results carry equal weight in a spike, so
  they must not abort the run that produces them. The evidence is the `SPIKE|`
  block; read it with `grep '^SPIKE|'`.

### Verified locally (commands and captured exit codes)

Redirected to files and the status read separately — never piped (honesty rule 3
and `CLAUDE.md`).

| Command | Exit |
|---|---|
| `cargo check -p wfp-spike --all-targets` (Linux) | `0` |
| `cargo check -p wfp-spike --all-targets --target x86_64-pc-windows-msvc` | `0` |
| `cargo clippy -p wfp-spike --all-targets --all-features -- -D warnings` (Linux) | `0` |
| `cargo clippy -p wfp-spike --all-targets --all-features --target x86_64-pc-windows-msvc` | `0`, 0 warnings |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` (Linux) | `0` |
| `cargo fmt --all` | `0` |

The last one is `CHARTER.md` §2 invariant 3: the workspace still builds on Linux
with the spike present.

### What is NOT yet known

Everything the spike is actually for. Nothing above touches a Windows kernel.
`FwpmFilterAdd0` has never been called; no connection has been blocked; no drop
record has been read; no GUI app has been driven across or inside an identity
boundary. **A clean cross-compile is not evidence of containment** — honesty
rule 8, restated for this iteration.

### Next

Open the pull request, add the `full-ci` label, read the `SPIKE|` block.

---

## Open questions, recorded rather than asked

Per the brief, a question about the current step becomes a log entry with the
best answer and its reasoning, not a stall.

1. **Should the spike test fail CI when containment fails?** No. A spike's
   output is a verdict with evidence; a red job truncates the evidence. The
   `SPIKE|SUMMARY` block carries `met` / `not_met` / `not_run` counts, so a human
   or a later gate can decide on the numbers. If this ever became a shipping
   feature the polarity would invert — a shipping containment test must fail
   loudly — but that is a different artefact.

2. **Is `1.1.1.1:443` a fair external probe?** Only client-side. There is no
   destination-side oracle for it, so its result is recorded as
   `external.client-side-only` and weighted accordingly. It is included because
   a *purely* on-host test could be fooled by loopback being special-cased in
   the ALE layer, and the two together distinguish that case.

3. **Does the negative control risk a false pass?** It is the same code path with
   one filter omitted, so if the harness were broken in a way that made
   everything look blocked, the negative control would still report blocked and
   the disagreement would show. That is precisely why it is run in the same job
   rather than trusted from a separate run.
