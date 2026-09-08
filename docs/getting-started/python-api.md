---
title: "Python API"
description: "Drive flowproof programmatically: Flow.record(), Flow.run(), and inspecting a trace."
---

flowproof is built to be driven by programs — usually AI agents — with the
CLI as a thin wrapper over the same library. Every call returns structured
data:

```python
from flowproof import Flow

flow = Flow("calc.flow.yaml")

rec = flow.record()          # includes per-step llm/rules/fallback routing
rec = flow.record(author="rules")  # deterministic grammar for all plain steps

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
