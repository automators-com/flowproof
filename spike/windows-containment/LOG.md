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

## Iteration 3 — the first real Windows run, and what it did not say

Run **30608374686**, job `windows build + E2E` (id 91085555047), commit
`8c0ad32`. Step 5 `build + test (windows)`: **success**.

```
     Running tests\spike.rs (target\debug\deps\spike-bb7e9b4ad9afb1cd.exe)
running 1 test
test windows_egress_containment_spike ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.10s
```

### Finding 3.1 — the harness reached a Windows kernel and told us nothing (NEGATIVE)

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

### Finding 3.2 — a real defect in the adversary gate (ESCALATION: protected path)

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

### Finding 3.3 — this worktree is not exclusively ours

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

---

## Iteration 3b — the first Windows run measured nothing, and why

`windows build + E2E` on run 30608374686: **success**. The spike test ran on a
real `windows-latest` kernel and reported `ok`.

**It produced zero `SPIKE|` lines.** `grep -c 'SPIKE|' winci.log` → `0`.

### Finding 3b.1 — `cargo test` swallowed the entire evidence base (NEGATIVE, cost one full run)

Line 2877 of the job log:

```
test windows_egress_containment_spike ... ok
```

It ran in **~5 seconds** (06:08:14 → 06:08:19), against stages that sleep for
longer than that on purpose. So the job was green, the test passed, and nothing
was learned.

The cause is the test harness, not the spike: **`cargo test` captures the stdout
of a *passing* test and prints it only for a failing one.** The Windows CI step
is `cargo test --workspace --all-features` with no `--nocapture`, and
`.github/workflows/` may not be modified. Every `SPIKE|` line was written,
captured, and discarded.

This is the same class of mistake as honesty rule 3's piped exit code: a green
signal that says nothing about the thing under test. **A green Windows job is
not evidence of containment.**

**The fix, verified before it was relied on.** libtest's capture is a
thread-local redirect of Rust's `print!` macros (`io::set_output_capture`); it
does not touch file descriptor 1. A child process inherits the real descriptor,
so its output reaches the job log even when the test passes. Confirmed on Linux
with a throwaway crate rather than assumed:

```
DIRECT|printed-by-the-test-thread     <- absent from the log
CHILD|this-came-from-a-subprocess     <- present, on a PASSING test
```

`tests/spike.rs` is now a launcher: it spawns `wfp-spike run-all` with
`Stdio::inherit()` and asserts only that the binary started and exited zero.
The whole spike moved into `harness::run_all()`.

### Finding 3b.2 — the elevation preflight was a veto disguised as a check

The 5-second runtime is consistent with the preflight abort, which was:

```rust
if !identity::is_elevated() { report.not_run("all", ...); return; }
```

**This is inference, not evidence** — the output that would have confirmed it
was captured and lost, so the cause is not established. It is corrected anyway,
because the check is wrong on its own terms:

`TokenIsElevated` reports whether a token *has been elevated*, which is only
meaningful when UAC produces a split token. GitHub's `windows-latest` runners
run with **UAC disabled**, where there is no split — so the flag can read false
on a token holding every administrative right. Gating the spike on it turns a
proxy into a veto, on precisely the machine the spike was built for.

Replaced with: report `TokenIsElevated`, report `CheckTokenMembership` against
the local Administrators group, report every privilege enable, **and run the
stages regardless**. `FwpmEngineOpen0` returning `ERROR_ACCESS_DENIED` is a
better answer to "elevated enough?" than any preflight flag, and it is an answer
the log can carry.

The general lesson, which applies past this spike: *a precondition check that
can be wrong should report, not veto.* A veto that fires wrongly is
indistinguishable in the log from the work having been done.

### Local gate before the push

| Command | Exit |
|---|---|
| `cargo check -p wfp-spike --all-targets --target x86_64-pc-windows-msvc` | `0` |
| `cargo clippy -p wfp-spike --all-targets --all-features --target x86_64-pc-windows-msvc` | `0`, 0 warnings |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `0` |
| `cargo fmt --all --check` | `0` |
| `cargo test -p wfp-spike --all-features` (Linux) | `0` |

### Still not known

Unchanged from iteration 3, and now known to be unchanged *because the run
measured nothing*: `FwpmFilterAdd0` has never been called on a real kernel.
Two Windows runs have now completed without producing a single byte of
containment evidence.

---

## Iteration 3c — the harness reached the kernel, and the kernel said no

Run **30610129516**, job id `91090900780`, commit `ad654ca`. Step 5
`build + test (windows)`: **success**. 35 `SPIKE|` lines — the capture fix from
finding 4.1 works on Windows exactly as it did locally.

### The summary line, unedited

```
SPIKE|SUMMARY|met=0|not_met=0|not_run=4
```

**Nothing was measured, and nothing was claimed.** The `not_run` bookkeeping
earned its place here: four assertions that could not be evaluated are recorded
as four assertions that could not be evaluated, not as four passes and not as
four failures. Honesty rule 1 in practice — the thing under test never ran, and
the log says exactly that.

### Finding 3c.1 — `NetUserAdd` refuses the password, and the error name lies (BLOCKER, fixed)

```
SPIKE|ASSERT|core|NOT-RUN|expected=boundary established|observed=NOT RUN:
  NetUserAdd failed: code=2245 (0x000008c5) parm_err=4294967295 name=fp-spk-9024-core
```

2245 is `NERR_PasswordTooShort`. The password was `Fp!Spk-9024-core` — sixteen
characters, upper, lower, digit and symbol. It is not short and it is not
simple.

The real rule: Windows password complexity **rejects a password containing any
token of the account name three characters or longer**, splitting the name on
delimiters. Account `fp-spk-9024-core` yields the tokens `spk`, `9024`, `core`,
and the password contained all three. The error is returned for every
password-policy rejection regardless of which rule fired, so a complexity
failure surfaces under a name that says "too short".

Fixed: the password now shares nothing with the account name, and the harness
prints the name length, the password length and a decoded explanation alongside
the raw code, so this class of failure reads correctly on first sight next time.
A stale account from a crashed run (`NERR_UserExists`, 2224) is now deleted and
recreated rather than aborting the run.

The constant password is recorded as a **spike shortcut**: acceptable only
because the account is local-only, unprivileged and deleted in the same run.
Anything shipping must generate it from a CSPRNG.

### Finding 3c.2 — the runner's Administrator does NOT hold SeAssignPrimaryTokenPrivilege

```
SPIKE|NOTE|preflight.elevated|true
SPIKE|NOTE|preflight.privilege.SeAssignPrimaryTokenPrivilege|NOT HELD by this token (ERROR_NOT_ALL_ASSIGNED)
SPIKE|NOTE|preflight.privilege.SeIncreaseQuotaPrivilege|ENABLED
SPIKE|NOTE|preflight.privilege.SeTcbPrivilege|NOT HELD by this token (ERROR_NOT_ALL_ASSIGNED)
SPIKE|NOTE|gui.foreign.token_sid|S-1-5-21-1742564184-1656218818-310408600-500
```

The job runs as RID **500**, the built-in Administrator, elevated — and still
does not hold `SeAssignPrimaryTokenPrivilege`. So **`CreateProcessAsUserW`
cannot be used on this runner**, and would have failed with
`ERROR_PRIVILEGE_NOT_HELD` (1314) — an error that reads as "you are not an
administrator" when the process demonstrably is one.

This was anticipated and the fallback was already in place from iteration 2:
`CreateProcessWithLogonW`, which goes through the Secondary Logon service and
needs no privilege. It is **not a weakening** — the child still runs under the
per-run identity, which is the only property containment depends on. It does
cost handle inheritance, which is why the canary writes its own side-channel log
(`--out`); without that, this run would have produced a contained child and no
evidence from it.

**This is a real constraint on the product claim, not a CI artefact.** Any
adopter whose agent host cannot grant `SeAssignPrimaryTokenPrivilege` is on the
`CreateProcessWithLogonW` path, which requires the Secondary Logon service to be
running and cannot be used from a process running as LocalSystem — so a
flowproof runner installed *as a Windows service* would need one or the other
arranged deliberately. Named here rather than discovered later.

### What worked, and is worth not re-proving

* The per-run identity's **SID lookup, desktop/window-station DACL grant path
  and net-event enablement** were all reached without error before the failure.
* The destination oracle bound correctly on a **non-loopback** address:
  `oracle.primary_ipv4|10.1.0.102`, `oracle.primary_is_loopback|false`. The
  loopback-versus-NIC comparison the log promised is therefore available.
* The GUI stage launched a foreign-owned Notepad (`gui.foreign.pid|6632`) and
  read its token SID. The day 7–9 scaffolding runs.

### Still not known

Every containment question. `FwpmFilterAdd0` has still not been called: the run
failed one step earlier, at identity creation. Three Windows runs in, the
mechanism remains **unmeasured**.

---

## Iteration 4 — every day ran. The mechanism holds; the prize question is open but favourable

Run **30611742558**, job id `91095884891`, commit `4a1cba2`-era tree. Step 5
**success**, 223 `SPIKE|` lines.

```
SPIKE|SUMMARY|met=31|not_met=1|not_run=0
```

All nine days were exercised in one job. What follows is what the log actually
shows, negative findings first.

### NEGATIVE 4.1 — raw sockets and promiscuous mode are NOT blocked

```
SPIKE|NOTE|wfp.block.promiscuous.error|FwpmFilterAdd0 failed: code=2150760487 (0x80320027) block promiscuous
```

`0x80320027` is `FWP_E_TYPE_MISMATCH`: `FWPM_CONDITION_ALE_PROMISCUOUS_MODE`
was given an `FWP_UINT8` value and wants `FWP_UINT32`. The filter was **never
added**, in every stage of the run.

This matters more than a typo. Everything else in this log is evidence about
`ALE_AUTH_CONNECT` — the *connect* path. A process that can open a raw socket
composes its own packets and never touches that layer, so **for this run the
claim is "it cannot connect anywhere undeclared", not "it cannot put a packet
on the wire"**. The difference is exactly the difference between honest
containment and a filter with a hole in it.

Fixed for the next run (both value types attempted, both reported) and a
separate `IPPROTO_RAW` block added at the same layer. **Until a run shows those
filters added and a raw-socket probe refused, this remains open.**

### NEGATIVE 4.2 — net-event collection cannot be enabled from a dynamic session

```
SPIKE|NOTE|wfp.net_events.enable.error|FwpmEngineSetOption0(COLLECT_NET_EVENTS) failed: code=2150760459 (0x8032000b)
```

`0x8032000B` is `FWP_E_DYNAMIC_SESSION_IN_PROGRESS`. Engine options are not
settable over a dynamic session — and the dynamic session is exactly what makes
teardown survive a killed supervisor (4.6). The audit lane worked anyway
*because collection was already on* on this runner.

**A shipping implementation cannot rely on that.** It needs a second,
non-dynamic engine handle purely to set the option, and must restore it
afterwards. Named now rather than discovered on a customer's host where
collection is off and the audit lane silently returns nothing.

### NEGATIVE 4.3 — UDP containment has no client-visible signal

```
SPIKE|ASSERT|core.udp.client|NOT-MET|expected=send_to fails|observed=sent=true os_error=0
SPIKE|ASSERT|core.udp.oracle|MET|expected=destination received 0 datagrams|observed=sightings=0 []
SPIKE|NETEVENT-LIVE| kind=classify-drop remote=10.1.0.101 port=51517 proto=17 sid=…-1003 filter_id=72041 layer=48
```

The single `not_met` in the run. Read together, the three lines say something
sharper than "a test failed": **the datagram was dropped by the kernel and the
sender was told it succeeded.** `send_to` returned success with `os_error=0`;
the destination received nothing; the drop is in the audit lane with the right
address, port, protocol 17 and the run SID.

So containment held for UDP. What does not exist is any way for the *contained
process* to observe it. Had this spike followed honesty rule 2 less strictly and
checked only the client's error, it would have concluded UDP was uncontained —
the exact inverse of the truth. This is the clearest vindication in the run of
insisting on a destination-side oracle.

Consequence for the product: **`assert_no_egress` on Windows must never be
implemented against client-visible errors.** The oracle is the audit lane.

### NEGATIVE 4.4 — the window enumeration was broken, so question 2's evidence is weaker than it looks

```
SPIKE|CHILD| GUI|WINDOW-LIST-FAILED|before-launch find element time out
SPIKE|CHILD| GUI|WINDOW-LIST-FAILED|after-own-launch find element time out
SPIKE|CHILD| GUI|Q2-WINDOW-NOT-FOUND|find element time out
```

The line meant to be the most informative in the whole stage produced nothing.
`create_matcher()` is a poll-until-match loop, so with no filter it never
matches and times out; that is a defect in the probe, not a fact about the
boundary. It failed even *after* question 1 had found and driven a window, which
is proof the enumeration — not the tree — was at fault.

That leaves question 2 resting on one negative, whose error string (`find
element time out`) is **the same string the broken enumeration produced**. The
result is therefore recorded as *consistent with* cross-identity refusal and
**not** as established.

Fixed two ways for the next run: enumeration now uses
`find_all(TreeScope::Children, true-condition)`; and the supervisor runs the
**same query from the original user at the same moment** as a control. If the
original user sees the window and the contained identity does not, question 2 is
answered. Without that control it is not.

### POSITIVE — days 1–3: the mechanism works, and the app-id escape is closed

```
SPIKE|ASSERT|core.declared.client|MET|observed=connected=true os_error=0
SPIKE|ASSERT|core.declared.oracle|MET|observed=sightings=2 [...payload="fp-spike"...]
SPIKE|ASSERT|core.undeclared.client|MET|observed=connected=false os_error=10013
SPIKE|ASSERT|core.undeclared.oracle|MET|observed=sightings=0 []
SPIKE|ASSERT|core.grandchild.client|MET|observed=connected=false os_error=10013
SPIKE|ASSERT|core.external.client-side-only|MET|observed=connected=false os_error=10013
SPIKE|ASSERT|canary.runs-as-run-identity|MET|observed=CANARY|WHOAMI|child|fp-spk-2076-core
```

Both oracles agree with both clients. The declared destination was reached and
**saw the payload**; the undeclared destination saw **zero** connections while
its client got a refusal.

**The grandchild result is the one that matters most.** `canary.exe` → `cmd.exe`
→ `canary.exe` is refused identically to its parent. That is the concrete
demonstration that a per-run identity closes the escape an app-id filter leaves
open, which was the entire architectural premise.

**The blocked-connect error code is confirmed: `10013` = `WSAEACCES`**, on the
undeclared on-host destination, on the external destination, and for the
grandchild. It was flagged unverified in the brief; it is now measured. A
blocked connect is distinguishable from an ordinary network failure, so audit
quality does not collapse.

Note also `loopback=true` on the drop records for `10.1.0.101` — the runner's
own NIC address takes the loopback path and **was still classified and
dropped**. The loopback question the log promised to answer is answered:
`ALE_AUTH_CONNECT` sees it.

### POSITIVE — day 4: the audit lane carries what an audit needs

```
SPIKE|ASSERT|core.audit.drop-carries-address-and-port|MET|observed=kind=classify-drop
  remote=10.1.0.101 port=56209 proto=6 sid=S-1-5-21-…-1003 filter_id=72041 layer=48
  loopback=true flags=0x57f
```

Remote address, remote port, protocol, the **run identity's SID**, and the id of
the filter that did it. Both paths agreed: the live `FwpmNetEventSubscribe4`
subscription and the independent `FwpmNetEventEnum5` readback returned the same
four drops.

### POSITIVE — day 5: the negative control inverts cleanly

With the block filter deliberately omitted and nothing else changed:

```
SPIKE|ASSERT|negative.undeclared.client|MET|observed=connected=true os_error=0
SPIKE|ASSERT|negative.undeclared.oracle|MET|observed=sightings=2 [...]
SPIKE|ASSERT|negative.grandchild.client|MET|observed=connected=true os_error=0
SPIKE|ASSERT|negative.udp.oracle|MET|observed=sightings=1 [...]
SPIKE|NOTE|netevent.attributed-to-run-identity|0
```

Every probe that was refused under enforcement now connects, both oracles see
the traffic, and **zero** drops are attributed to the run identity. The failure
path is exercised, not assumed: this run can tell contained from uncontained.

### POSITIVE — day 6: teardown survives an outright kill

```
SPIKE|HOLDER| HOLD|READY|149171802790184517201392271000588005076|3|fp-spk-9208-hold
SPIKE|ASSERT|teardown.abrupt.filters-existed-first|MET|observed=3 filters
SPIKE|ASSERT|teardown.abrupt.filters-gone|MET|observed=0 remain
```

Three filters confirmed present *before* the kill — so "0 remain" cannot be read
as "never added" — then `TerminateProcess`, then zero remaining as counted
through a **fresh** engine handle. The dynamic session does what it claims.

`core.teardown.clean-close` and `negative.teardown.clean-close` also both MET.

### THE SPIKE — days 7–9: question 1 passes

```
SPIKE|CHILD| GUI|USERNAME|fp-spk-2076-gui
SPIKE|CHILD| GUI|UIA-INIT|ok
SPIKE|CHILD| GUI|Q1-LAUNCH|notepad.exe pid=6836
SPIKE|CHILD| GUI|Q1-WINDOW-FOUND|name="Untitled - Notepad" class="Notepad"
SPIKE|CHILD| GUI|Q1-EDIT-FOUND|15
SPIKE|CHILD| GUI|Q1-SEND-TEXT|ok
SPIKE|CHILD| GUI|Q1-READBACK|contains_marker=true value="fp-spike-inside-boundary"
SPIKE|ASSERT|gui.q1.KILL-CRITERION.drives-own-gui-app|MET
SPIKE|ASSERT|gui.q3.launched-app-carries-run-identity-sid|MET|…SID …-1006
```

**A process running as a freshly created, unprivileged, per-run local user
initialised a UI Automation client, launched a GUI application, found its
window, typed into its editor control, and read the text back.** The readback is
what makes this a result rather than an API call that returned `Ok`.

**The kill criterion did not fire.** Question 1 was the one that could have
ended this: had it failed, the fused claim would be dead and no amount of WFP
work would revive it. It passed.

Question 3's supporting evidence is exact rather than argued: the GUI app's
token user SID is the run identity's SID, and that SID is precisely what the
WFP filters are scoped to. A GUI app launched inside the boundary is inside the
containment scope **by construction**, not by hope.

### What this does NOT say

**Notepad is a fair proxy for the identity boundary and a poor one for SAP GUI.**
Question 1's pass is evidence that the identity boundary does not, in itself,
prevent a contained process from driving a GUI app. It is **not** evidence about
SAP GUI, and reporting it as such would be precisely the overclaim this codebase
exists to prevent.

Untouched, and untouchable on a hosted runner: SAP GUI's licensing under a
freshly created local user; its COM registration (`SAPGUI` / `SapROTWr`) being
per-user or per-machine; and whether it needs a populated user profile that a
`NetUserAdd` account created seconds earlier does not have. Also untested:
Citrix Receiver/Workspace, which adds its own session broker to the same
question.

The prior pass put ~40% on the fused claim surviving contact with SAP GUI. This
run removes the *identity-generic* half of that risk and leaves the
*SAP-specific* half exactly where it was.

### Confirmed operational facts, for whoever builds this

| Fact | Value |
|---|---|
| blocked-connect error | `WSAEACCES` (10013), TCP connect |
| UDP blocked | drop recorded; `send_to` still returns success |
| runner identity | `runneradmin`, RID 500, elevated |
| `SeAssignPrimaryTokenPrivilege` | **not held** → `CreateProcessAsUserW` unusable |
| spawn path actually used | `CreateProcessWithLogonW` |
| logon type accepted | `INTERACTIVE` |
| filters per boundary | 3 (1 permit, 2 block) |
| drop record fields | remote addr, port, protocol, user SID, filter id, layer, loopback flag |

---

## Iteration 5 — question 2 established, and a self-inflicted wound worth keeping

Run **30613043532**, job id `91100043088`. Step 5 **success**, 217 `SPIKE|` lines.

```
SPIKE|SUMMARY|met=29|not_met=3|not_run=0
```

### NEGATIVE 5.1 — the raw-socket block adds cleanly and denies everything (SELF-INFLICTED)

The three `not_met` are all the same cause:

```
SPIKE|NOTE|wfp.block.promiscuous|72728
SPIKE|ASSERT|core.declared.client|NOT-MET|expected=CONNECTED|observed=connected=false os_error=10013
SPIKE|ASSERT|core.declared.oracle|NOT-MET|expected=destination saw >=1|observed=sightings=0 []
SPIKE|ASSERT|core.audit.drop-carries-address-and-port|NOT-MET|observed=no such record (0 attributed records, 0 total)
SPIKE|CHILD| CANARY|child.udp.undeclared(bind-failed)|10.1.0.148:55571|REFUSED|os_error=10013
```

Iteration 4 fixed the `FWP_E_TYPE_MISMATCH` by switching the
`FWPM_CONDITION_ALE_PROMISCUOUS_MODE` value from `FWP_UINT8` to `FWP_UINT32`.
The filter then **added successfully — id 72728 — and blocked every socket the
contained identity tried to open.** The declared destination became
unreachable, UDP `bind` itself failed with 10013, and the audit lane recorded
**zero** events because no connection ever reached `ALE_AUTH_CONNECT`.

The condition is not evaluable as written, so the filter collapses to its
remaining condition — the user id — and means "block every socket bind by this
user".

**This is honesty rule 8 in its purest form.** The previous iteration's version
*failed* to add and said so honestly. This iteration's version *succeeded* and
was catastrophic. `FwpmFilterAdd0` returning success carried no information
whatever about whether containment was correct; only the canary — a probe to a
declared destination that must succeed — caught it. Had this spike concluded
"contained" from a filter id, it would have shipped a configuration that denies
the agent all network access and calls it containment.

**Resolution.** The `ALE_RESOURCE_ASSIGNMENT` block is **not added**. The code is
left in `wfp.rs`, unused and annotated, because deleting it would delete the
evidence. Raw sockets are closed by a better mechanism anyway — see 5.2.

### POSITIVE 5.2 — raw sockets are refused, for a sturdier reason than a filter

```
SPIKE|CHILD| CANARY|child.rawsocket|AF_INET/SOCK_RAW/IPPROTO_RAW|REFUSED|os_error=10013
SPIKE|CHILD| CANARY|grandchild.rawsocket|AF_INET/SOCK_RAW/IPPROTO_RAW|REFUSED|os_error=10013
SPIKE|ASSERT|core.rawsocket.refused|MET
SPIKE|ASSERT|negative.rawsocket.refused|MET
```

Child and grandchild both refused, `WSAEACCES`. Note it is `MET` in the
**negative control too**, where no block filter exists at all — which is the
proof that the refusal does not come from WFP.

It comes from the identity: **creating a raw socket on Windows requires
Administrator, and the per-run identity is an unprivileged member of `Users`.**
That is a stronger guarantee than the filter would have been, because it does
not depend on getting a WFP condition right — as 5.1 demonstrates is easy to get
wrong. The over-determination is stated in the assertion text itself so no
reader can mistake which mechanism is doing the work.

### POSITIVE 5.3 — question 2 is now ESTABLISHED, not merely consistent

The control iteration 4 lacked:

```
SPIKE|ASSERT|gui.q2.CONTROL.foreign-window-visible-to-its-own-identity|MET
  observed=visible=true | name="fpforeign7196.txt - Notepad" class="Notepad" pid=7344

SPIKE|CHILD| GUI|Q2-ENUM-VISIBLE|false | absent from 1 top-level windows:
  ["name=\"C:\\fp-spike-7196\\canary.exe\" class=\"ConsoleWindowClass\" pid=4396"]
```

**The same query, at the same moment, from two identities.** The original user
sees `fpforeign7196.txt - Notepad`. The contained identity enumerates the
desktop and sees exactly **one** top-level window — its own console. The foreign
window is not refused on interaction; it is **not in the tree at all**.

And the enumeration is now known to work, which is what iteration 4 could not
say:

```
SPIKE|CHILD| GUI|WINDOW-LIST-COUNT|before-launch n=1
SPIKE|CHILD| GUI|WINDOW-LIST-COUNT|after-own-launch n=2
```

One window before it launched Notepad, two after. A working probe returning
"absent" is evidence; a timeout was not.

**Question 2 is answered NO, as expected.** The constraint every later design
must live with: *a fused flow must launch its GUI application inside the
boundary.* Attaching to an already-running SAP GUI session started by the
logged-in user is not possible under this design.

### POSITIVE 5.4 — question 1 reproduces

```
SPIKE|CHILD| GUI|Q1-READBACK|contains_marker=true value="fp-spike-inside-boundary"
SPIKE|ASSERT|gui.q1.KILL-CRITERION.drives-own-gui-app|MET
```

Second consecutive run. Not a fluke.

---

# VERDICT

**Nine days complete.** The kill criterion did not fire.

## Negative findings first

1. **The raw-socket/promiscuous filter must not be used as written.** It adds
   successfully and denies the contained identity every socket, declared
   included (5.1). Raw sockets are instead closed by the run identity being
   unprivileged (5.2) — sturdier, because it cannot be got wrong.
2. **UDP containment is invisible to the contained process.** `send_to` returns
   success on a datagram the kernel drops (4.3). `assert_no_egress` on Windows
   must be implemented against the audit lane, never against client errors.
3. **Net-event collection cannot be enabled from a dynamic session** (4.2). A
   shipping implementation needs a second, non-dynamic handle to turn it on and
   restore it. This spike's audit lane worked only because collection happened
   to be on already.
4. **`CreateProcessAsUserW` is unavailable on a host that does not grant
   `SeAssignPrimaryTokenPrivilege`** — including GitHub's elevated RID-500
   Administrator (3c.2). The `CreateProcessWithLogonW` path works but needs the
   Secondary Logon service and cannot be used from a process running as
   LocalSystem, so a flowproof runner installed *as a Windows service* needs
   this arranged deliberately.
5. **Cross-identity GUI control is impossible** (5.3). Every fused flow must
   launch its GUI application inside the boundary. This changes what an adopter
   has to do, not merely what flowproof does.
6. **Administrator is required**, on every run, to add the filters. Linux gets
   its containment unprivileged; Windows does not. This must be said in the same
   breath as the claim, always.
7. **An escalation, unrelated to the mechanism:** the adversary gate refuses any
   pull request over ~128 KiB and reports a tooling failure as a review refusal
   (3.2). Protected path; a human must fix it.

## The answer

**Build it.** The mechanism works and the obstacle that could have killed it did
not.

Measured, not assumed, across two consecutive Windows runs:

* a declared destination is reachable and **the destination sees the payload**;
* an undeclared destination is refused with `WSAEACCES` (10013) and **the
  destination sees nothing**;
* **a grandchild through `cmd.exe` is refused identically** — the escape that
  makes app-id filtering useless is closed by the per-run identity;
* UDP to an undeclared host is dropped, with the drop recorded;
* the drop record carries **remote address, port, protocol, the run identity's
  SID, and the filter id**;
* filters are gone after the supervisor is **killed outright**, verified through
  a fresh engine handle, with their prior existence confirmed first;
* with the block filter deliberately removed, **every one of those inverts** —
  the failure path is exercised, not assumed;
* a process under a freshly created, unprivileged, per-run local user
  **launched a GUI app, drove it through UI Automation, and read its own text
  back**.

## The sentence the product could then say

> On Windows, an agent under test runs as a dedicated per-run identity under a
> kernel-enforced default-deny egress filter. It reached only the destinations
> the flow declared; every other attempt was refused by the kernel and recorded
> with its address, port and protocol. It drove the desktop application under
> test, which ran inside the same boundary. Establishing that boundary requires
> Administrator on the host.

## The sentence it still could not say

> This agent drove **SAP GUI** and provably touched nothing else.

Not because containment failed — it held — but because **Notepad is a fair proxy
for the identity boundary and a poor one for SAP**. Question 1's pass removes
the *identity-generic* half of the risk. It says nothing about the half that is
specifically SAP's.

## The one unknown left, and what it costs to close

**Does SAP GUI run usably under a freshly created, unprivileged local user?**
Three named sub-questions, none testable on a hosted runner:

1. **Licensing** — whether SAP GUI's licensing is per-user and whether a
   throwaway account satisfies it.
2. **COM registration** — whether `SAPGUI` / `SapROTWr` are registered per-user
   (`HKCU`) or per-machine (`HKLM`). Per-user registration would mean each
   per-run identity needs its own, which changes provisioning from "create a
   user" to "create and prepare a user".
3. **Profile state** — whether SAP GUI needs a populated user profile, connection
   entries and saved settings that an account created seconds earlier lacks.

**Cost to close: one Windows VM with a licensed SAP GUI install, and roughly two
to three days.** The harness already exists and is parameterised; it needs the
app name and window title changed and a real host to run on. That is the only
remaining gate before the fused claim can be made, and it cannot be closed on
GitHub Actions at any price.

Citrix is untested and adds its own session broker to the same question. Treat
it as a separate, later investigation, not as covered by this one.

## Recommendation on spend

The prior estimate was 6–10 engineer-weeks to full parity, with ~90% confidence
on the mechanism and ~40% on the fused claim. The mechanism is now measured
rather than estimated, and the identity-generic half of the fused-claim risk is
retired.

**Do not spend the 6–10 weeks yet.** Spend the two to three days on a licensed
SAP GUI host first. It is the cheapest possible test of the only remaining
unknown, and it is the one that decides whether the differentiating sentence is
sayable at all. If SAP GUI runs under a per-run identity, the rest is
engineering with the risk already retired. If it does not, what remains is
contained headless agents on Windows — worth having, but not differentiated, and
not worth a quarter of the only engineer's time.

---

## Housekeeping — numbering, and one unverified addition

**Finding numbers were colliding.** Two earlier sections had been renumbered
into the `4.x`/`5.x` namespace that the verdict's cross-references point at, so
"(3.2)" and "(2.2)" in the verdict resolved to the wrong findings. The two
earlier sections are now `3b` and `3c`, every finding number is unique, and the
verdict's references were corrected to match. No finding text changed.

**`Engine::enable_net_events_globally` is added and is UNVERIFIED.** It is the
fix named by NEGATIVE 4.2 — a second, non-dynamic engine handle for setting
`FWPM_ENGINE_COLLECT_NET_EVENTS`, because a dynamic session refuses engine
options with `FWP_E_DYNAMIC_SESSION_IN_PROGRESS` (0x8032000B). It is written out
so whoever builds this does not have to rediscover it.

**No CI run has exercised it.** `establish()` still calls the dynamic-handle
path, and the runs behind this spike's verdict got their audit records because
collection happened to already be on that runner. Marked unverified in the
doc comment as well as here, because a reference implementation that reads as a
measured result is exactly the overclaim this codebase exists to prevent.

Local gate at this commit, exit codes captured separately, never piped:

| Command | Exit |
|---|---|
| `cargo check -p wfp-spike --all-targets --target x86_64-pc-windows-msvc` | `0` |
| `cargo clippy -p wfp-spike --all-targets --all-features --target x86_64-pc-windows-msvc` | `0`, 0 warnings |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | `0` |
| `cargo fmt --all --check` | `0` |
| `cargo test -p wfp-spike --all-features` (Linux) | `0` |

---

## Iteration 6 — finding 2.2 closed, with human authorisation

The escalation in finding 2.2 was put to a human, who authorised the
protected-path change. Recorded here because the authorisation is the part that
matters: `scripts/gate/` is constitution-protected precisely so that a loop
cannot fix the gate that is refusing its own work, and this change was made only
after being asked for and granted.

### The defect, restated

`scripts/gate/adversary-review.sh` passed the whole prompt — including the diff —
as a **single command-line argument**. Linux caps one argv entry at
`MAX_ARG_STRLEN` = 128 KiB, independently of the far larger `ARG_MAX` (2 MiB
here). Past that, `claude` cannot be exec'd at all, the script observes rc 126,
and reports:

```
::error::correctness: the reviewer exited 126; refusing
```

**A gate that never ran, reading as a gate that said no** — and self-concealing,
because the bigger the change the more an unexplained refusal looks earned.

### The fix

The prompt is written to a temp file and reaches the reviewer on **stdin**
(replacing the previous `< /dev/null`). Verified that `claude -p` reads a prompt
from stdin before relying on it, rather than assuming:

```
printf 'Reply with exactly the word PONG and nothing else.' > p.txt
claude -p --output-format text < p.txt      # EXIT=0, output: PONG
```

Exit codes 126 and 127 are now reported as a **tooling failure**, explicitly
"not a review of your change". The verdict is unchanged — still `exit 1`,
still fail-closed — because fail-closed must not bend for the reason. Only the
message distinguishes, so a maintainer cannot read a broken gate as a judgement.

### The test, and its non-vacuity

`CLAUDE.md`: a fix ships with the test that proves it stays fixed.
`adversary-review.test.sh` gains a case that builds a **625,017-byte** diff with
git plumbing (`hash-object` / `mktree` / `commit-tree`) — no working-tree change,
no branch, two unreferenced objects left behind — and drives the real script over
it. The fixture's size is asserted before the behaviour, so the case cannot pass
vacuously by being too small.

Demonstrated sensitivity, per `CHARTER.md` §6 — same fixture, same stub, same
approving reply, only the script differs:

| script | exit | output |
|---|---|---|
| `HEAD` (before the fix) | `1` | `::error::correctness: the reviewer exited 126; refusing` |
| after the fix | `0` | `correctness: approve` |

The first attempt at this comparison ran the old script from a scratch directory
and got `exit 2, no such lens: correctness` — `dirname "$0"` had resolved
elsewhere, so the thing under test never ran while the non-zero exit satisfied
"it failed". Honesty rule 1, caught and redone rather than reported.

### Locally verified (exit codes captured separately, never piped)

| Command | Exit |
|---|---|
| `bash scripts/gate/adversary-review.test.sh` | `0` (12 cases, incl. the 625 KB diff) |
| `bash scripts/gate/ratchets.test.sh` | `0` |
| `bash scripts/gate/constitution-check.test.sh` | `0` |
| `AUTHOR=AminChirazi CHANGED_FILES="…" constitution-check.sh` | `0` — flags the protected paths, allows the human author |

No CHANGELOG entry: that file records product-facing change, and no previous
gate-infrastructure change appears in it. Following the existing convention
rather than inventing one.
