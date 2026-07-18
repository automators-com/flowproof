# Getting started: your first flow (Windows Calculator)

flowproof records a flow once from a natural-language YAML spec, then replays
it deterministically — zero LLM calls at replay time. This walkthrough drives
Windows Calculator to compute **5 + 3 = 8**.

Requirements: **Windows 10/11** with the Calculator app, Python ≥ 3.9.

## Install

From PyPI (once 0.1.0 is published) or from a built wheel:

```powershell
pip install flowproof
# or: pip install .\flowproof-0.1.0-cp39-abi3-win_amd64.whl
```

Building from source instead? You need Rust and maturin: `pip install .`
from `sdk/python` compiles the engine automatically.

## 1. Write a spec

`calc.flow.yaml` (also in [`examples/calc.flow.yaml`](../examples/calc.flow.yaml)):

```yaml
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
```

## 2. Record

```powershell
flowproof record calc.flow.yaml
```

flowproof launches Calculator, resolves every step to a real UI Automation
element, **actually performs the flow** (you'll see the buttons pressed),
verifies the assertion against the live display, and writes
`calc.trace.jsonl` — one JSON step per line, human-diffable:

```text
Recorded 'Add two numbers': 5 steps -> calc.trace.jsonl
```

## 3. Replay

```powershell
flowproof run calc.flow.yaml
```

Replay is deterministic: it re-resolves the recorded selectors, presses the
same buttons, and evaluates the assertion by reading the display.

```text
  [PASS] s0001 Type 5
  [PASS] s0002 Press plus
  [PASS] s0003 Type 3
  [PASS] s0004 Press equals
  [PASS] s0005 display shows 8
PASS: Add two numbers (2154 ms) -> .flowproof\runs\20260718T120000.000Z\result.json
```

Exit codes: `0` pass, `1` test failure, `2` error. Each run writes a
self-contained bundle under `.flowproof/runs/<timestamp>/`: `result.json`
(the machine surface, including the step→time mapping), `report.html`
(with a step-synchronized frame viewer — click any step to see exactly
what happened), `junit.xml` (one testcase per step, for Jenkins / GitLab /
Azure DevOps / any CI that ingests JUnit — point your test-report collector
at `.flowproof/runs/*/junit.xml`), and `recording/` with the captured
keyframes plus `recording.gif` — the whole run as one animation, paced
like the real execution, embedded at the top of the report. Sensitive
regions are masked before frames are written: declare `redact:` rules in
the spec, and password fields are always masked automatically
(see [docs/recording.md](recording.md)).

Programmatic callers invoking the CLI should pass `--json`: the full
structured report prints to stdout instead of the human-readable lines —
never parse the prose output.

```powershell
flowproof run calc.flow.yaml --json
```

## Python API (the primary surface)

flowproof is built to be driven by programs — usually AI agents — with the
CLI as a thin wrapper over the same library. Every call returns structured
data:

```python
from flowproof import Flow

flow = Flow("calc.flow.yaml")

rec = flow.record()          # RecordResult(trace_path=..., steps=5)

result = flow.run()          # RunResult — truthy iff the flow passed
result.passed                # True
result.steps[4].status      # "passed"
result.steps[4].intent      # "display shows 8"
result.report_path           # Path to the result.json artifact

trace = flow.get_trace()     # {"header": {...}, "steps": [...]} for inspection
```

A failing test is a `RunResult` with `passed=False` (with per-step status
and failure detail) — not an exception. `RuntimeError` is reserved for runs
that could not execute at all.

## Running the end-to-end tests

Two E2E tests drive real apps, both Windows-only and gated on
`FLOWPROOF_E2E=1`:

```powershell
$env:FLOWPROOF_E2E = "1"
cargo test -p flowproof-cli --test calc_e2e -- --nocapture     # needs a desktop VM
cargo test -p flowproof-cli --test notepad_e2e -- --nocapture  # also runs in CI
```

The Notepad one (`examples/notepad.flow.yaml` — type text, assert the
document contains it) runs automatically in CI on `windows-latest`, so the
record→replay spine is proven on every push. Calculator stays a manual VM
walkthrough because GitHub's Windows Server runners don't ship the
Calculator app.

## Web flows (any OS)

The same spine drives browsers through the `web` adapter — this works on
Linux and macOS too, since it runs on headless Chromium rather than Windows
UIA. Specs add a `url:` and use the web vocabulary
([`examples/web.flow.yaml`](../examples/web.flow.yaml)):

```yaml
name: Greet the user
app: web
url: examples/web/greeter.html   # or any http(s):// URL
steps:
  - Type Ada into the name field
  - Press the greet button
  - assert: page shows Hello, Ada
```

```bash
flowproof record web.flow.yaml && flowproof run web.flow.yaml
```

Set `CHROME=/path/to/chrome` if the browser isn't auto-detected. The web E2E
(`cargo test -p flowproof-cli --test web_e2e`, `FLOWPROOF_E2E=1`) runs in CI
on ubuntu.

## Authoring with a model (arbitrary steps)

The rules only know the demo vocabularies. With a model backend configured,
`record` falls back to the **LLM author** for any step the rules can't parse
(web flows today; the model picks targets from the live page's element list
and can never invent a selector):

```bash
export FLOWPROOF_AI_PROVIDER=anthropic        # or openai-compatible
export FLOWPROOF_AI_API_KEY=sk-...            # falls back to ANTHROPIC_API_KEY
flowproof record shop.flow.yaml               # steps in your own words
flowproof run shop.flow.yaml                  # replays with ZERO model calls
```

`--author rules|llm|auto` controls the mode (default auto). The trace header
records `agent: {backend, model}` whenever a model authored steps — reviewers
always know. For a local model: `FLOWPROOF_AI_PROVIDER=openai-compatible`
plus `FLOWPROOF_AI_BASE_URL=http://localhost:8000/v1` (vLLM).

## When the app drifts: fallback selectors and `degraded`

Replay walks each step's recorded selector ladder in order: the native id
first, then structural (control type + accessible name), then a text
anchor. If the primary selector is dead but a fallback rung still finds the
element, the step runs and the flow stays green — but the step and the run
are marked `degraded` in `result.json` (with the matched tier in
`selector_tier`), the CLI prints a `DEGRADED:` line pointing at `heal`, and
`RunResult.degraded` is set in Python. Degraded-but-passing is the signal
to heal the trace *before* the remaining rungs die too:

```text
  [PASS] s0002 Press plus (matched via structural fallback)
PASS: Add two numbers (2154 ms) -> .flowproof\runs\...\result.json
DEGRADED: fallback selectors were needed — the app drifted; run `flowproof heal calc.flow.yaml`
```

## Healing a stale trace

When the app changes and replay fails, `heal` re-authors the flow from the
spec against the live app and proposes a reviewable diff — it never touches
the trace on its own:

```text
$ flowproof heal calc.flow.yaml
  [CHANGED] s0002 Press plus (selectors)
REVIEW: calc.heal.html (before/after with frames)
PROPOSED: review calc.proposed.jsonl then re-run with --apply
$ flowproof heal calc.flow.yaml --apply   # explicit opt-in
```

Alongside the machine-readable proposal, heal writes `<name>.heal.html` — a
self-contained review page with a before/after pair per changed step: the
frames each execution's recording captured for that step (recorded run vs.
re-authored run) plus the step JSON, rendered entirely from the structured
report. Open it to see *what the app looked like* when each version of the
step ran, then decide on `--apply`.

Exit codes: `0` healthy (or applied), `1` changes proposed for review,
`2` error. `--json` emits the structured report (including `diff_html`); the
Python API returns a `HealResult` (with `diff_html: Path | None`) and the
MCP tool mirrors it.

## What's deliberately missing (this is the first slice)

- Only `calc`, `notepad`, and `web` resolve, each with a small vocabulary —
  the rule-based resolver stands in where the AI authoring agent will go
  (healing re-uses the same authoring seam).
