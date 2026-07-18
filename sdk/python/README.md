# flowproof

AI-native E2E testing for the apps Selenium can't reach — SAP GUI, Oracle
Forms, Citrix, and any Windows desktop app.

**AI authors, deterministic engine executes.** A flow is described in YAML
with natural-language steps, recorded once against the live app, and replayed
deterministically in CI with zero LLM calls. Built agent-first: every
operation returns structured results a program can reason over.

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

```python
from flowproof import Flow

flow = Flow("calc.flow.yaml")
flow.record()            # performs the flow live (Windows), writes the trace
result = flow.run()      # deterministic replay -> RunResult
assert result.passed
```

Or from the shell: `flowproof record calc.flow.yaml`, then
`flowproof run calc.flow.yaml` (add `--json` for the structured report).

The wheel bundles the Rust engine (Windows UI Automation driver); no separate
install. Windows is required to record/run flows — the package imports fine
elsewhere for inspection and tooling.

**Status: early (v0.1)** — the deterministic record→replay spine, proven on
Windows Calculator. LLM authoring agents, SAP GUI adapter, Citrix vision
mode, and self-healing are on the roadmap.

Docs and source: [github.com/automators-com/flowproof](https://github.com/automators-com/flowproof)
