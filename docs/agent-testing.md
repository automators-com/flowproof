# Agent-boundary testing (design)

Status: **design for review** — no code yet. Tracking: the v1 issue and
the MCP follow-up issue linked from this doc's PR.

## The problem

Teams building AI-based systems (an assistant that answers a prompt by
calling tools, an agent embedded in a product) have no standard way to
test them deterministically. The failing pattern in practice:

- "Given this input prompt, the system should make these tool calls" —
  but running the test executes real tools (side effects, cost) against
  a nondeterministic model (flaky assertions).
- One prompt rarely means one tool call: real behavior is a multi-step
  trajectory — call a tool, read its result, call the next — so
  point-assertions on a single call miss the shape of the behavior.
- Ad-hoc harnesses get written per repo (ours included). Each one
  reinvents mocking, capture, and comparison, none of it reviewable.

Two problems hide in "test the AI", and they are very different:

1. **Testing an AI-based system**: the system under test *uses* a model
   internally. The test asks: does the system wire the model to its
   tools correctly — right tool, right arguments, right sequence, right
   final behavior? This is an integration-testing problem and it can be
   made **fully deterministic**.
2. **Validating model output quality**: is the model's answer *good*?
   That is an eval problem — sampling, scoring functions, thresholds,
   judges — with no fixed expected output.

flowproof takes on **problem 1**. Problem 2 is explicitly out of scope
(see the decision at the end): a deterministic replay engine is the
wrong runner for statistical quality measurement, and pretending
otherwise would make both worse.

## The key simplification: one boundary sees everything

Everything a trajectory test needs to observe or control crosses the
**model API boundary**:

- the input prompt (the request the system sends to the model),
- every tool call (returned BY the model as a tool-use response),
- every tool result (sent back TO the model by the system),
- the final reply.

So flowproof does not instrument the system's tools at all. It stands up
a local model-API proxy; the system under test is pointed at it through
its normal configuration (`OPENAI_BASE_URL`-style env vars — suite env
already does this). "Mock the tool, don't run it" falls out for free:
when the proxy answers the model turn *for* the model, the scripted
tool call is returned to the system, the system executes its own tool
dispatch against whatever the spec staged, and no real side effect ever
fires — or, in cassette mode (the default), even the tool results are
replayed, and nothing external runs at all.

This mirrors how flowproof already treats the browser's network: mock at
the boundary, identically at record and replay, with the rules traveling
in the trace.

## Record → replay, applied to the model boundary

The existing core loop maps one-to-one:

- **Record**: run the flow once against the real model. The proxy passes
  traffic through and captures the full trajectory — request/response
  pairs, tool calls, tool results — into the trace as a **cassette**
  (redaction applies; API keys stay `${VAR}` refs and are never stored).
  Recording asserts too: a trace is only minted for a trajectory that
  actually satisfied the spec.
- **Replay**: the proxy serves the recorded model responses. The system
  under test becomes fully deterministic — no model cost, offline,
  CI-safe — and the assertions verify the trajectory is unchanged.
- **Drift**: the system's prompt template or tool schema changed, so a
  live request no longer matches the cassette. That is the heal moment,
  same as a moved button: re-record and produce a reviewable
  **trajectory diff** ("previously `search_flights` → `create_booking`;
  now also calls `check_visa` in between") for human approval.

## Spec shape (sketch)

```yaml
name: Booking assistant books a flight
app: agent
agent:
  command: "npm run assistant"        # process to drive; http target later
  env:
    OPENAI_BASE_URL: "${FLOWPROOF_LLM_PROXY}"   # injected by the runner
tools:
  - name: search_flights              # mocked at the boundary, never executed
    result: { flights: [ { id: KQ311, dest: NBO } ] }
  - name: create_booking
    result: { booking: B-1042 }
steps:
  - prompt: Book me a flight to Nairobi tomorrow
  - assert_tool_call: search_flights where destination contains NBO
  - assert_tool_call: create_booking
  - assert: reply contains booked
```

Semantics:

- `assert_tool_call` steps assert an **ordered subsequence**: the listed
  calls must occur in this order; unlisted calls in between are allowed.
  A `strict: true` flow-level flag forbids unlisted calls — both modes
  are needed in practice, and subsequence is the right default for
  multi-step agents.
- `tools:` entries provide the mocked results the trajectory needs to
  continue past each call (a multi-step agent cannot proceed without
  them). In cassette replay these are recorded anyway; the block is what
  lets **record** avoid executing anything real.
- `assert_no_tool_call: <tool>` asserts a tool was NOT called anywhere
  in the trajectory (optionally `where` clauses narrow it: "never with
  amount above X"). This is the guard-path assertion — "the agent must
  refuse WITHOUT side effects" — and arguably the highest-value one in
  the feature: the dangerous tool is mocked, so even a buggy agent
  causes no harm while the test proves it misbehaved. Scoped to the
  whole trajectory regardless of position; a positional variant can come
  later if the field demands it.

## Argument assertions

Which tool was called is half the test; **what it was called with** is
the other half, and usually where the bugs are.

**Path matchers, partial by default.** Tool arguments are JSON, often
nested. The prose form takes `where` clauses on dotted paths, reusing
the existing matcher vocabulary (`equals`, `contains`, numeric
normalization); the structured form takes an `args:` mapping for
anything prose reads badly:

```yaml
- assert_tool_call: create_booking where flight.id equals KQ311
- assert_tool_call:
    tool: create_booking
    args:
      flight.id: KQ311                # equals
      passenger.name: { contains: Casey }
      seat: { matches: "[0-9]+[A-F]" }   # volatile shape, not value
```

Partial matching is the default — assert the arguments that carry the
intent, not the whole object. An `args_exact:` form does full deep
equality when the whole payload IS the contract. `${VAR}` refs resolve
at execution like everywhere else.

**Chained arguments are statically assertable.** Because tool results
are spec-authored mocks, the expected arguments of *downstream* calls
are known when the spec is written: if the `search_flights` mock returns
`id: KQ311`, asserting `create_booking where flight.id equals KQ311`
tests that the agent correctly threaded data from one tool's result into
the next tool's call — the actual behavior multi-step agents get wrong —
with zero nondeterminism and no capture machinery.

**Volatile arguments** ("tomorrow" rendered as a date, generated
idempotency keys): assert shape, not value — `matches` a pattern, or
`exists`. The cassette layer (below) still pins the exact recorded value
for regression purposes; the spec assertion names only what must hold
across re-records.

**Two layers, two jobs.** The cassette pins EVERY argument byte-exactly:
at replay, any argument drift is a cassette mismatch reported as a
field-level diff of the call's arguments (naming the path that changed),
so even unasserted arguments are regression-protected by default.
`assert_tool_call` is the *intent* layer on top: it is checked at record
time (no trace is minted for a trajectory that fails it — same rule as
UI flows), re-checked against the new trajectory after every re-record,
and it documents in the spec which argument properties are meaningful —
the ones a reviewer should defend in a heal diff, versus incidental
values the cassette merely happens to pin.

## Phasing

1. **v1**: OpenAI-compatible chat-completions proxy (non-streaming),
   `app: agent` process driver, cassette in trace v1 (additive header +
   step artifacts), `assert_tool_call` grammar, trajectory diff on
   re-record.
2. **v2**: Anthropic messages API; streaming replay (serve the recorded
   stream); http-target agents (drive a service instead of a process).
3. **v3**: MCP servers as a second mockable boundary, for systems whose
   tools are external MCP processes rather than internal functions.

## Open questions (to settle in review)

- **Cassette matching tolerance.** Byte-exact prompt matching is brittle
  (timestamps, IDs inside prompts). Proposal: match on the structural
  envelope (model, tool schema, message roles/count) plus a normalized
  prompt hash, with named `${VAR}`-style holes for known-volatile spans.
  The failure message must always show WHAT diverged.
- **Trajectory branching.** If the recorded trajectory had N turns and
  replay's turn K request doesn't match turn K's recording, do we search
  forward (reordering tolerance) or fail immediately? Start strict
  (fail, name the divergence) — reordering tolerance can be added if the
  field demands it.
- **Where the reply assertion reads from.** `assert: reply contains …`
  needs a definition of "reply" per driver (stdout for a process,
  response body for http).

## Decision: model-output evals are out of scope

The second problem — "is the model's answer good?" — needs samples,
scoring, thresholds, and judges. Its verdicts are statistical, not
deterministic, and its artifacts are score distributions, not traces. A
future `flowproof eval` could exist as a *separate* runner sharing the
proxy/cassette infrastructure, but the replay engine's promise
("recorded once, passes forever unless the system changed") must not be
blurred by a step type that can fail on an unchanged system. Same
philosophy as the `page.evaluate` rejection in
[design.md](design.md): protect the invariant that makes the tool
trustworthy.
