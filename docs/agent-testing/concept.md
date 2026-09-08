---
title: "The model-boundary concept"
description: "The problem agent testing solves, the key simplification, and how record-replay applies to the model boundary."
---

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

Hermetic at the model boundary is not hermetic at the tool boundary, and
the difference bites hardest at REPLAY. Replay serves the recorded
assistant message - tool calls included - to a live agent process, so a
tool that fired once while recording fires again on EVERY replay, which
for most teams means every CI run. A side effect you accepted once as the
cost of recording is not a one-off. The runtime says so: a flow that mocks
or forbids a tool nothing intercepts prints a warning naming that tool, at
both record and replay. Declaring the tool under `mcp:` with a `result:`
is what actually stops it running, and silences the warning by fixing the
cause.

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
