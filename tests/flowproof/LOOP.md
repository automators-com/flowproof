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
| goose tag (PINNED) | **v1.44.0** — latest non-RC tag; v2.x are release candidates |
| goose acquisition | prebuilt release binary `goose-x86_64-unknown-linux-gnu.tar.bz2` |
| flowproof version | **0.7.0** (npm) |
| specs committed | **1** (`goose/smoke.flow.yaml`) |
| traces recorded | **1** (`goose/smoke.trace.jsonl`) — REAL, recorded against claude-opus-5 |
| **does it replay green?** | **NO — blocked by a flowproof capture bug (B1 below)** |
| CI state | **no job committed** (correct — nothing replays green yet) |
| iterations with no *green* trace | **2** (of 3 before the loop self-terminates) |

## Current verdict

flowproof can reach into goose cleanly — both interception boundaries work with
essentially no glue — but it **cannot yet produce a replayable cassette for it**,
because `record` captured only 1 of the 2 model calls goose actually made through
flowproof's own proxy, and replay then fails on the count it recorded itself.
That is a flowproof bug, not an adoption cost, and it is the single thing standing
between this suite and a green gate. Two further findings are real but solvable:
goose stamps wall-clock time into every prompt (defeats strict matching until the
clock is frozen), and flowproof's `record` cannot traverse a TLS-terminating proxy.

---

## Adoption cost so far

| measure | value |
|---|---|
| wall-clock, iteration 1 (spike) | ~6 min |
| wall-clock, iteration 2 (record attempt) | ~35 min (11:20 → 11:55 UTC) |
| **time to first GREEN replay** | **not reached** |
| **adoption glue** (things a real goose adopter must write) | **4 lines**, all inside the spec: 1 line of `sh -c "IFS=; …"` command form, 3 lines of clock-freezing env. Plus one system package (`libfaketime`). Still no entry point, no wrapper, no `config.yaml` templating. |
| environment workaround (NOT adoption glue) | 69 lines — `spike/tls_relay.py`, needed only because this container has a TLS-terminating proxy (B2) |
| diagnostic code (NOT adoption glue) | 106 lines — `spike/probe_server.py`, `spike/marker_mcp.py` |
| **reads of goose SOURCE** | **still 0.** Everything came from `goose --help`, `goose run --help`, and observing its wire traffic. One read of its root `Cargo.toml` in iteration 1 for the pinned version. |
| **reads of flowproof SOURCE** | **1 (metadata only).** Grepped `Cargo.toml` files for the TLS stack while diagnosing B2. All behavioural answers came from `docs/agent-testing.md`. |

The wiring, in full, as it actually works:

```yaml
agent:
  command: sh -c "IFS=; exec goose run --no-session --no-profile -t $FLOWPROOF_PROMPT"
  env:
    GOOSE_PROVIDER: openai
    GOOSE_MODEL: claude-opus-5
    LD_PRELOAD: /usr/lib/x86_64-linux-gnu/faketime/libfaketime.so.1
    FAKETIME: "2026-01-01 00:00:00"
    FAKETIME_DONT_FAKE_MONOTONIC: "1"
```

`OPENAI_BASE_URL` is never mentioned — flowproof injects it and goose reads it.

---

## Flows committed

| flow | recorded? | replays green? |
|---|---|---|
| `goose/smoke.flow.yaml` — goose answers a trivial question, no extensions | **YES**, against claude-opus-5 | **NO** — B1 |

This is the gate flow, deliberately minimal: no tools, no extensions, one turn. It
exists to prove the pipeline, and right now it proves the pipeline is broken at the
capture step. **It is committed as the reproduction for B1, not as a passing test.**
No CI job, because nothing here would pass.

Priority order once the gate is passed (unchanged, none started):
1. `assert_no_egress` (Linux-only — and this container IS Linux, so it can run here)
2. `assert_no_secret_leak`
3. destructive-tool guard at the `mcp:` boundary
4. `strict: true`

---

## Blockers

### B1 — **flowproof gap (the one reportable gap this iteration).** `record` captures only the first model call; replay then counts all of them and fails.

**What happens.** goose makes **two** model calls for a one-line task. Both go
through flowproof's proxy — confirmed by logging them at the upstream relay, which
sits *behind* the proxy:

```
CALL /v1/chat/completions nmsgs=2 roles=['system','user'] last='<turn-context>…What is the capital of France?…'
CALL /v1/chat/completions nmsgs=2 roles=['system','user'] last='---BEGIN USER MESSAGES---\nWhat is the capital of France?…\n---END USER MESSAGES---\n\nGenerat…'
```

The second is goose generating a session title. It happens **even with
`--no-session`**. The resulting cassette contains **one** turn:

```
$ python3 -c "import json;print(len(json.load(open('smoke.trace.jsonl'))['cassette']['turns']))"
1
```

And replay fails on flowproof's own arithmetic:

```
FAIL: … — turn 2: the system under test made 2 model calls, the recording has 1
```

**Why this is a bug and not a design choice.** Record and replay disagree about
what counts as a model call. Whatever rule caused the recorder to drop the second
call is not applied by the replayer, so a cassette minted by `record` can never
satisfy `run`. flowproof's docs are emphatic that a record run capturing *nothing*
must fail loudly (agent-testing.md: "A record run that captures nothing FAILS…
that is the one failure a determinism tool must never let through"). A *partial*
capture is the same hazard wearing a disguise — it exits 0, writes a trace, and
looks like success.

**Confirmed not the cause:** goose is not erroring or retrying. With its stdout
captured during replay, goose printed the correct answer, `Paris`, with no retry
message. The trajectory is healthy; only the accounting is wrong.

**What would unblock it:** flowproof capturing every request that traverses its
proxy, or — if some calls are deliberately excluded — excluding them identically at
replay. Either makes this flow green immediately; nothing else about it is broken.

**Do NOT work around this by patching or reconfiguring goose.** An agent making a
follow-up model call it did not tell you about is exactly the third-party behaviour
this loop exists to discover.

### B2 — flowproof `record` cannot traverse a TLS-terminating proxy. Worked around.

`record` forwards to the upstream with a Rust client using compiled-in roots. It
ignores `SSL_CERT_FILE` **and** the system trust store (verified: installed the CA
via `update-ca-certificates`, no change), so in any environment with a MITM proxy —
this container, and most corporate networks — recording dies with:

```
error: recording touched the real model and it failed: io: invalid peer certificate: UnknownIssuer
```

Worked around with `spike/tls_relay.py`, a 69-line plain-HTTP→HTTPS relay that
flowproof talks to over `http://127.0.0.1:8100` so no certificate is involved.
Reported here as secondary because it has a workaround; B1 does not.

### B3 — goose is non-deterministic at the model boundary by construction. Solved, at a cost.

goose prefixes every user message with a `<turn-context>` block containing the
current time at minute resolution:

```
<turn-context>
<current-time>2026-07-26 11:37:00 +00:00</current-time>
<working-directory>/home/user/flowproof</working-directory>
</turn-context>
```

flowproof's matching is strict by design — agent-testing.md, "Settled in review":
*"Cassette matching is strict, by position, with no tolerance holes… a test that
quietly tolerates drift stops being a test."* Normalisation with named holes for
volatile spans was **explicitly proposed and rejected for v1**. So the two are
incompatible until the clock stops moving.

Fixed by freezing the wall clock with `libfaketime`. Two gotchas worth keeping:
freezing `CLOCK_MONOTONIC` too makes goose's async timers never fire and the run
**hangs forever** — `FAKETIME_DONT_FAKE_MONOTONIC=1` is mandatory, not optional.

**This generalises well beyond goose**, and is worth flowproof's attention on its
own: any agent that stamps time, a session id, or a working directory into its
prompt is untestable by flowproof v1 without an `LD_PRELOAD` trick that most
adopters will not think of and cannot use on macOS. The rejected design note
assumed volatile spans come from *tool results*; here it is the agent's own
prompt template.

---

## Iteration log

### Iteration 2 — first record (11:20 → 11:55 UTC)

**Shipped:** `goose/smoke.flow.yaml` and a genuinely recorded
`goose/smoke.trace.jsonl` (against `claude-opus-5`, via Anthropic's
OpenAI-compatible endpoint). `spike/tls_relay.py` as the B2 workaround. Trace
scanned for the key: **zero matches**, exact and prefix.

**Found:**
- **B1 — the reportable flowproof gap:** `record` captured 1 of 2 model calls;
  replay counts 2 and fails. Cassette is unusable, exit status at record was 0.
- **B3:** goose stamps wall-clock time into every prompt; strict matching cannot
  survive it. Solved with libfaketime + the mandatory monotonic exemption.
- **B2:** `record` can't verify a MITM proxy's CA via any standard mechanism.
- flowproof's `command:` tokenizer consumes double quotes and does not treat
  single quotes as grouping, so `sh -c '…'` reaches `sh` as `'goose` and dies with
  `Unterminated quoted string`. Every shipped example is a bare executable, so this
  path was previously unexercised. Worked around with `IFS=` to disable word
  splitting, which needs no inner quotes at all.
- The record-time failure messages are **good** — "the agent made no model calls;
  it exited 2 without talking to the proxy" named the real cause immediately.
  Worth saying, since the rest of this entry is faults.
- The run report (`report.json`) contains no agent stdout/stderr, so diagnosing a
  divergence meant re-running with output redirected inside the spec's own command.

**Deliberately not built:** any second spec (the gate is not passed); any CI job
(nothing replays green, and a skip-with-warning job is forbidden); the fake-model
baseline (it is scheduled after flow 1 is green, and building it now would invite
using it as a recording substitute).

**Next iteration should:** verify B1 against a second, independent agent to
establish whether the dropped call is goose-specific or general — the cheapest
decisive test is `examples/agent-demo/weather-node.flow.yaml`, which ships with
flowproof and is known to work, instrumented at the upstream to count calls. If it
also drops a trailing call, B1 is a plain recorder bug and should be filed with the
goose evidence attached. **Do not add a second goose spec either way.** If B1 turns
out to be unfixable from outside flowproof, the honest end-state for this loop is a
negative result whose cause is precisely located and worth reporting — which is a
win under this brief, and considerably more useful than four specs that prove
nothing.
