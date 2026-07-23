# Agent-boundary testing

Status: **shipped (v1)** in the 0.3.x line. This page is the reference for
the feature as built. The `## Phasing` section below marks what is v1 versus
the v2/v3 roadmap (Anthropic messages API, streaming replay, http-target
agents, the MCP boundary); "Settled in review" records the design calls that
went into v1. A complete, runnable example ships in
[`examples/agent-demo/`](../examples/agent-demo/).

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
already does this). What the proxy controls is what the MODEL sees: for a
tool the spec gave a `result:`, the tool result the system reports back is
replaced with the mock before the model conditions on it (see "Settled in
review"), so the trajectory is driven entirely by spec-authored data.

Be precise about what this does and does not prevent. flowproof sits at
the model boundary, not the tool boundary, so the system STILL EXECUTES
ITS OWN TOOLS — substitution pins what the model reads, it does not stop
the tool from running. A tool with real side effects (a booking, a charge)
still fires unless the author stubs or sandboxes it, or waits for the v3
MCP boundary. What v1 guarantees is that the model's view is
spec-controlled, and that replay is hermetic AT THE MODEL BOUNDARY: zero
model calls, canned responses.

This mirrors how flowproof already treats the browser's network: mock at
the boundary, identically at record and replay, with the rules traveling
in the trace — with the one honest caveat that the browser mock intercepts
the request, while the model-boundary mock only rewrites what the model is
told about a tool the system ran itself.

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

## Spec shape

```yaml
name: Booking assistant books a flight
app: agent
agent:
  command: "npm run assistant"        # process to drive; http target later
tools:
  - name: search_flights              # result substituted at the model boundary
    result: { flights: [ { id: KQ311, dest: NBO } ] }
  - name: create_booking
    result: { booking: B-1042 }
steps:
  - prompt: Book me a flight to Nairobi tomorrow
  - assert_tool_call: search_flights where destination contains NBO
  - assert_tool_call: create_booking
  - assert: reply contains booked
```

The proxy URL is injected into the agent process automatically (see
[Running an agent flow](#running-an-agent-flow) below), so no `env:` wiring
is needed; `agent.env` is only for a client that reads a non-standard
variable.

Semantics:

- `assert_tool_call` steps assert an **ordered subsequence**: the listed
  calls must occur in this order; unlisted calls in between are allowed.
  A `strict: true` flow-level flag forbids unlisted calls — both modes
  are needed in practice, and subsequence is the right default for
  multi-step agents.
- `tools:` entries provide the mocked results the trajectory needs to
  continue past each call (a multi-step agent cannot proceed without
  them). In cassette replay these are recorded anyway; the block is what
  lets **record** avoid executing anything real. A `tools:` entry with NO
  `result:` is a **declaration only**: it is not mocked, so the tool's real
  result passes through unsubstituted. It still validates an
  `assert_tool_call` target and documents which tools the flow expects.
- `assert: reply contains <text>` reads the FINAL ASSISTANT MESSAGE of
  the trajectory, whatever the driver. `reply is <text>` is accepted as an
  alias and means the same thing (substring match, not exact equality). See
  "Settled in review" below.
- `assert_no_tool_call: <tool>` asserts a tool was NOT called anywhere
  in the trajectory (optionally `where` clauses narrow it to calls matching
  specific arguments, using the same matchers as `assert_tool_call`). This
  is the guard-path assertion: "the agent must
  refuse WITHOUT side effects" — and arguably the highest-value one in
  the feature: the assertion proves the agent misbehaved, and its result
  is spec-controlled so the model cannot be steered by a real return
  value. It does NOT by itself stop the tool from executing (flowproof is
  at the model boundary, not the tool boundary) — for a genuinely
  dangerous tool, stub or sandbox it author-side, or use the v3 MCP
  boundary. Scoped to the
  whole trajectory regardless of position; a positional variant can come
  later if the field demands it.

## Argument assertions

Which tool was called is half the test; **what it was called with** is
the other half, and usually where the bugs are.

**Path matchers, partial by default.** Tool arguments are JSON, often
nested. The prose form takes `where` clauses on dotted paths, reusing
the existing matcher vocabulary (`equals`, `contains`, `matches`, plus the
value-less `exists` and `is absent`). The guard path uses the same clauses
on `assert_no_tool_call` to forbid a specific shape of call:

```yaml
- assert_tool_call: create_booking where flight.id equals KQ311
- assert_tool_call: create_booking where passenger.name contains Casey
- assert_tool_call: book_seat where seat matches [0-9]+[A-F]   # volatile shape, not value
- assert_no_tool_call: issue_refund where status equals approved   # guard path
```

`assert_tool_call:` takes a single prose line: a tool name, optionally
followed by one or more `where <path> <matcher> <value>` clauses joined
with `and`. The matchers are `equals` (alias `is`), `contains`, `matches`
(a regex, validated at parse time so a broken pattern fails the spec, not a
replay), plus the value-less `exists` and `is absent` / `is missing`. Paths
are dotted and may index arrays: `passengers.0.name`. Partial matching is
the default: assert the arguments that carry the intent, not the whole
object. The value runs unquoted to the end of its clause, so the one case
this trades away is a value that must itself contain the word `and`.
`${VAR}` refs resolve at execution like everywhere else.

A structured `args:` mapping and an `args_exact:` deep-equality form are on
the roadmap but are NOT in v1: today every argument assertion is the prose
line above.

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

## Running an agent flow

The agent under test is an ordinary process flowproof spawns (`agent.command`).
Five facts about the runtime contract, all exercised by
[`examples/agent-demo/`](../examples/agent-demo/):

- **The prompt arrives in `FLOWPROOF_PROMPT`.** Every `prompt:` step is joined
  by newlines into ONE task string, set on the process environment before it
  starts. v1 delivers the whole task up front and reads the trajectory the
  agent produces; it is a single turn, not a back-and-forth conversation.
- **The proxy URL is injected for you.** flowproof points the agent at its
  local proxy by setting `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OPENAI_BASE`,
  and `FLOWPROOF_LLM_PROXY`, plus a placeholder `OPENAI_API_KEY` so a client
  that refuses to start without a key still starts. `agent.env` is applied
  LAST, so a flow can override any of these for a client that reads a
  different variable.
- **Record needs a real model; replay needs none.** On `record`, name the
  upstream with `FLOWPROOF_AGENT_UPSTREAM` (falling back to an
  `OPENAI_BASE_URL` you already have set) and supply the key through
  `FLOWPROOF_AGENT_KEY`, `ANTHROPIC_API_KEY`, or `OPENAI_API_KEY`. The key
  goes straight into the outbound `Authorization` header (a bare key is
  `Bearer`-wrapped) and nowhere else: the trace stores request bodies only,
  so no key is ever written to disk. `replay` serves the cassette and makes
  zero model calls.
- **`reply` is the final assistant message** of the trajectory, not the
  process's stdout (see "Settled in review").
- **A flow is bounded to 300 seconds.** The agent's own logic decides when it
  is done; if it never finishes, the run fails on the timeout.

## Phasing

1. **v1**: OpenAI-compatible chat-completions proxy (non-streaming),
   `app: agent` process driver, cassette in trace v1 (additive header +
   step artifacts), `assert_tool_call` grammar, trajectory diff on
   re-record.
2. **v2**: Anthropic messages API; streaming replay (serve the recorded
   stream); http-target agents (drive a service instead of a process).
3. **v3**: MCP servers as a second mockable boundary, for systems whose
   tools are external MCP processes rather than internal functions.

## Settled in review

The three questions this design left open have answers, and they are the
same answer three times: a test that quietly tolerates drift stops being a
test.

**Cassette matching is strict, by position, with no tolerance holes.** The
sketch proposed matching a structural envelope plus a normalized prompt
hash, with named holes for volatile spans. Rejected for v1. An edited
prompt template is exactly what this feature exists to catch, so a
matcher with holes in it would be excused from catching the main case.
Turn K of a replay must match turn K of the recording.

Envelope comparison survived, but as a REPORTING rule rather than a
matching one: model, tool names and message roles are compared and
reported before any message body, because a byte diff of two 8000-token
prompts is unreadable and "you added a tool" is a one-line answer.

**Divergence fails at the first bad turn.** No searching forward for a
turn that fits. Once a trajectory has diverged its later turns say
nothing about the system under test, and continuing would report a
cascade whose only real cause was the first failure. Reordering tolerance
can be added if the field ever demands it; nothing has.

**`reply` is the final assistant message in the trajectory.** Not the
process's stdout, which this document originally suggested. Stdout is
whatever a harness chose to print - a banner, a spinner, nothing at all -
and it differs per driver, so a spec would mean different things
depending on what it was pointed at. The last assistant message is the
same fact everywhere, and it is what the agent actually decided to say. A
trajectory whose last turn is a tool call has not replied yet, which is a
real state and reads as absent rather than as empty text.

## Implementation status

Built and tested, each independently:

| Piece | What it does |
|---|---|
| cassette | the recorded trajectory, plus strict positional matching and envelope-first divergence reporting |
| tool-call matching | ordered subsequence, partial dotted-path arguments, the `assert_no_tool_call` guard path |
| proxy | serves a cassette over an OpenAI-compatible endpoint, and in record mode forwards to a real model and captures |
| substitution | rewrites a mocked tool result at the model boundary, identically at record and replay |
| trajectory diff | sorts a re-record into what the agent DID versus what it was TOLD, flagging changes the spec asserts |
| `assert_tool_call` grammar | the prose form |
| `app: agent` | the spec surface, process runner, record/replay orchestration and CLI dispatch, exercised end to end |

Not built yet: per-call result sequences (v1 is one static result per
tool), the structured `args:` / `args_exact:` assertion forms, and the v3
MCP tool boundary. The `matches` argument matcher shipped in 0.3.x. v1's
acceptance bar (a real external agent recording and replaying through the
proxy) is met by [`examples/agent-demo/`](../examples/agent-demo/) (a real
OpenAI-SDK agent against a live model); the in-tree E2E proves the same path
with a fake agent and a fake model.

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
