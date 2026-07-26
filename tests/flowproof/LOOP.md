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
| **does it replay green?** | **NO — blocked by B1, now root-caused** |
| CI state | **no job committed** (correct — nothing replays green) |
| iterations with no green trace | **3** |

## Current verdict

flowproof's interception works on goose with ~4 lines of glue, but **goose cannot
be recorded at all**, and the reason is a design assumption rather than a bug:
flowproof matches cassette turns strictly by position, while goose issues its two
model calls *concurrently*, so their arrival order varies run to run and no
recording can reliably describe the next run. Whichever order gets recorded, the
replay may see the other one — measured at **3 of 6 runs producing an unusable
cassette, with `record` exiting 0 every time**. This is the single blocker; the
fix is scoped to one function, and it does not require weakening byte-exact
matching.

---

## Adoption cost so far

| measure | value |
|---|---|
| wall-clock, iteration 1 (spike) | ~6 min |
| wall-clock, iteration 2 (first record) | ~35 min |
| wall-clock, iteration 3 (root-cause B1) | ~25 min |
| **time to first GREEN replay** | **not reached** |
| **adoption glue** (what a real goose adopter writes) | **4 lines**, all in the spec — 1 line of `sh -c "IFS=; …"` command form, 3 lines of clock-freezing env. Plus the `libfaketime` system package. No entry point, no wrapper, no `config.yaml` templating. |
| environment workaround (NOT adoption glue) | 69 lines — `spike/tls_relay.py`, needed only because this container has a TLS-terminating proxy (B2) |
| diagnostic code (NOT adoption glue) | ~200 lines in `spike/` — probe server, marker MCP server, and three throwaway agents used to isolate B1 |
| **reads of goose SOURCE** | **0.** Everything from `goose --help`, `goose run --help`, and observed wire traffic. |
| **reads of flowproof SOURCE** | **2.** `Cargo.toml` (TLS stack, for B2) and `crates/flowproof-trace/src/cassette.rs` (to locate B1's fix site, after the behaviour was already established from outside). |

---

## B1 — THE BLOCKER, root-caused

### What goose does

goose issues **two** model calls for a one-line task: the task itself, and a
session-title generation call. It makes them **concurrently**, and it does this
even under `--no-session`.

### What flowproof does

`Cassette::turn(index, …)` in `crates/flowproof-trace/src/cassette.rs:250` serves
turn *N* by position and refuses to look elsewhere. Its own doc comment states the
contract:

> *"Position is the whole contract: this does NOT scan for a turn that happens to
> fit."*

And `docs/agent-testing.md:832`, under "Settled in review":

> *"Reordering tolerance can be added if the field ever demands it; nothing has."*

**The field now demands it.** This is the first real third-party agent tried, and
it fails on exactly this.

### The measurement

Six consecutive `record` runs, counting calls at the upstream (which sits *behind*
flowproof's proxy, so it sees what flowproof saw) against turns in the cassette:

| run | calls at upstream | turns captured | which call arrived first |
|---|---|---|---|
| 1 | 2 | **1** | task |
| 2 | 2 | **1** | task |
| 3 | 2 | **1** | task |
| 4 | 2 | 2 | title |
| 5 | 2 | 2 | title |
| 6 | 2 | 2 | title |

Perfect correlation with arrival order, and `record` **exited 0 in all six**.

Both outcomes are unusable:

- **1-turn cassette** → `FAIL: turn 2: the system under test made 2 model calls, the recording has 1`
- **2-turn cassette** → `FAIL: turn 1: message 0 (system) content changed` — because turn 1 was recorded as the *title* call, and on replay the *task* call arrived first.

### Why this is flowproof's, not goose's

Ruled out by construction, each with a purpose-built agent in `spike/`:

| hypothesis | agent | result |
|---|---|---|
| recorder drops trailing calls | `trailing_agent.mjs` | 2 calls → **2 turns**. Not it. |
| recorder mishandles concurrency | `concurrent_agent.mjs` | 2 calls → **2 turns** (won the race that sample). Not deterministic. |
| recorder mishandles streaming | `streaming_agent.mjs` | 2 calls → **2 turns**. Not it. |
| flowproof's own example still works | `examples/agent-demo/weather-node.flow.yaml` | 2 calls → **2 turns**. Recorder is not generally lossy. |

The recorder captures fine. What breaks is the *positional* contract meeting a
*concurrent* client.

### The fix, and why it needs a human decision

Match an incoming request against any **unconsumed** recorded turn by byte-exact
body equality, consuming it — instead of by index. This keeps every existing
guarantee (bodies still match byte-for-byte; an edited prompt template still
fails) and relaxes only the assumption that a trajectory is strictly sequential.

**It overturns a decision the docs record as deliberate.** That is a design call,
not a bug fix, so it wants sign-off rather than a quiet patch. The blast radius is
small: one function, plus the divergence-reporting path that currently says "turn
N" and would need to say which recorded turn went unmatched.

---

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

### Iteration 3 — root-cause B1 (12:00 → 12:25 UTC)

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
