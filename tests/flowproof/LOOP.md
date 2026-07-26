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
| goose tag (PINNED) | **v1.44.0** — latest non-RC tag; v2.x are release candidates, not pinned |
| goose acquisition | prebuilt release binary, `goose-x86_64-unknown-linux-gnu.tar.bz2` (285 MB unpacked). **No source build required.** |
| flowproof version | **0.7.0** (npm, `npm install flowproof@0.7.0`) |
| specs committed | **0** (correct — the gate is not passed) |
| traces recorded | **0** |
| CI state | **no job committed** (correct — nothing runs yet, and a skip-with-warning job is forbidden) |
| iterations with no recorded trace | **1** (of 3 allowed before the loop self-terminates) |

## Current verdict

Both iteration-1 spike questions answered **YES**, and more cheaply than expected:
goose honours `OPENAI_BASE_URL` — a variable flowproof already injects — so the
model boundary needs **zero glue**, and goose takes an MCP server's command and
URL straight off the CLI (`--with-extension`, `--with-streamable-http-extension`,
plus `--no-profile` to suppress its own defaults), so the tool boundary needs no
`config.yaml` templating either. flowproof's interception contract fits a
third-party agent it has never seen, at a cost of roughly one line in a spec.
The open risk is no longer *can we intercept goose* — it is whether we can
**record** at all: this container has no model credential, so the gate (one real
trace replaying green offline) is untested.

---

## Adoption cost so far

| measure | value |
|---|---|
| wall-clock to first green replay | **n/a — no replay yet** (blocked on a model credential, see Blockers) |
| container wall-clock, first command → both spikes answered | **~6 min** (09:49 → 09:55 UTC) |
| **adoption glue written** | **0 lines.** The wiring is one `agent.command` string (below). No entry point, no config templating, no wrapper. |
| diagnostic code written (NOT adoption glue) | 106 lines, in `spike/` — `probe_server.py` (60), `marker_mcp.py` (46). Throwaway instruments used to answer the spike; not tests, not shipped glue. |
| **reads of goose SOURCE** | **0 reads of goose logic.** One read of its root `Cargo.toml` (to learn the pinned version and that the workspace is huge) — build metadata, not internals. Everything else came from `goose --help`, `goose run --help`, `goose info`. This is the good news: goose's own CLI help was sufficient. |
| **reads of flowproof SOURCE** | **0.** `README.md` and `docs/agent-testing.md` only — user-facing docs. |

The wiring, in full, as it will appear in a spec:

```yaml
agent:
  command: >
    sh -c 'goose run --no-session --no-profile
           --with-extension "$FLOWPROOF_MCP_SERVER_<NAME>"
           -t "$FLOWPROOF_PROMPT"'
```

`OPENAI_BASE_URL` needs no mention at all — flowproof injects it and goose reads it.

---

## Spike results (iteration 1) — answered by real command runs, not docs

### Q1: Can goose's model base URL be pointed at flowproof's proxy from the environment? — **YES**

Method: a logging HTTP server (`spike/probe_server.py`) on `127.0.0.1:8099`; goose
run under `env -i` with one candidate variable set at a time, so nothing ambient
could explain a hit.

| variable set (alone) | goose reached the probe? |
|---|---|
| `OPENAI_BASE_URL` | **YES** — 6 requests |
| `OPENAI_HOST` | YES — 6 requests (goose's own documented variable) |
| `OPENAI_API_BASE` | no |
| `OPENAI_BASE` | no |
| *(none — control)* | no — 0 requests |

The control run is the important row: with no variable set goose reached the probe
zero times, so the hits are genuinely caused by the variable and not by a default.

**`OPENAI_BASE_URL` is one of the three flowproof already injects.** No
`agent.env` mapping, no `${flowproof.proxy_url}` handle needed.

Observed request shape, which the next iteration depends on:
`POST /v1/chat/completions`, **`"stream": true`**, OpenAI tool-call schema.
Streaming replay is documented as shipped (v2), but it is now on the critical
path — goose never sends a non-streaming request.

### Q2: Can a goose MCP extension's command or URL be redirected to flowproof's stand-in? — **YES**

goose exposes, on `goose run` itself:

- `--with-extension <COMMAND>` — stdio server from a full command string, format
  `'ENV1=val1 command args...'`. This is exactly the shape of
  `FLOWPROOF_MCP_SERVER_<NAME>`.
- `--with-streamable-http-extension <URL>` — matches `FLOWPROOF_MCP_URL_<NAME>`.
- `--no-profile` — "don't load your default extensions". **This is what removes
  the need to template `~/.config/goose/config.yaml`**, and it is also what makes
  the tool set deterministic enough to assert on.

Proved by running goose with `FLOWPROOF_MCP_SERVER_MARKER="python3 marker_mcp.py"`
in the environment and `--with-extension "MARKER_ENV=from_env $FLOWPROOF_MCP_SERVER_MARKER"`.
The stand-in recorded:

```
SPAWNED argv=['.../marker_mcp.py'] MARKER_ENV=from_env
RPC initialize
RPC notifications/initialized
RPC tools/list
```

and its tool reached the model boundary — the probe's last captured request
advertised `tools: ['python3__marker_ping']`.

**Consequence for spec authoring (carry this forward):** goose *renames* extension
tools by prefixing them with a name derived from the command's **first token**.
`marker_ping` became `python3__marker_ping`. So an `assert_no_tool_call: shell`
will not match — the spec must name the prefixed form, and the prefix depends on
the command string flowproof generates. Verify the exact prefix from a real
trajectory before writing any guard assertion; do not guess it.

---

## Flows committed

None. Correct: the gate ("one recorded trace that replays green offline before a
second spec exists") is not passed, and the rule is that if you cannot record you
cannot add specs. **Do not write flow specs until a trace exists.**

Priority order once the gate is passed (unchanged):
1. `assert_no_egress` (Linux-only; this suite is CI-only, so require Linux and say so)
2. `assert_no_secret_leak`
3. destructive-tool guard at the `mcp:` boundary
4. `strict: true`

---

## Blockers

### B1 — No model credential in this container. Blocks the gate.

`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY` are all unset.
`ANTHROPIC_BASE_URL=https://api.anthropic.com` is set but rejects unauthenticated
calls (`authentication_error: x-api-key header is required`). Recording requires
one real model call; replay requires none.

**What would unblock it, in order of preference:**
1. A real key (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) in the environment. Cheapest
   and most faithful — one recording is a few cents.
2. A local model served on localhost (e.g. ollama) as the *upstream* flowproof
   records through. This is legitimate — a small local model is still a real model
   making real decisions, so the cassette is recorded, not fabricated. Risk: a small
   model may not emit tool calls reliably enough to mint a useful trajectory.
   Cost: model download + slow inference on 4 cores.

**What must NOT unblock it:** hand-writing a cassette, or recording against
`spike/probe_server.py`. A fabricated trace replays green and proves nothing.
Note especially that the fake-model baseline (the falsifiability instrument) is a
*separate deliverable built later* — it is not a substitute for recording, and
the two must not be conflated.

### B2 — GitHub API and `github.com` HTTPS are blocked for non-`automators-com` repos.

`add_repo` refuses cross-owner adds. Two workarounds found and both work, so this
is not blocking: `git clone https://github.com/aaif-goose/goose.git` succeeds via
the session's git proxy, and release *assets* under
`github.com/<o>/<r>/releases/download/...` return 200. Recorded so the next
iteration does not rediscover it.

---

## Iteration log

### Iteration 1 — spike (09:49 → 09:55 UTC)

**Shipped:** nothing to CI, by design. Committed `spike/probe_server.py` and
`spike/marker_mcp.py` as reproduction evidence for the two answers above, plus
this file.

**Found:**
- Both spike questions are **YES**. goose reads `OPENAI_BASE_URL`; goose takes MCP
  stdio commands and HTTP URLs on the CLI. Adoption glue so far: **0 lines**.
- goose pins cleanly to **v1.44.0** and ships a prebuilt Linux binary, so pinning
  costs a download, not a 40-minute Rust build of a workspace that vendors v8 and
  candle.
- goose only ever sends **streaming** chat-completions requests.
- goose **rewrites extension tool names** with a prefix derived from the command's
  first token (`marker_ping` → `python3__marker_ping`). This will bite guard
  assertions and must be read off a real trajectory.
- **No model credential in this container** (B1) — the gate is blocked, not failed.

**Deliberately not built:** any flow spec (the gate forbids it); any CI job
(nothing to run, and a skip-with-warning job is forbidden); the fake-model
baseline (it is scheduled *after* flow 1 is green, and building it now would
invite using it as a recording substitute).

**No flowproof gap reported this iteration.** Both spikes passed; nothing new and
genuine to report. The tool-name-prefix issue above is a goose behaviour and a
spec-authoring hazard, not a flowproof defect — reassess once a real trajectory
exists.

**Next iteration should:** resolve B1 and nothing else. Concretely — check whether
a model credential can be provided; if not, evaluate option 2 (local model as the
record upstream) far enough to answer yes/no on whether it can mint a trajectory
containing a real tool call. **Do not write a flow spec in that iteration either
way.** If B1 cannot be resolved after two more iterations, the loop's own
stopping rule applies: three consecutive iterations with no recorded trace *is*
the verdict, and it must be reported as such — with the honest qualifier that the
cause here is a missing credential in the test environment, not flowproof's
adoption cost, which measured at zero lines of glue.
