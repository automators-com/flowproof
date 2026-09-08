---
title: "Resilience and healing"
description: "Fallback selectors and degraded matches when the app drifts, waiting on slow operations, and healing a stale trace."
---

Replay walks each step's recorded selector ladder in order: the native id
first, then structural (control type + accessible name), then a text
anchor. If the primary selector is dead but a fallback rung still finds the
element, the step runs and the flow stays green, but the step and the run
are marked `degraded` in `result.json` (with the matched tier in
`selector_tier`), the CLI prints a `DEGRADED:` line pointing at `heal`, and
`RunResult.degraded` is set in Python. Degraded-but-passing is the signal
to heal the trace *before* the remaining rungs die too:

```text
  [PASS] s0002 Press plus (matched via structural fallback)
PASS: Add two numbers (2154 ms) -> .flowproof\runs\...\report.html
DEGRADED: fallback selectors were needed, the app drifted; run `flowproof heal calc.flow.yaml`
```

## Waiting on slow operations (no sleeps, still deterministic)

Assertions **auto-wait**: the engine polls until the expectation holds or a
bounded timeout elapses (default 10s), during recording and at every
replay. The bound is recorded into the trace, so replay waits exactly as
long as authoring allowed: deterministic, no sleeps in specs. For slow
backend operations, use an explicit wait step (default bound 60s) or a
`within` qualifier on either form:

```yaml
steps:
  - Press the generate button
  - Wait until page shows Generation complete within 120s
  - assert: page shows 100 rows within 5s
```

## Healing a stale trace

When the app changes and replay fails, `heal` re-authors the flow from the
spec against the live app and proposes a reviewable diff; it never touches
the trace on its own:

```text
$ flowproof heal calc.flow.yaml
  [CHANGED] s0002 Press plus (selectors)
REVIEW: calc.heal.html (before/after with frames)
PROPOSED: review calc.proposed.jsonl then re-run with --apply
$ flowproof heal calc.flow.yaml --apply   # explicit opt-in
```

Alongside the machine-readable proposal, heal writes `<name>.heal.html`: a
self-contained review page with a before/after pair per changed step: the
frames each execution's recording captured for that step (recorded run vs.
re-authored run) plus the step JSON, rendered entirely from the structured
report. Open it to see *what the app looked like* when each version of the
step ran, then decide on `--apply`.

Exit codes: `0` healthy (or applied), `1` changes proposed for review,
`2` error. `--json` emits the structured report (including `diff_html`); the
Python API returns a `HealResult` (with `diff_html: Path | None`) and the
MCP tool mirrors it. `--author <auto|rules|llm>` picks the authoring backend
for the re-record, same meaning and same default (`auto`) as `record`'s flag
of the same name.
