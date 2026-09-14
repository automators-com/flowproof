---
title: "Agent-testing overview"
description: "How a test runs with no model, what's actually under test, and wiring a real agent for recording."
---

Status: **shipped**. v1 (OpenAI-compatible proxy, `assert_tool_call`), v2
(Anthropic Messages API, streaming replay, http-target agents) and v3.1/v3.2
(the MCP tool boundary, stdio and streamable-HTTP) are all built; the
`## Phasing` section below is authoritative on what landed when, and
"Settled in review" records the design calls. A complete, runnable example
ships in [`examples/agent-demo/`](../examples/agent-demo/).

## How a test runs with no model

The question this page has to answer first, because everything else depends
on it: if there is no LLM at replay, who decides to call the tool?

**The model's decisions are RECORDED, not mocked.**

1. **Record, once, against a real model.** Your agent runs for real.
   flowproof points its SDK at a local proxy (the standard
   `OPENAI_BASE_URL` / `ANTHROPIC_BASE_URL`), forwards each call to the real
   model, and captures the request and the reply - *including the model's
   tool-call decisions* - as a **cassette** in the trace.
2. **Replay, every run after that, against nothing.** The agent runs for
   real AGAIN: same code, same SDK, same tool loop. But when it asks the
   model what to do next, the proxy answers from the cassette. No model is
   contacted, so a CI run is free, offline, and cannot flake on sampling.

Nobody needs an LLM to decide to call `get_weather` at replay, because that
decision was already made and written down. The agent still issues the call;
it is being told what to do by a recording instead of by a live model.

**Then what are the `tools:` mocks for?** Not for replacing the model - for
keeping the conversation reproducible. Your real tool returns something
volatile (a timestamp, a generated id), and that value goes back into the
NEXT request to the model. Replay matches each incoming request against the
recorded one, so a fresh timestamp would be a mismatch. The `result:` mock
substitutes a fixed value at the boundary, so the second turn is identical
every run.

Two mechanisms, two jobs:

| | replaces | so that |
|---|---|---|
| **cassette** | the model's decisions | no LLM is called at replay |
| **`tools:` mock** | a volatile tool result | those decisions still match |

And this is where a regression surfaces: if the agent calls a different tool
or passes a different argument, the request no longer matches what was
recorded, and replay fails with a divergence rather than passing quietly.

### So what is actually under test?

A fair objection: if the model is a recording and the tools are mocks, what
is left? The answer is specific, and it is worth being blunt about both
halves.

**Under test: your agent's own code and configuration.** That is the glue
between the model and the tools, and it is where agent bugs actually live:

- the tool-call is parsed and dispatched to the right function
- arguments are threaded correctly (`assert_tool_call ... where city
  contains Nairobi` is checking YOUR mapping, not the model's spelling)
- the tool result is fed back in the right shape, so the loop continues
- the loop terminates instead of spinning
- a message carrying several tool calls is still handled
- the request you SEND still looks the same: the system prompt, the tool
  schemas, the model id, the message history you construct. Edit any of
  them and the recorded request stops matching, which is the point.

**Not under test: whether the model is any good.** A cassette cannot tell
you the model got worse after an upgrade, or that your prompt is weak. That
is an evaluation problem with statistical answers, and it is deliberately
out of scope (see "Decision: model-output evals are out of scope"). Nor does
it test your tool's implementation - that is an ordinary unit test - or a
real MCP server's behaviour.

The closest familiar thing is HTTP cassette testing (VCR, nock, `responses`).
You are not testing Stripe's servers; you are testing your integration with
them, on every commit, for free. Same trade here, with the same honest
limit: a green suite means "the deterministic half still behaves", not "the
system is smart".

Where that pays off most sharply is the guard path. Record one adversarial
model response - a jailbreak, an injected instruction - and then assert
FOREVER, at no per-run cost, that your scaffolding refuses to act on it:

```yaml
  - assert_no_tool_call: transfer_funds
```

The model said "call it"; the test proves your agent did not. That is a
regression test you cannot practically run against a live model, because
you would be paying to re-roll a dice you already know the face of.

### Wiring a real agent: env, handles, and the record upstream

The runtime contract, in one place, because an adopter whose agent is not a
plain SDK loop hits all of it at once.

**What flowproof injects into a `command:` agent:**

| variable | value |
|---|---|
| `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OPENAI_BASE` | the proxy, with `/v1` |
| `ANTHROPIC_BASE_URL` | the proxy WITHOUT `/v1` (that SDK appends its own path) |
| `FLOWPROOF_LLM_PROXY` | the same base again, for a client that takes it as an argument |
| `OPENAI_API_KEY`, `ANTHROPIC_API_KEY` | placeholders, so a client that refuses to start without a key still starts |
| `FLOWPROOF_PROMPT` | the task |
| `FLOWPROOF_MCP_SERVER_<NAME>` / `FLOWPROOF_MCP_URL_<NAME>` | the stand-in for each declared MCP server |

**If your client reads a different variable**, map it in `agent.env` using a
runtime handle. The proxy binds an ephemeral port, so its URL cannot be
written into a spec ahead of time; these are substituted at spawn:

```yaml
agent:
  command: ./start-agent
  env:
    AI_GATEWAY_URL: "${flowproof.proxy_url}"          # includes /v1
    OTHER_GATEWAY: "${flowproof.proxy_url_no_v1}"     # client appends its own
    EXEC_MCP_BASE: "${flowproof.mcp_url.datamaker_exec}"
```

`agent.env` is applied LAST, so a mapping here overrides anything injected
above. An unknown `${flowproof.*}` handle is passed through untouched rather
than failing the run.

**MCP paths.** The HTTP stand-in matches any path CONTAINING `/mcp`, so a
client that derives `<base>/mcp`, `<base>/mcp-exec` and `<base>/mcp-exec/sap`
from one base all route to the same stand-in. You do not need one listener
per path; you need the base to point at the stand-in, which is what
`${flowproof.mcp_url.<name>}` is for.

**Check the wiring before writing a spec.** The failure above is the
commonest one in adoption, and it used to be found only after a spec was
written and a key spent. `flowproof doctor` answers the same question in
seconds, with no spec, no assertions and no key:

```bash
flowproof doctor --agent "./start-agent"
```

It starts the proxy, runs the command once against a canned reply, and
reports how many model requests ARRIVED. Zero means the client is not
honouring the injected base URL, and the output names the handles to map.

It reports what it saw rather than declaring the wiring correct, because an
agent with more than one client can reach the proxy with one and the real
provider with another. `record` is what settles that.

The task it hands the agent is `Say hello.`, delivered through
`FLOWPROOF_PROMPT`. Change it with `--prompt` when that default would not
make your agent call a model at all, one that routes on the task, or
short-circuits something trivial, can answer without a single request and
report a zero that says nothing about the wiring:

```bash
flowproof doctor --agent "./start-agent" --prompt "Look up order 4711."
```

The reply is canned either way, so the prompt only decides whether the agent
reaches for a model, never what comes back.

Two limits worth knowing. It cannot tell a hang from a slow agent, so a
process waiting for a useful answer sits until `--timeout` (60 seconds by
default). And if the agent spawns a child that outlives it, the wall clock
can exceed that timeout, because flowproof stops the process it started
rather than the tree.

`--agent` is this doctor's only concern; for SAP GUI / Fiori connectivity
(`app: sap` / `app: web`), see [`flowproof doctor --sap` /
`--fiori`](../getting-started/secrets-and-config.md#flowproof-doctor---sap----fiori-is-any-of-this-reachable)
in the getting-started guide instead.

**A record run that captures nothing FAILS.** If zero model requests reach
the proxy, `record` errors and writes NO trace. That is the one failure a
determinism tool must never let through: an agent that reached the real
provider instead of the proxy would otherwise leave a cassette that replays
green while proving nothing. The error names the likely cause, because it is
usually invisible - a client whose base URL comes from a config object or a
custom variable never sees the standard ones flowproof injects. There is no
opt-out: a flow that legitimately makes no model calls is not an `app: agent`
flow.

Related, and worth knowing before you go hunting: the proxy routes on a
SUBSTRING of the path, so a base URL with a doubled `/v1` still reaches it.
Picking `${flowproof.proxy_url}` where you wanted `${flowproof.proxy_url_no_v1}`
is therefore not a silent failure mode.

**Testing an unreleased fix.** An adopter who hits a gap should not have to
wait for a release to test the fix. `FLOWPROOF_BIN` points the launcher at
any build:

```bash
export FLOWPROOF_BIN=/path/to/flowproof/target/release/flowproof
npx flowproof run specs/
```

It wins over the resolved platform package, is announced on stderr every run
(`flowproof: using FLOWPROOF_BIN=...`), and exits 2 if the path does not
exist rather than falling back. Deliberately noisy: an engine swapped
silently would make a green run mean nothing. Build one with
`cargo build --release -p flowproof-cli`, or take the binary from a CI run of
the branch carrying the fix. CI should NOT set this - a suite whose job is to
prove the RELEASED package works must use the released package.


**Recording needs a real model.** Replay needs nothing, but `record` has to
call something. The upstream is read from, in order:

1. `FLOWPROOF_AGENT_UPSTREAM` - an OpenAI-compatible base URL, including a
   gateway. Use this when `OPENAI_BASE_URL` in your shell points somewhere
   else.
2. `OPENAI_BASE_URL` - the one a developer usually already has set.

The key is read from `FLOWPROOF_AGENT_KEY`, then `ANTHROPIC_API_KEY`, then
`OPENAI_API_KEY`. It goes into the outbound `Authorization` header and
nowhere else: the trace stores request bodies only, so no key reaches disk.

**What `assert: reply contains` reads.** The content of the LAST assistant
message in the trajectory - taken from the model boundary, NOT from the
agent's stdout. This matters for any agent that returns its answer over SSE,
polling, a queue, or a subprocess boundary: none of that affects the
assertion, because the reply is read where the model produced it. A
trajectory whose last turn is a tool call has no reply yet, which is a real
state rather than an empty string.

**`assert_no_egress` is enforced on Linux only.** On macOS and Windows the
run reports "not contained" and the assertion fails as a capability error
rather than passing vacuously, so it will not silently certify nothing. See
[Egress containment](#egress-containment).

**Two limits to know before you start**, because they shape what a flow can
express rather than being details you hit later:

- **A flow is ONE turn, not a conversation.** Every `prompt:` step is joined
  into a single task string delivered up front; flowproof then observes the
  trajectory the agent produces on its own. There is no follow-up user turn,
  and no step that replies to the agent mid-run. A conversational system can
  be tested this way only for what one task produces. See
  [Single-turn, and what multi-turn would cost](#single-turn-and-what-multi-turn-would-cost).
- **The model boundary is not the tool boundary.** A `tools:` mock rewrites
  what the model is TOLD a tool returned; the system under test still ran
  that tool. Only the `mcp:` boundary keeps a tool from executing. Flows that
  mock or forbid a tool nothing intercepts get a runtime warning.
