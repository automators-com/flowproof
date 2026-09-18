---
title: "Record and replay"
description: "Record one flow against a real model or application, then replay it deterministically with recognizable results."
---

This tutorial records one agent flow and replays it with zero model calls. You
will finish with a versioned trace and a run report that can fail CI when the
agent calls the wrong tool.

## Before you start

You need:

- Flowproof installed with npm or pip;
- Node.js for the example agent; and
- an OpenAI-compatible model key for the one-time recording.

The runnable example is in
[`examples/agent-demo/`](../../examples/agent-demo/). If you installed
Flowproof from npm, use the Node example below. The Python example has the same
flow and assertions.

## 1. Read the flow

The flow runs a weather agent, supplies a result for its `get_weather` tool,
and asserts the agent's behavior:

```yaml
# examples/agent-demo/weather-node.flow.yaml
name: Weather assistant answers with the forecast (Node)
app: agent
agent:
  command: node examples/agent-demo/weather_agent.mjs
tools:
  - name: get_weather
    result: { city: Nairobi, sky: sunny, temp_c: 26 }
steps:
  - prompt: What is the weather in Nairobi right now? Use your tools.
  - assert_tool_call: get_weather where city contains Nairobi
  - assert: reply contains sunny
```

`assert_tool_call` fails if the agent uses the wrong tool, sends the wrong city,
or calls the tool out of order.

## 2. Record once

Install the agent's SDK, configure the model key, and record:

```bash
npm install openai
npx flowproof config ai
npx flowproof record examples/agent-demo/weather-node.flow.yaml
```

Recording is the only phase that calls the real model. Flowproof runs the real
agent and captures its model exchange as a trace.

## 3. Replay without a model

Run the same flow:

```bash
npx flowproof run examples/agent-demo/weather-node.flow.yaml
```

A successful replay looks like this:

```text
  [PASS] s0001 prompt
  [PASS] s0002 get_weather where city contains Nairobi
  [PASS] s0003 reply contains sunny
PASS: Weather assistant answers with the forecast (Node)
```

At replay, Flowproof serves the recorded model response back to the same agent
and rechecks the trajectory. Replay needs no model key and makes no provider
network call.

Two boundaries matter:

| Boundary | What Flowproof replaces | Does the agent's real tool execute? |
| --- | --- | --- |
| Model boundary with `tools:` | What the model is told the tool returned | Yes |
| MCP tool boundary with `mcp:` | The tool server itself | No |

A bare `prompt:` flow is one turn. Flowproof joins all `prompt:` steps into one
task delivered up front. Use `conversation:` for a real back-and-forth. See
[Agent-boundary testing](../agent-testing/index.md) for conversations, MCP tool
mocking, and the complete assertion contract.

Python users can run
[`weather.flow.yaml`](../../examples/agent-demo/weather.flow.yaml) after
installing the OpenAI Python package.

## Record and replay a UI flow

The same lifecycle applies to a live application. On Windows 10 or 11, this
spec drives Calculator to compute 5 + 3:

```yaml
# calc.flow.yaml
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
```

Record and replay it:

```powershell
flowproof record calc.flow.yaml
flowproof run calc.flow.yaml
```

Flowproof derives `calc.trace.jsonl` from the spec name. Keep the spec and trace
together unless you need an explicit location:

```powershell
flowproof record calc.flow.yaml --out traces/calc.trace.jsonl
flowproof run calc.flow.yaml --trace traces/calc.trace.jsonl
flowproof heal calc.flow.yaml --trace traces/calc.trace.jsonl
```

Directory suite runs ignore `--trace` and resolve each trace by convention. A
relocated trace therefore appears missing in a suite unless it is placed where
the convention expects it.

## Recognize a successful run

A passing Calculator replay prints each step and the report path:

```text
  [PASS] s0001 Type 5
  [PASS] s0002 Press plus
  [PASS] s0003 Type 3
  [PASS] s0004 Press equals
  [PASS] s0005 display shows 8
PASS: Add two numbers (2154 ms) -> .flowproof\runs\20260718T120000.000Z\report.html
```

Exit code `0` means pass, `1` means test failure, and `2` means an execution or
configuration error. Each run bundle contains:

| Artifact | Use it for |
| --- | --- |
| `report.html` | Review steps and synchronized frames |
| `result.json` | Consume the structured verdict and step timing |
| `junit.xml` | Publish test results in CI |
| `recording/` | Inspect captured keyframes |
| `debug/dom.html` and `debug/console.log` | Diagnose a failed web step when available |

Password fields are always masked. Add `redact:` rules for other sensitive
regions. See [Run recording](../recording.md) for capture controls and the
artifact format.

## Choose recording options

| Goal | Option |
| --- | --- |
| Add an animated `recording.gif` | `--video` |
| Capture the initial state, every fifth step, and final state | `--recording-detail low` |
| Disable screenshots and video | `--recording-detail off` |
| Show a synthetic cursor and click halo | `--highlight-cursor` |

These options change evidence artifacts, not execution or verdicts.

## Verify or update a recording

Use `--verify` when a flow is safe to perform twice:

```bash
flowproof record shop.flow.yaml --verify
```

Verification immediately replays the new trace and refuses the recording if it
cannot reproduce itself. It can repeat orders, emails, payments, or other side
effects, so leave it off when repetition is unsafe.

Use `--reuse` when only part of an existing flow changed:

```bash
flowproof record shop.flow.yaml --reuse
```

Flowproof reuses matching, resolvable trace steps and authors only new or
drifted steps. If model authoring is configured, recording can also repair a
failing spec step with a bounded set of edits. Pass `--no-repair` to stop on the
first failure. See [Resilience and healing](resilience.md) for selector fallback
and reviewable healing.

Element actions wait until the target is enabled, stable, and able to receive
events. A gate that does not clear reports the failed condition, such as
`element exists but is disabled after 5000ms`.

## Next steps

- Run multiple flows with manifests, dependencies, and checkpoints in
  [Run a suite](suite-runs.md).
- Add Flowproof to an existing project with [Adopting Flowproof](../adopting.md).
- Use `--json` when another program invokes the CLI. Structured output goes to
  stdout, while per-step progress continues on stderr.
