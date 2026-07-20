# flowproof

A generic open-source automation framework for the AI-agent era:
automated testing and agentic process automation across web, desktop,
and Citrix.

**Agents author, a deterministic engine executes.** A flow is described in
YAML with natural-language steps, recorded once against the live app, and
replayed deterministically in CI with zero LLM calls. Built agent-native:
every operation returns structured results a program can reason over, and
agents can drive record/run/get_trace/heal over MCP
(`pip install flowproof[mcp]`, run `flowproof-mcp`).

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
flow.record()            # performs the flow live, writes the trace
result = flow.run()      # deterministic replay -> RunResult
assert result.passed
```

Or from the shell: `flowproof record calc.flow.yaml`, then
`flowproof run calc.flow.yaml` (add `--json` for the structured report).

The wheel bundles the Rust engine; no separate install. Targets: `web`
(headless Chromium, any OS), Windows desktop via UI Automation, `sap`
(SAP GUI Scripting), `vision` (pixels-only for Citrix/RDP), and `api`
(no UI — HTTP/SQL assertion flows, any OS).

**Status: early, in active development (v0.2).** Record→replay,
model-grounded authoring, healing with reviewable diffs, suites, run
recordings, and the MCP server all work and are tested in CI.

Docs and source: [github.com/automators-com/flowproof](https://github.com/automators-com/flowproof)
