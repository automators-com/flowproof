---
title: "Agent flows"
description: "Testing an AI agent, and authoring arbitrary steps with a model."
---

`app: agent` tests an AI agent at the **model boundary** instead of a UI:
record its tool-call trajectory once against a real model, then replay it
deterministically with zero model calls. The spec drives the agent process,
mocks the tools at the boundary, and asserts the calls it makes:

```yaml
name: Weather assistant answers with the forecast
app: agent
agent:
  command: python3 examples/agent-demo/weather_agent.py
tools:
  - name: get_weather
    result: { city: Nairobi, sky: sunny, temp_c: 26 }
steps:
  - prompt: What is the weather in Nairobi right now? Use your tools.
  - assert_tool_call: get_weather where city contains Nairobi
  - assert: reply contains sunny
```

Recording needs a real model to record against; replay needs none. Point
flowproof at the upstream and give it a key, then record and replay:

```bash
export FLOWPROOF_AGENT_UPSTREAM=https://api.openai.com/v1   # or your endpoint
export FLOWPROOF_AGENT_KEY=sk-...                           # never enters the trace
flowproof record examples/agent-demo/weather.flow.yaml
flowproof run    examples/agent-demo/weather.flow.yaml      # zero model calls
```

flowproof spawns the agent, injects the proxy URL (`OPENAI_BASE_URL` and
friends) and the prompt (`FLOWPROOF_PROMPT`) into its environment, and
captures the trajectory into a cassette. The key rides only the outbound
`Authorization` header and is never written to disk. The full grammar and
runtime contract are in [agent-testing.md](../agent-testing/index.md); the runnable
example is `examples/agent-demo/`.

## Authoring with a model (arbitrary steps)

In the default `--author auto` mode, a plain scalar UI step is
**natural-language model intent**:

```yaml
steps:
  - Enter 24 Market Street in the shipping address field
  - Press the Save button
  - assert: page shows Address updated
```

With an authoring model configured, `record` grounds the plain step against
the live scene on web and Windows desktop apps alike. The driver lists each
actionable or readable element with a provenance-neutral *target token*
(`css:#name` on the web, `id:15` / `text:Close` under UI Automation), and
the model must copy one of those listed tokens verbatim; it cannot invent a
selector. Assertions may also target the literal `surface` token:
everything readable on the current screen, whatever the driver.

The grounded actions and selector ladder are written to the trace. The
model is an author at recording time, not an executor at replay time:
`flowproof run` reads those persisted deterministic actions and makes zero
authoring-model calls.

Write what a person would do; selector and rule syntax are not required. This
includes dragging between visible regions, clicking a particular part of a
control, remembering a value or row count, choosing one or several options,
scrolling an embedded surface, typing in a same-origin frame, and moving focus
with a key. The live inventory includes rendered controls below the fold as
well as stable table, frame, and relational targets. The model may only choose
from that inventory, and Flowproof compiles the choice into the same
deterministic trace used by an explicitly rule-authored flow.

A step is a unit of **intent**, not a single click. One step may cover a whole
form:

```yaml
steps:
  - Fill out all the vehicle data and click next
  - Fill out all the insurant data and click next
  - assert: page shows Select Price Option
```

Each such step is still one model call: the model answers with the sequence of
grounded actions that carries it out, and the recorder performs and verifies
each one exactly as if it had been written out by hand. The trace that results
lists every action individually, so replay is no less deterministic than a flow
whose steps were spelled out field by field. The scene the model works from
carries what a form-filler needs — what each field currently holds, which ones
the page marks required, which boxes are ticked, and a dropdown's exact
options — so a chosen option is one the control actually offers. A password's
value is never included.

```bash
flowproof config ai                           # provider + masked API key prompt
flowproof doctor --ai                         # validates the configured model key
flowproof record shop.flow.yaml               # steps in your own words
flowproof run shop.flow.yaml                  # replays with ZERO model calls
```

Use `rules: <text>` when one step should bypass model authoring and use the
deterministic grammar explicitly:

```yaml
steps:
  - Enter the customer's new address
  - rules: Press the "Save" button
  - assert: page shows Address updated
```

`--author rules|llm|auto` controls the whole recording. `--author rules`
is the global deterministic opt-in for a flow already written in the
[rules grammar](../authoring/index.md); `--author llm` forces model authoring for
plain UI steps. Structured steps such as `assert:` keep their own meaning.

If auto mode has no configured model, recording warns visibly and then
tries the deterministic rules for plain steps. It does not silently change
the route. Human output identifies each step as `rules`, `llm`, `reused`, or `fallback`,
and structured/JSON output exposes the same routing information without
requiring callers to parse terminal prose. The trace also records the
authoring backend and model whenever one participated.

Natural remembered values work across model-authored steps:

```yaml
steps:
  - Remember the order number
  - Enter it in the "Confirmation" field
```

`it` is accepted only when one remembered value is the clear candidate. If
the flow has remembered, for example, both an order number and a customer
number, an ambiguous `Enter it ...` stops with a structured clarification
listing the candidates instead of guessing. Give the value a natural name
to disambiguate it (`Remember the order number as the order ID`, then
`Enter the order ID ...`). For exact rule-authored flows, the explicit
`${captured.name}` syntax remains available:

```yaml
steps:
  - rules: Remember the "id:oid" as oid
  - rules: Type ${captured.oid} into the "Confirmation" field
```

For a local model, set `FLOWPROOF_AI_PROVIDER=openai-compatible` plus
`FLOWPROOF_AI_BASE_URL=http://localhost:8000/v1` (vLLM).
