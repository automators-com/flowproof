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

---

## Iteration 2 — the harness reaches a Windows runner

Branch `spike/windows-containment`, pull request
[#280](https://github.com/automators-com/flowproof/pull/280), `full-ci` label
applied. Commit `8c0ad32`.

### What changed, and why it was worth a commit rather than a one-line push

Three plumbing failures would each have cost a full CI round trip *and* produced
a log that read like a containment result rather than a setup problem. The brief
says to batch changes and to make the harness print everything that might be
wanted next time; these are that, applied to the three places the child process
can fail to exist at all.

* **`CreateProcessAsUserW` needs `SeAssignPrimaryTokenPrivilege` and
  `SeIncreaseQuotaPrivilege`.** A hosted runner is an administrator with UAC
  off, so it should hold both — but "should" is the kind of claim this spike
  exists to check. The privileges are now enabled explicitly, and if the call
  still fails the harness falls back to `CreateProcessWithLogonW` and **records
  which path worked** (`canary.spawn_path`).
* **`LogonUserW` logon type is policy, not a given.** A freshly created local
  user's right to log on interactively/as a batch job can be denied. Tried as
  `INTERACTIVE`, then `BATCH`, then `NETWORK_CLEARTEXT`; the type that succeeded
  is recorded (`canary.logon_type`).
* **`CreateProcessWithLogonW` cannot inherit handles.** On that fallback path
  the child's redirected stdout goes nowhere and the run would produce *no
  evidence at all* — a silent, total loss of the thing the spike is for. The
  canary now tees every line to a file it opens itself (`src/tee.rs`), so the
  evidence survives whichever spawn path was taken.

### Finding 2.1 — a compile error the local Windows typecheck caught for free

`CreateProcessWithLogonW` takes `PROCESS_CREATION_FLAGS`, not `u32`; the
fallback passed `.0`. Caught by
`cargo check --target x86_64-pc-windows-msvc` on the Linux box in seconds.
On a CI-only loop this would have cost a full cycle for a one-token fix, and it
is the second concrete return on finding 0.3.

### Finding 2.2 — `adversary` is red on this branch, and it is not the spike's doing

`adversary` failed on `8c0ad32` (run 30608374678) while passing on `5b731ef`.
The failure is inside the gate's **own** self-tests:

```
FAIL  a clean approval    got REFUSE, wanted APPROVE
reducer tests FAILED
```

`scripts/gate/adversary-review.test.sh` stubs the reviewer, feeds
`adversary-review.sh` a synthetic reply that should approve, and asserts it
does. It builds its diff from `HEAD~1..HEAD`, so the commit under test is an
input to a test that is supposed to be about reply parsing.

Ruled out: **diff size**. The commit that *passed* is 3341 insertions; the one
that *failed* is 302. The larger diff passed and the smaller failed, so a size
ratchet is not the cause. No `400`/`cap` string appears in
`adversary-review.sh`.

**Not escalated as a spike blocker, deliberately.** `scripts/gate/` is a
constitution-protected path this spike may not touch, and `adversary` is a
*merge* gate. This spike never merges — its output is a verdict and a log, and
nothing here is proposed for `main`. The Windows evidence comes from the `CI`
workflow's `windows build + E2E` job, which is unaffected. Recorded here because
it is a real observation about the repository that a human should see, not
because it stops anything.

`main` is green (run 30604400709, `0a463ab`), so the circuit-breaker stop
condition has not fired.

### Finding 2.3 — `constitution` passes, confirming the CI route is legitimate

Run 30608374738: **success**. The spike adds a workspace member and touches no
protected path, which is the whole basis of finding 0.1. The route onto a
Windows runner is sound rather than merely undetected.

### Local gate before the push (exit codes captured separately, never piped)

| Command | Exit |
|---|---|
| `cargo check -p wfp-spike --all-targets --target x86_64-pc-windows-msvc` | `0` |
| `cargo clippy -p wfp-spike --all-targets --all-features --target x86_64-pc-windows-msvc` | `0`, 0 warnings |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `0` |
| `cargo fmt --all --check` | `0` |
| `cargo test -p wfp-spike --all-features` (Linux) | `0` |

### Still not known

Everything the spike is for. As of this entry `FwpmFilterAdd0` has still never
been called on a real kernel. **A clean cross-compile is not evidence of
containment.**

---

## Iteration 2 — the first real Windows run, and what it did not say

Run **30608374686**, job `windows build + E2E` (id 91085555047), commit
`8c0ad32`. Step 5 `build + test (windows)`: **success**.

```
     Running tests\spike.rs (target\debug\deps\spike-bb7e9b4ad9afb1cd.exe)
running 1 test
test windows_egress_containment_spike ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.10s
```

### Finding 2.1 — the harness reached a Windows kernel and told us nothing (NEGATIVE)

The spike crate compiled on `windows-latest`, the test ran, and it produced
**zero** `SPIKE|` lines. Grepping the downloaded job log:

```bash
gh api "repos/automators-com/flowproof/actions/jobs/91085555047/logs" > wj.log   # exit 0
grep -c 'SPIKE|' wj.log                                                          # 0
```

Cause: **`cargo test` captures `println!` output for a test that passes**, and
this test always passes on purpose. The evidence block existed and was thrown
away. The CI step is `cargo test --workspace --all-features` with no
`--nocapture`, and that step lives in `.github/workflows/`, which the spike may
not modify.

This is the spike's own version of the mistake the honesty rules are about: the
run *looked* successful — green step, green job, green test — and carried no
information whatsoever. `test ... ok` was, precisely, a false green.

**What the 5.10s runtime does and does not tell us.** It is not instant, so the
test did more than return at the elevation check. It is also far too short for
the GUI stages. Both readings are consistent with the log, so **neither is
recorded as a result.** No conclusion is drawn from a duration.

**Fix, and its verification.** Evidence now goes to the stderr *file descriptor*
via `report::emit`, not through `println!`. libtest captures by swapping the
sink that the `print!`/`eprint!` macros consult; a handle from `io::stderr()`
writes to the descriptor directly and is not intercepted.

Verified on the Linux box rather than assumed — the same mechanism, one CI cycle
saved:

```bash
cargo test -p wfp-spike --all-features > capture.log 2>&1   # exit 0
grep -n 'SPIKE|' capture.log
# 18:SPIKE|SKIP|not Windows; nothing to measure
```

Before the fix that line did not appear. Rejected alternatives: `--nocapture`
and `RUST_TEST_NOCAPTURE` both live in the protected workflow; putting the
variable in `.cargo/config.toml` would make every other crate's tests noisy to
fix one crate's problem.

### Finding 2.2 — a real defect in the adversary gate (ESCALATION: protected path)

`adversary` failed on run **30608374678** with:

```
FAIL  a clean approval    got REFUSE, wanted APPROVE
reducer tests FAILED
```

It passed on the same branch one commit earlier, and the failing case is a
reducer unit test on synthetic fixtures — which cannot depend on the diff. It
does. `scripts/gate/adversary-review.test.sh` drives the **real**
`adversary-review.sh` against the real `HEAD~1..HEAD` diff, and
`adversary-review.sh` passes that diff to the model **as a single command-line
argument**:

```sh
claude -p "$(cat <<PROMPT ... ${diff} ... PROMPT)"
```

Linux caps a *single* argv entry at `MAX_ARG_STRLEN` = 128 KiB, independently of
`ARG_MAX` (2 MiB here). This pull request's diff is **137,731 bytes**. Exec
fails, the script sees rc 126, and reports it as a review refusal.

Reproduced directly, with a stub `claude` on `PATH` and an unambiguous approving
reply:

```
PR diff bytes: 137731
ARG_MAX: 2097152
SUT_EXIT=1
scripts/gate/adversary-review.sh: line 47: /tmp/.../claude: Argument list too long
::error::correctness: the reviewer exited 126; refusing
```

The same test passes locally at `HEAD~1..HEAD` (a small diff), which is exactly
why it looked branch-specific.

**Why it matters beyond this spike.** Any pull request whose diff exceeds ~128
KiB gets a refusal that reads as *the reviewer refused the change*, when the
truth is *the reviewer never ran*. That is the defect class `CHARTER.md` §5.1
puts first — a verdict that does not mean what it says — sitting in the gate
itself. It is also self-concealing: the bigger the change, the more a refusal
looks earned.

**This spike cannot fix it.** `scripts/gate/` is constitution-protected
(`CLAUDE.md`); a loop that could edit the gate could weaken the gate.

> **ESCALATION — needs a human.** `scripts/gate/adversary-review.sh` line ~47
> must stop passing the diff through argv. Writing the prompt to a temporary
> file and piping it (`claude -p - < "$prompt"`, or `--prompt-file`) removes the
> limit. Separately, an exec failure should be reported as a *tooling* failure
> distinct from a review refusal, so a 126 never again reads as a judgement.
> Nothing in this spike depends on the fix: `adversary` is not what gates the
> Windows job, and this pull request is not for merging.

### Finding 2.3 — this worktree is not exclusively ours

Commit `8c0ad32` was authored and pushed by something other than this session,
from this session's uncommitted working tree, mid-edit. That cancelled the
in-flight CI run (`cancel-in-progress` on the branch's concurrency group) and is
why run 30608104377 shows `cancelled` with the Windows job half-built.

No harm done — the pushed tree matched the intended batch and `cargo fmt --all
--check` was clean at that commit (exit `0`). Recorded because it changes how
this spike must be run: **edits to the worktree can become pushes at any
moment**, so a run must not be assumed to survive an edit. Later iterations
wait for the Windows job to reach `completed` before touching a file.

### Next

Re-run with the capture fix. That run is the first one that can produce evidence
about containment; everything before it is scaffolding.
