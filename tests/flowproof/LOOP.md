# goose adoption loop — memory

**This file is the only memory between iterations. Rewrite it at the end of every
iteration.**

Question the loop exists to settle: does flowproof deliver real value on an agent
we do not own? Representative third-party adopter: **goose** (aaif-goose/goose,
Apache-2.0, Rust). goose is a **black box** — do not read its internals to make a
test pass.

---

## Status at a glance

| | |
|---|---|
| goose tag (PINNED) | **v1.44.0** |
| flowproof version | **0.7.0** + three fixes (D1 `26a5922`, D2 `854a9f1`, D3 `a7338b7`) |
| specs committed | **2** (`goose/smoke.flow.yaml`, `goose/no-egress.flow.yaml`) |
| traces recorded | **2**, real, against `claude-opus-5` |
| **flows GREEN** | **2 of 4** — smoke (record 6/6, replay 3/3) and flow 1 `assert_no_egress` (replay 3/3, containment enforced) |
| CI state | **no job committed** — blocked on a RELEASE, not on the suite (see B4) |
| iterations with no green trace | **4, then green on the 5th** |

## Current verdict

**flowproof works on an agent we do not own, and the highest-value guarantee is
live:** goose reaches nothing off this host, certified deterministically at zero
model cost, replaying green offline 3/3 with seccomp containment enforced.
Adoption cost is still 4 lines of glue and zero reads of goose's internals — it
was never the obstacle. What was: **six defects in flowproof**, five now fixed,
none of them reachable from flowproof's own examples, because each needs an agent
that does something the harness did not expect (a concurrent side call, or a
worker thread).

---

## Adoption cost so far

| measure | value |
|---|---|
| wall-clock, iteration 1 (spike) | ~6 min |
| wall-clock, iteration 2 (first record) | ~35 min |
| wall-clock, iteration 3 (mis-diagnosed B1) | ~25 min |
| wall-clock, iteration 4 (corrected B1, built minimal repro) | ~30 min |
| **time to first GREEN replay** | **not reached** |
| **adoption glue** (what a real goose adopter writes) | **4 lines**, all in the spec — 1 line of `sh -c "IFS=; …"` command form, 3 lines of clock-freezing env. Plus the `libfaketime` system package. No entry point, no wrapper, no `config.yaml` templating. |
| environment workaround (NOT adoption glue) | 69 lines — `spike/tls_relay.py`, needed only because this container has a TLS-terminating proxy (B2) |
| diagnostic code (NOT adoption glue) | ~200 lines in `spike/` — probe server, marker MCP server, and three throwaway agents used to isolate B1 |
| **reads of goose SOURCE** | **0.** Everything from `goose --help`, `goose run --help`, and observed wire traffic. |
| **reads of flowproof SOURCE** | **3.** `Cargo.toml` (TLS stack), `crates/flowproof-trace/src/cassette.rs` (iteration 3, chasing the wrong cause), `crates/flowproof-adapters/src/agent_proxy.rs` + `crates/flowproof-cli/src/agent_flow.rs` (capture path). Worth noting against the black-box discipline: reading the source is what produced the WRONG answer in iteration 3; isolating the upstream from outside is what produced the right one. |

---

## Defects found in flowproof, and their status

All three were found by running a third-party agent. flowproof's own
`weather-node` example records and replays 2-for-2 throughout and could not have
surfaced any of them: each needs an agent that makes a model call the harness
does not expect. goose makes two — its task, and a session-title call it issues
**concurrently** and does **not wait for**, even under `--no-session`.

### D1 — `record` dropped in-flight model calls. FIXED (`26a5922`), verified.

Record read the cassette the moment the agent process exited, while the proxy
pushes a turn only after the upstream answers. A call the agent fired without
waiting for was still in flight, so it was forwarded, answered, and then silently
missing from the trace. `record` exited **0** while doing it.

Latency-driven, so a fast fake upstream hides it entirely. Verified by control —
same tree, same build, back to back, slow canned upstream, 8 runs each:

| binary | turns captured | drops |
|---|---|---|
| control (`26a5922~1`) | `2 1 1 2 2 2 1 2` | **3/8** |
| with `AgentProxy::quiesce` | `2 2 2 2 2 2 2 2` | **0/8** |

### D2 — positional matching could not survive concurrent calls. FIXED (`854a9f1`).

With D1 fixed the cassette was complete and replay *still* failed:

```
FAIL: turn 1: message 0 (system) content changed
  recorded: Generate a short title (four words or less)...
  replayed: You are a general-purpose AI agent called goose...
```

At record the slow upstream call lands second; at replay the cassette answers
instantly and the order inverts. `Cassette::match_turn` now consumes the earliest
unconsumed turn whose body matches byte-for-byte. Bodies are still compared
byte-for-byte, every turn is still consumed exactly once, an extra call still
fails — only the order *between concurrent calls* is no longer asserted, because
the agent does not guarantee it and a recording therefore cannot either.

This overturns "Position is the whole contract" (`cassette.rs`) and the deferral
in `docs/agent-testing.md`: *"Reordering tolerance can be added if the field ever
demands it; nothing has."* The field demanded it on the first third-party agent.

### D3 — `reply` was whichever call landed last. FIXED (`a7338b7`), verified.

`reply` is defined as the last assistant message in the trajectory. goose's
trailing title call is an assistant message, so with capture now correct the
"reply" is often the **title** rather than the answer:

```
error: reply does not contain `Paris`; the agent's final message was `France's capital city`
```

Racy, measured over 3 records: title-last → record FAILS; task-last → record
succeeds. **2 of 3 succeeded.** Before D1 was fixed this was masked, because the
title call was usually dropped.

Replay is unaffected once a good cassette exists — stored turn order is fixed, so
the 3/3 green replays are stable. But `record` is a coin flip, and a spec author
cannot tell a lucky record from a correct one.

**Fixed.** Turns are grouped by system prompt — turns continuing one
conversation share one, a housekeeping call brings its own. The thread with the
most turns wins; ties go to the thread carrying the most request text, because
the working conversation carries the agent's system prompt and tool schemas
while a title call is small. Both halves are order-independent, which is the
point.

Measured after the fix: **record 6/6** (was 2/3), replay unchanged at 3/3.

**It is a heuristic and the limit is stated in the code and the docs:** an agent
whose side conversation is BIGGER than its real one would defeat the tie-break.
A single-system-prompt cassette — the ordinary case — takes the identical path
it always did. Rejected alternatives: stdout (already rejected in the design,
for good reasons) and history-prefix threading (both goose calls are
single-turn, so it does not discriminate).

### D5 — egress containment denied every MULTI-THREADED agent. FIXED, verified.

**The widest-reaching of the six.** Not a goose quirk: containment was broken for
any agent that opens sockets off a worker thread, and the symptom was a refused
connection to an **allowed** destination — which reads as a network fault, not a
containment bug. An adopter would most likely have blamed their own networking.

`dup_child_fd` called `pidfd_open(req.pid)`, but a seccomp notification carries
the **TID** of the calling thread and `pidfd_open` only accepts a thread-group
**leader**. Python and Node do their networking on the main thread, where
TID == TGID; goose is Rust/tokio and does not.

Fix: on `pidfd_open` failure, resolve the leader from `/proc/<tid>/status`
`Tgid:` and retry. Threads share one fd table, so the dup is identical. The
single-threaded fast path is untouched and a real failure on a leader still
reports the original error.

**Measured, not guessed.** Four decision points instrumented behind
`FLOWPROOF_EGRESS_DEBUG`; the policy ALLOWED loopback 8/8 and every one died at
`dup_child_fd`. The standing suspicion was sockaddr parsing — **wrong, the third
wrong inference this loop**, and the reason the instrument went in first. The
helper stays, behind the env var, because it is what made this findable.

Verified: flow 1 records and replays green 3/3; flowproof's own `egress_e2e`
still 3/3 with the real filter; full suite 642 passed, 0 failed.

### D5 (original report) — refused loopback, kept for the isolation record

`docs/agent-testing.md`: *"Loopback (`127/8`, `::1`) is exempt WHOLESALE, so the
model proxy and any local MCP server need not be listed."* It is not exempt for
goose. Containment engages correctly and then goose cannot reach flowproof's OWN
proxy:

```
egress containment: enforced (linux seccomp)
Network error: Could not connect to 127.0.0.1:35035
```

Isolated rather than inferred — same container, same flowproof binary, same
containment:

| agent | reaches the loopback proxy under containment? |
|---|---|
| flowproof's own `egress_e2e` (3 tests, `RUN_EGRESS_E2E=1`, real filter) | **yes** |
| Node agent (`spike/streaming_agent.mjs`) | **yes** |
| **goose (Rust / tokio)** | **NO** |

`libfaketime` is exonerated: removing `LD_PRELOAD` entirely reproduces it
identically. So the loopback exemption covers the syscall path a Python or Node
agent takes to open a socket and not the one goose takes. The mechanism needs
flowproof-side investigation; do NOT chase it in goose's source.

`tests/flowproof/goose/no-egress.flow.yaml` is committed as the reproduction,
**not** as a passing test. Do not make it pass by adding `allow_egress` entries:
loopback is supposed to need none, and declaring what the contract exempts would
hide the finding.

**A correction worth keeping:** CI *does* set `RUN_EGRESS_E2E: "1"` (an `env:`
block under the step). An earlier reading of this file with `grep -B6` cut that
block off and nearly produced a false "CI proves nothing" report. Read CI steps
verbatim.

### D3 addendum — the heuristic does nothing for an agent with no system prompt

Found while isolating D5. `spike/streaming_agent.mjs` sends **no** system message
on either call, so both land in one group, the thread split never happens, and
`reply` falls back to v1 behaviour — it returned the title, `**Capital of
France**`, not `Paris`.

The fix's reach is therefore narrower than "an agent whose side conversation is
bigger would defeat it": it needs the side conversation to carry a DIFFERENT
system prompt. An agent that sends none gets no protection at all. goose does
send distinct system prompts, which is why it works there.

### D7 — the MCP stand-in lost its whole lane on an abrupt shutdown. FIXED, verified.

**Root cause, measured.** The stand-in writes `<server>.out.json` ATOMICALLY AT
STDIN EOF — after joining its pump threads and reaping the real server. goose
terminates its MCP subprocess before that, so the stand-in never reached the
write and the entire recording was lost. flowproof then read the missing file and
reported "the agent never spawned flowproof's MCP stand-in", sending the adopter
to debug wiring that was already correct.

Measured, not inferred, in two commands: listing the run dir immediately after
goose exits showed only `files.plan.json`, no `files.out.json`; four seconds later
still nothing, and no stand-in process alive. The earlier suspicion (flowproof's
double-quoted path breaking goose's `--with-extension`) was WRONG — goose dequotes
it fine, proved by the real server's parent being
`flowproof mcp-stdio --server files`.

**Fix:** persist the lane INCREMENTALLY after each captured call, at both capture
sites. `write_out_atomic` is a temp-file rename, so a partial flush is never
observable, the last write wins, and an abrupt kill can no longer lose the
recording. The EOF write stays authoritative. Full suite: 642 passed, 0 failed.

### D7 (original report) — kept for the isolation record

`record` fails an `mcp:` flow with:

```
error: the agent never spawned flowproof's MCP stand-in for `files`;
its config still points at the real server (point it at ${FLOWPROOF_MCP_SERVER_FILES})
```

It did spawn it. Proved by making the real server log its parent:

```
ppid=8051 parent=/home/user/flowproof/target/release/flowproof mcp-stdio --server files
```

That is the stand-in. goose spawned it, it forwarded to the real server, and
`list_files` came back through it with the real server's content — visible in
goose's own output as `▸ list_files flowproof`, the extension named after the
stand-in binary. Every part of the boundary worked; only the check disagrees.

**Why this one is expensive:** the message tells an adopter to fix wiring that is
already correct. It is the most costly shape of wrong error message — it sends you
to debug the one thing that is working.

Wiring for the record (this took no glue beyond the existing 4 lines):
`--with-extension $FLOWPROOF_MCP_SERVER_FILES` inside the same `IFS=` command
form. flowproof emits the value with the executable path double-quoted
(`"/path/flowproof" mcp-stdio --server files`); goose dequotes it correctly, so
the quoting is NOT the problem.

**What would unblock it:** find what the post-run check actually looks for and why
a genuinely-spawned stand-in fails it. Do not chase this in goose.

### D8 — the model refuses to misbehave, so the guard cannot be recorded. **OPEN, design-level.**

Not a bug — a limit worth writing down, and exactly the case
`docs/agent-testing.md` names under "Making a guard flow prove enforcement, not
compliance".

Ordered outright to `delete_all` with `confirm: true`, goose declined and asked
for confirmation instead:

> "Before I pull the trigger — this deletes `src/main.rs` permanently... Is this
> project pushed to a remote?"

So no adversarial attempt can be recorded, and `assert_no_tool_call: delete_all`
would certify a polite day rather than a control. The doc's own answer applies:
say so rather than ship a flow that looks like a guard.

What IS real here is the `mcp:` `result:` — flowproof answers `delete_all` and the
real server never runs it in either phase, and the real server's marker file
confirms it never executed. That is enforcement in code; the model's restraint is
not.

Options for the next iteration, none of them "record it anyway": prompt-inject
harder and accept it may still refuse; assert the enforcement with a unit test on
the `mcp:` interception instead; or keep flow 3 scoped to the read-only task where
`assert_tool_call: list_files` (liveness) and `assert_no_tool_call: delete_all`
are both honest, while stating plainly that the guard half proves compliance.

### D4 — the released package cannot run this suite. **OPEN until a release ships.**

Both fixes are local. npm `flowproof@0.7.0` has neither, and it also predates
`FLOWPROOF_BIN`, so an adopter cannot point the released launcher at a patched
build — it ignores the variable **silently**, with no error and no warning. That
silence cost a full wrong measurement in iteration 5: a run that reported
`1,2,2,2,1,1` and looked like "the fix failed" was the OLD binary all along. The
only tell is the absent `flowproof: using FLOWPROOF_BIN=...` line the docs
promise on every run.

Consequence: **no CI job can be committed yet.** A CI job pinned to released
0.7.0 would fail; one pinned to a local build would not be testing the released
package. CI is unblocked by a release containing D1 and D2, not by more work here.

## Secondary findings (both worked around, neither blocking)

### B2 — `record` cannot traverse a TLS-terminating proxy

Forwards with compiled-in roots; ignores `SSL_CERT_FILE` **and** the system trust
store (verified with `update-ca-certificates`). Dies with `invalid peer
certificate: UnknownIssuer`. Affects any corporate MITM network. Worked around
with `spike/tls_relay.py` (plain HTTP → HTTPS, 69 lines).

### B3 — goose is non-deterministic at the model boundary by construction

Every user message is prefixed with a `<turn-context>` block containing the current
time at minute resolution. Strict matching cannot survive it. Frozen with
`libfaketime`; **`FAKETIME_DONT_FAKE_MONOTONIC=1` is mandatory** or goose's async
timers never fire and the run hangs forever.

Generalises past goose: any agent stamping time, a session id, or a working
directory into its prompt is untestable by v1 without an `LD_PRELOAD` trick that
most adopters will not think of and that does not exist on macOS.

### Smaller notes

- flowproof's `command:` tokenizer consumes double quotes and does not treat single
  quotes as grouping, so `sh -c '…'` reaches `sh` as `'goose` → `Unterminated
  quoted string`. Every shipped example is a bare executable, so this was
  unexercised. Worked around with `IFS=`, which needs no inner quotes.
- `report.json` carries no agent stdout/stderr, so diagnosing a divergence means
  re-running with output redirected inside the spec's own command.
- Credit where due: *"the agent made no model calls; it exited 2 without talking to
  the proxy"* named the real cause instantly, twice.

---

## Flows committed

| flow | recorded? | replays green? |
|---|---|---|
| `goose/smoke.flow.yaml` | **YES**, against `claude-opus-5` | **YES — 3/3 offline** |

The gate is **passed**: one real recorded trace, replaying green offline with the
relay down and `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `FLOWPROOF_AGENT_UPSTREAM`
and `OPENAI_BASE_URL` all unset. goose runs for real; the cassette drives it.

Requires the patched binary (D1 + D2). No CI job yet — see D4, which is a release
problem, not a suite problem. Priority flows 1–4 (`assert_no_egress`, `assert_no_secret_leak`, `mcp:`
destructive guard, `strict: true`) are **now unblocked** and none is started.
Flow 1 CAN run here — this container is Linux.

---

## Iteration log

### Iteration 11 — D9 fixed; flow 3's real blocker measured

**Shipped:** `names_same_tool` in `agent_flow.rs` — the unprotected-tool check now
understands MCP client-side namespacing. 644 passed, 0 failed.

**D9 — FIXED.** An MCP client routinely namespaces a server's tools, so a tool
intercepted under `mcp:` as `delete_all` reaches the model boundary as
`files__delete_all`. Comparing literally warned that a CORRECTLY intercepted tool
was unprotected — a false positive on exactly the flows that did the right thing,
and the worst kind for this particular warning: an adopter who learns to ignore it
stops reading the true ones.

The fix matches only on a namespace SEPARATOR (`__`, `.`, `/`, `:`), never a bare
substring, with two tests pinning both halves: `files__delete_all` IS the same
tool; `soft_delete_all`, `predelete_all` and `delete_all_now` are NOT. Silencing a
real warning would have been worse than the false positive.

**Flow 3's remaining blocker, now MEASURED not guessed:**

```
turn 2: tools offered changed
  recorded: [flowproof__delete_all, flowproof__list_files]
  replayed: []
```

At replay goose is offered **no tools at all**. The recording has both. So the MCP
stand-in is not serving the tool list in replay mode, or goose never completes the
handshake with it. The alternating first failure ("turn 1: message 0 (system)
content changed") is almost certainly the same cause one turn earlier — goose's
system prompt differs when it has no tools — but that is INFERRED; confirm it.

**Next iteration should:** diagnose why the replay lane serves an empty tool list.
Start by dumping the stand-in's replay-mode behaviour (does it answer `tools/list`
from the recorded lane?) and the run dir's `files.out.json` at replay. Log what the
process DID; that method has now solved D5, D7 and pointed at this one.

**Deliberately not built:** any fix for the divergence (one thing per iteration);
flow 4; the fake-model baseline.

### Iteration 10 — D7 fixed; flow 3 records but does not replay

**Shipped:** incremental lane persistence in `mcp_stdio.rs`. The `mcp:` record
that failed every time now succeeds. 642 passed, 0 failed.

**Found:**
- **D9 (new, OPEN):** goose namespaces MCP tools, so `delete_all` reaches the
  model as `flowproof__delete_all`. flowproof's unprotected-tool check does not
  connect the asserted name to the `mcp:` lane, and warns that a CORRECTLY
  intercepted tool is unprotected — a false positive on exactly the flows that did
  it right. Client-side namespacing is standard MCP behaviour, not a goose quirk.
  The loop treats this warning as an error, so flow 3 is not shippable.
- **Flow 3 replay divergence (OPEN):** fails differently each run — "turn 1:
  message 0 (system) content changed", then "turn 2: tools offered changed".
  Smells like a non-deterministic tool list or ordering, but that is INFERRED and
  the last four inferences were wrong. Measure it.

**What held:** the `mcp:` boundary itself. `/tmp/DESTRUCTIVE_RAN` never appeared,
at record or replay — flowproof answered `delete_all` and the real server never
ran it.

**The method that worked, twice now:** log what a process actually DID rather than
reasoning about what it should do. It cracked D5 (parent-process log) and D7 (run
dir listing), each in about two commands, after theorising had produced a
confident wrong answer in both cases.

**Deliberately not built:** any fix for D9 or the divergence (one thing per
iteration); flow 4; the fake-model baseline.

**Next iteration should:** fix D9, or — if it stalls — build the fake-model
baseline instead, which is now the only remaining item that tests the premise
rather than the tool.

### Iteration 9 — flow 3 blocked, two findings

**Shipped:** `spike/files_mcp.py` (a two-tool stdio MCP server, read-only +
destructive, with a marker file that fires if the destructive one ever really
runs) and `goose/mcp-guard.flow.yaml` as a REPRODUCTION, not a passing test.

**Found:** D7, a false-negative stand-in detection that fails a record which did
everything right; and D8, the model refusing to misbehave so the adversarial turn
cannot be recorded.

**Measured, not inferred:** the parent-process log is what turned "flowproof is
probably wrong" into proof. The earlier suspicion — that flowproof's double-quoted
executable path broke goose's `--with-extension` parsing — was WRONG; goose
dequotes it fine. Fourth wrong inference this loop, caught before it was reported.

**Deliberately not built:** a flow-3 that records by pointing goose at the real
server (it would destroy the boundary it exists to prove); any CI job (D4).

**Next iteration should:** fix D7. Start by finding what the post-run check
inspects — the stand-in ran, so something it is expected to leave behind is
missing or is looked for in the wrong place. Then decide D8 on the evidence.

### Iteration 7 — fix D5, flow 1 GREEN

**Shipped:** the `dup_child_fd` thread-group-leader fix, the
`FLOWPROOF_EGRESS_DEBUG` diagnostic, and **flow 1 recorded and replaying green
offline 3/3** with containment enforced. Two of four flows now green.

**Found:** D5 was never goose-specific — egress containment denied *every*
multi-threaded agent, presenting as a network error against an allowed
destination.

**Deliberately not built:** flow 2, and any CI job (still D4: no release carries
the fixes).

**Next iteration should:** flow 2, `assert_no_secret_leak` — plant a fake key in
the env and prove it never reaches the trajectory. It needs no containment and no
new flowproof work, so it is the cheapest next green flow.

### Iteration 5 — fix D1 and D2, pass the gate

**Shipped:** `AgentProxy::quiesce` (D1) and `Cassette::match_turn` (D2), both
committed; **the first green offline replay of a third-party agent**, 3/3.
Tests: `flowproof-trace` 67 passed, `flowproof-adapters` 62 passed with
`--features agent`, 0 failed.

**Found:**
- D1 verified by proper control (worktree at the pre-fix commit, separate target
  dir, main tree never dirtied): 3/8 drops without the fix, 0/8 with it.
- D2 was hiding underneath D1 — fixing capture alone leaves goose unrecordable.
- **D3, new and open:** `reply` picks whichever concurrent call lands last, so
  `record` succeeds 2 times in 3. Masked until D1 was fixed.
- The `agent` cargo feature is **off by default**, so a bare
  `cargo test -p flowproof-adapters` compiles none of the agent-boundary code and
  runs **1** test. CORRECTION: CI is fine — `.github/workflows/ci.yml` runs
  `cargo test --workspace --all-features`. This is a local-developer footgun, not
  a CI gap; an earlier iteration reported it as one.

**Two process errors worth keeping, both mine:**
1. A verification run using `FLOWPROOF_BIN` silently measured the OLD binary
   (0.7.0 predates the flag). It reported what looked like "the fix failed". Only
   the missing announcement line gave it away. **Check the tool honoured your
   override before believing a measurement.**
2. Reverting files in the working tree to build a control fights the commit hook.
   A `git worktree` is the right instrument and cost nothing.

**Deliberately not built:** D3's fix (a semantics decision, escalated); any second
spec; any CI job (D4); the fake-model baseline.

**Next iteration should:** decide D3, then build flow 1 (`assert_no_egress`) —
the highest-value flow, runnable here, and the one no cheap substitute covers.
Then the fake-model baseline, which is now a fair fight: the honest comparison is
a ~100-line scripted server against a harness whose real cost included finding
and fixing three defects.

### Iteration 4 — correct iteration 3, build a model-free reproduction (12:25 → 12:55 UTC)

**Shipped:** the corrected B1 account above, and a minimal reproduction that needs
no model, no network and no TLS — `spike/probe_server.py` with `PROBE_DELAY` set.

**Found:** iteration 3's root cause was **wrong**. The drop is latency-driven, not
ordering-driven. Ruled out, each by isolation rather than argument: my relay's
chunked encoding (rewrote it), the relay entirely (removed it from the path), the
real model (replaced with a canned responder), and positional matching (the
iteration-3 claim).

**The process lesson, worth keeping:** iteration 3 reached for flowproof's source
and reasoned from it to a confident, wrong conclusion. Iteration 4 changed one
variable at a time from outside and got the right one in three experiments. The
black-box discipline this brief imposes on *goose* would have been worth applying
to *flowproof* too.

**Deliberately not built:** the approved matcher fix — it targets a cause that has
since been disproven, so building it would have been waste. No second spec, no CI
job, no fake-model baseline.

**Next iteration should:** prove the inferred mechanism by draining in-flight
requests before `proxy.captured()` in `crates/flowproof-cli/src/agent_flow.rs`,
then re-run the slow-canned experiment. The fix is correct iff it goes 6-for-6 at
2 turns. Only then re-record goose, check the gate, and start flow 1
(`assert_no_egress`, which CAN run here — this container is Linux).

### Iteration 3 — MIS-diagnosed B1 (12:00 → 12:25 UTC) — superseded by iteration 4

**Shipped:** three diagnostic agents in `spike/` that eliminate the wrong
explanations, the six-run measurement table above, and the exact fix site.

**Found:** B1 is not a dropped call — it is **positional matching meeting a
concurrent client**, which makes the cassette itself a coin flip. Also established
that flowproof's recorder is *not* generally lossy (its own example still records
2-for-2), which is what makes the goose result specific and actionable.

**Deliberately not built:** the fix itself — it overturns a documented design
decision and was escalated instead; any second spec; any CI job; the fake-model
baseline.

**Next iteration should:** implement the unconsumed-turn matching fix in
`crates/flowproof-trace/src/cassette.rs`, once the design change is signed off,
then immediately re-run the six-record experiment above — the fix is correct iff
all six produce a cassette that replays green. Only then does the gate open and
flow 1 (`assert_no_egress`, which CAN run here — this container is Linux) begin.

**On the stopping rule:** three iterations have now passed with no green trace,
which is the loop's own termination condition. Reporting it as *"adoption cost is
too high"* would be **wrong and the measurements say so** — adoption cost came in
at 4 lines of glue and zero reads of goose's internals. The honest verdict is that
adoption is cheap and the blocker is a single, located, fixable design assumption
inside flowproof. That distinction is the most valuable thing this loop produced;
do not let it get flattened into "goose didn't work".
