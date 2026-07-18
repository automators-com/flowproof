# flowproof

[![CI](https://github.com/automators-com/flowproof/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/automators-com/flowproof/actions/workflows/ci.yml)
[![PyPI](https://img.shields.io/pypi/v/flowproof)](https://pypi.org/project/flowproof/)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)

Open-source AI-native E2E testing for the apps your existing tools can’t reach - SAP GUI, Oracle Forms, Citrix, and any Windows desktop app. Runs alongside your current test stack; AI authors and heals tests, a deterministic engine replays them in CI.

![flowproof demo: record and replay a Calculator flow](docs/assets/flowproof-demo.gif)

## How it works

**AI authors, deterministic engine executes.** An agent performs a flow once
from a natural-language YAML spec and records a **trace** — the resolved
selectors, actions, and assertions. The trace replays deterministically in CI
with **zero LLM calls**. When the app changes and a step breaks, healing
proposes a reviewable diff — never a silent mutation.

flowproof is built **agent-first**: the primary caller is a program (usually
an AI agent), with humans in an oversight role. Every operation is a library
call returning structured results; the CLI is a thin rendering over the same
code paths.

## Quick start (Windows)

```powershell
pip install flowproof
```

Write a spec — natural-language steps, no selectors:

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

Record once, replay forever:

```powershell
flowproof record calc.flow.yaml   # performs the flow live, writes calc.trace.jsonl
flowproof run calc.flow.yaml      # deterministic replay: per-step PASS/FAIL, exit 0/1
```

Add `--json` for the full structured report on stdout. See
[docs/getting-started.md](docs/getting-started.md) for the complete
walkthrough.

## Python API

```python
from flowproof import Flow

flow = Flow("calc.flow.yaml")
flow.record()                    # RecordResult(trace_path=..., steps=5)

result = flow.run()              # RunResult — truthy iff the flow passed
result.steps[4].status           # "passed"
result.report_path               # result.json artifact for this run

trace = flow.get_trace()         # inspect the recorded trace programmatically
```

## Status

Early. **v0.1 is the first working slice**: the deterministic
record→replay spine, proven end-to-end against real Windows apps via UI
Automation — a Notepad flow records and replays **in CI on every push**
(GitHub's Windows runners), and the Calculator walkthrough runs on any
Windows desktop.

Works today:

- Windows UIA driver (find, invoke, type, read), rule-based NL step resolution
- Web adapter: the same record→replay spine drives browsers via headless
  Chromium — cross-platform, E2E-tested in CI on Linux
- Versioned JSON-lines [trace format](docs/trace-format.md) with a
  provenance-tagged selector ladder
- Deterministic replayer with structured run reports and artifacts
- One wheel (PyO3/maturin) shipping the Rust engine, Python API, and CLI
- MCP server (`pip install flowproof[mcp]`, run `flowproof-mcp`): agents
  drive record/run/get_trace/heal as MCP tools

On the roadmap: LLM authoring agents (Anthropic computer-use + local
OpenAI-compatible), SAP GUI Scripting adapter, vision mode for Citrix/RDP,
self-healing diffs. See [docs/design.md](docs/design.md).

## Repository layout

| Path | What it is |
| --- | --- |
| `crates/flowproof-driver` | Screen/input/UIA driver (Windows-native, stubbed elsewhere) |
| `crates/flowproof-trace` | Trace format, selector ladder, JSON Schema |
| `crates/flowproof-replay` | Deterministic executor + run reports |
| `crates/flowproof-agent` | Spec parsing, step resolution, recorder (model backends later) |
| `crates/flowproof-adapters` | SAP GUI Scripting COM / web adapters (feature-gated) |
| `crates/flowproof-cli` | `flowproof` CLI (thin wrapper over the library) |
| `sdk/python` | The `flowproof` Python package (bundles the engine) |

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Licensed under
[Apache-2.0](LICENSE).
