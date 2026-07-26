# The README's demo GIF

`docs/assets/flowproof-demo.gif` is a **recording of a real run**, not a
mock-up. Everything the GIF shows lives here, and every line of output in the
frames was captured from the CLI: if the CLI's wording changes, re-rendering
changes the GIF.

| File | What it is |
| --- | --- |
| `order-status.flow.yaml` | the flow in the GIF - an `app: agent` spec |
| `support_agent.py` | the agent under test: the real OpenAI SDK, one tool, a genuine tool-calling loop |
| `order-status.trace.jsonl` | the recorded cassette, so the demo replays with no model and no API key |
| `fake_model.py` | a local OpenAI-compatible upstream, so `record` needs no API key either |
| `make_readme_gif.py` | captures the three commands and renders the frames |

## Run the demo yourself

From the repository root, with no API key and no network:

```bash
pip install openai                                     # the agent's own SDK
flowproof run scripts/demo/order-status.flow.yaml       # replays the cassette
```

## Re-render the GIF

```bash
pip install pillow openai
python3 scripts/demo/make_readme_gif.py                 # builds the CLI, captures, renders
python3 scripts/demo/make_readme_gif.py --no-build      # reuse target/release
```

The script deletes the trace and re-records it against `fake_model.py`, so the
`record` half of the GIF is a real recording too. The cassette is deterministic:
a re-record is byte-identical, so regenerating the GIF does not churn the trace.

## Why this flow

It is the shortest spec that shows the README's opening claim end to end -
record an agent's run once against a model, replay it with zero model calls, and
assert the tool call and the argument it was threaded with. It deliberately
leaves out two things that would each add half a screen of output:

- **a `result:` mock**, which `examples/agent-demo` covers - the tool here
  returns a deterministic result, so replay needs no substitution;
- **`assert_no_tool_call`**, the guard path - see
  [`docs/agent-testing.md`](../../docs/agent-testing.md) and
  `examples/access-control`. Naming a tool at the model boundary that nothing
  intercepts earns an (accurate) multi-line warning on every run, which is
  right in a terminal and wrong in a five-line hero GIF.
