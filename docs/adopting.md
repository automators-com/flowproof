# Adopting flowproof in an existing repo

Written to be handed to a coding agent. Point Claude Code (or any agent) at
this file and it has the whole adoption path, including the parts that are
judgement calls rather than commands.

The order matters. Every adopter so far has spent their first session on
reconnaissance, and every one of them answered the same three questions
before a single spec was worth writing. Do the audit first.

## Step 0: install

```bash
npm install --save-dev flowproof     # or: pip install flowproof
npx flowproof --version
```

Replay needs no API key. Recording needs one, once per flow — `flowproof
config ai` stores it once per machine instead of exporting it in every shell
(see [getting-started.md](getting-started.md#flowproof-config-credentials-without-hand-exporting-env-vars)).

## Step 1: the audit (do this before writing any spec)

Read the codebase and answer these. Report the answers before proposing
anything - a wrong answer here wastes the whole session, and two of the
three are usually not what they look like.

**1. How is the agent started, and by whom?**

- Can flowproof START it (`agent.command:`)? Then flowproof injects
  environment into that process.
- Or is it an already-running service (`agent.url:` plus `proxy_port:`)?
  Then flowproof cannot inject anything, and you must point the service at
  the proxy yourself.

The trap: an orchestrator. If a service spawns a child that makes the model
call, `command:` only works when the child INHERITS the environment. One
adopter's stack was three processes deep, with the model call in the
grandchild. Check the spawn, do not assume.

**2. Where does the model client get its base URL?**

flowproof injects `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OPENAI_BASE`,
`ANTHROPIC_BASE_URL` and `FLOWPROOF_LLM_PROXY`. If the client reads one of
those, you are done.

If it reads something else - an AI gateway variable, a config object built
at spawn time - map it, because the proxy's port is not known until it
binds:

```yaml
agent:
  command: ./start-agent
  env:
    AI_GATEWAY_URL: "${flowproof.proxy_url}"        # includes /v1
    OTHER:          "${flowproof.proxy_url_no_v1}"  # client appends its own
```

Find out which form the client wants BEFORE recording. Both exist because
both mistakes are common.

**3. Which tools have side effects?**

List every tool the agent can call and mark the ones that create, mutate,
delete, send, charge, or deploy. This is the judgement call that matters
most, and it decides the boundary:

- **`tools:` (model boundary)** rewrites what the model is TOLD a tool
  returned. **The agent still runs its own tool.** Read-only tools only.
- **`mcp:` (MCP boundary)** is answered by flowproof; the real server never
  runs it, at record or replay. Everything with a side effect goes here.

Get this wrong and a RECORDING run mutates real data. flowproof prints a
runtime warning when a flow mocks or forbids a tool nothing intercepts;
treat that warning as an error.

**Also worth checking while you are in there:** does the agent's MCP client
build several endpoints from one base (`<base>/mcp`, `<base>/mcp-exec`)?
The HTTP stand-in matches any path containing `/mcp`, so one stand-in
serves them all - point the base at `${flowproof.mcp_url.<name>}` rather
than wiring each path.

## Step 2: one smoke flow, and nothing else

Do not write five specs. Write the smallest flow that proves the wiring:

```yaml
name: the agent answers at all
app: agent
agent:
  command: <however it starts>
steps:
  - prompt: <the simplest real task>
  - assert: reply contains <something the answer must contain>
```

Record it, then replay it offline with no key set. Until that passes, every
other spec is guesswork.

If the record fails with **0 model requests captured**, question 2 is the
answer: the client is not honouring the base URL you mapped. No trace is
written in that case, by design - a green replay of an empty cassette would
prove nothing.

`assert: reply contains` reads the last assistant message from the model
boundary, NOT stdout. An agent that returns its answer over SSE, a queue,
or a subprocess boundary is unaffected.

## Step 3: the flows worth having

Now write the tests. In value order:

1. **Argument threading.** Assert a value from the user's request reached
   the tool intact: `assert_tool_call: create_set where rows equals 100`.
   This catches the most common real regression.
2. **A guard flow.** `assert_no_tool_call` on a destructive tool. Read
   [agent-testing.md](agent-testing.md#making-a-guard-flow-prove-enforcement-not-compliance)
   first: a guard proves the agent did not ASK, not that it could not. Pair
   it with enforcement in code, and record an adversarial turn where the
   model actually tried, or the flow certifies a polite day.
3. **`strict: true`** where the exact call set is the contract.

## Step 4: CI

```bash
npx flowproof run specs/
```

Commit each trace next to its spec. Replay needs no key, so this runs on
every push for nothing. It emits `junit.xml` per flow and a merged
`suite-junit.xml`.

Do not set `FLOWPROOF_BIN` in CI - that points at a local build, and CI
should prove the RELEASED package works.

## What to tell the team these tests prove

Be precise, because the honest claim is narrower than it sounds and the
overclaim is easy.

**They prove:** your code and configuration - tool dispatch, argument
threading, tool-result handling, loop termination, and the request you send
(system prompt, tool schemas, model id). Change any of those and the
recorded request stops matching.

**They do not prove:** that the model is any good. Its decisions are
recorded, not judged. A cassette cannot tell you a model degraded after a
version bump. That is an evals problem, and these tests should never be
described as covering it.

The closest familiar thing is HTTP cassette testing: you are not testing
the provider's servers, you are testing your integration with them, on
every commit, for free.

## Known limits, so you do not design around them blind

- **A flow is ONE turn.** All `prompt:` steps are joined into a single task
  delivered up front. No follow-up user turn. A conversational guarantee
  can only be tested for what one task produces.
- **`assert_no_egress` is Linux-only.** Elsewhere it fails as a capability
  error rather than passing vacuously, so keep it out of flows that must
  pass on a developer Mac.
- **An agent that stamps WALL-CLOCK TIME into its prompt cannot replay.**
  Matching is byte-exact, so an agent that prefixes each turn with the current
  time (or a session id, or the working directory) sends a different request
  every run and diverges at turn 1. Check for this in Step 1 by recording twice
  and diffing the two traces - it is much cheaper to find there than after you
  have written flows. The workaround is to freeze the agent's clock, e.g.
  `LD_PRELOAD` with libfaketime and a fixed `FAKETIME`; if you do, set
  `FAKETIME_DONT_FAKE_MONOTONIC=1` as well, or the agent's async timers never
  fire and the run hangs for ever. That trick is Linux-only, so an agent like
  this is not testable on a developer Mac.

- **The tool name you assert is the name the MODEL saw, not the MCP name.**
  An MCP client normally namespaces a server's tools before offering them, so a
  server tool `delete_all` can reach the model boundary as `files__delete_all`,
  and the prefix comes from how the client names the server. `assert_tool_call`
  and `assert_no_tool_call` match the model-boundary name. Read it off a
  recorded trajectory rather than guessing it - a guard asserting a name that
  never appears passes for the wrong reason.

- **No structured `args_exact:`.** Prose `where` clauses only. Unasserted
  arguments are still pinned byte-exactly by the cassette, and a drift
  names the path that moved.
