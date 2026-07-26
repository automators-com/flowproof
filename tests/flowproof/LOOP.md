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
| flowproof version | **0.7.0** (npm) |
| specs committed | **1** (`goose/smoke.flow.yaml`) |
| traces recorded | **1**, re-recorded ~10× while characterising B1 |
| **does it replay green?** | **NO — blocked by B1** |
| CI state | **no job committed** (correct — nothing replays green) |
| iterations with no green trace | **4** |

## Current verdict

flowproof's interception works on goose for about 4 lines of glue — that part is a
genuine success and the measurements back it. But goose still cannot be recorded,
because `record` **non-deterministically drops a model call it forwarded and got an
answer for**, and it exits 0 while doing so. The drop is driven by upstream
latency, reproduces with **no model and no network** (a canned responder with a
2.5 s delay drops in 2 of 6 runs; the same responder with no delay drops in 0 of
6), and is the single thing between this suite and a green gate.

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

## B1 — THE BLOCKER

> **Correction (iteration 4).** Iteration 3 committed the claim that B1 was
> *positional cassette matching meeting a concurrent client*. **That was wrong.**
> It was inferred from a correlation between arrival order and turn count, without
> isolating the upstream. Isolating it reversed the result. The matcher is not
> implicated, and the fix approved on that basis — matching unconsumed turns by
> body — would not have fixed anything. Corrected account below.

### What is actually true

`record` **non-deterministically drops a model call that it forwarded and got an
answer for**, and whether it does depends on **upstream latency**.

Minimal reproduction, needing **no model, no network and no TLS** — a canned
local responder with an artificial delay (`spike/probe_server.py`, `PROBE_DELAY`):

| upstream | runs | turns captured |
|---|---|---|
| canned, instant | 6 | **2, 2, 2, 2, 2, 2** — never drops |
| canned, 2.5 s delay | 6 | **2, 1, 1, 2, 2, 2** — drops in 2 of 6 |
| real model (~1–3 s) | 6 | **1, 2, 1, 2, 2, 2** — drops in 2 of 6 |

goose makes two calls (the task, and a session-title call it makes even under
`--no-session`). Both are forwarded — confirmed at the upstream, which sits
*behind* flowproof's proxy — and both are answered. Only one reliably reaches the
cassette. `record` exits **0** every time.

Replay then fails whichever way the race went:

- 1-turn cassette → `FAIL: turn 2: the system under test made 2 model calls, the recording has 1`
- 2-turn cassette → `FAIL: turn 1: message 0 (system) content changed` (it recorded the title call as turn 1)

### What ruled out the alternatives

| suspected | test | verdict |
|---|---|---|
| my TLS relay's chunked encoding | rewrote it to buffer with `content-length` | still drops — **not the relay** |
| the relay at all | canned upstream, relay removed from the path | still drops when slow — **not the relay** |
| the real model | canned responder, zero model calls | still drops — **not the model** |
| recorder is generally lossy | `weather-node`, plus purpose-built trailing / concurrent / streaming agents in `spike/` | all record 2-for-2 — **not general** |
| positional matching | isolated upstream | **disproven — this was the iteration-3 error** |

### Mechanism: inferred, NOT yet proven

Reading `crates/flowproof-cli/src/agent_flow.rs:1071`, record does:

```rust
let run = plan.drive(&proxy)?;   // returns when the agent process exits
let cassette = proxy.captured(); // read immediately
drop(proxy);
```

and `agent_proxy.rs` pushes a turn to `captured` only *after* `forward()` returns
from the upstream. So a call the agent fired but did not wait for is still being
forwarded when the agent exits, and `captured()` reads before the push lands. A
fast upstream wins the race; a slow one loses it. At replay the cassette is served
instantly, so the same call always completes and always counts — which is exactly
the record/replay asymmetry observed.

**This is consistent with every measurement above but has not been proven
directly.** Proving it means draining in-flight requests before `captured()` and
re-running the slow-canned experiment; the fix is correct iff that goes 6-for-6.

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
| `goose/smoke.flow.yaml` | YES | **NO** — B1 |

Committed as the reproduction for B1, **not** as a passing test. No CI job, because
nothing would pass. Priority flows 1–4 (`assert_no_egress`,
`assert_no_secret_leak`, `mcp:` destructive guard, `strict: true`) are **not
started** — the gate forbids it.

---

## Iteration log

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
