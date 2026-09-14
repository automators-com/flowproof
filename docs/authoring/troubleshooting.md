---
title: "When authoring gets stuck"
description: "What happens when a step cannot be authored, and drafting a spec from a requirement document with author-from-doc."
---

In auto mode, plain freeform UI text (for example, `Smash the shiny
button`) is model intent. The model receives the live scene and must ground
its answer to one of the listed target tokens; it cannot invent a selector.
An explicit `rules:` step instead succeeds or fails against the grammar on
this page and names the accepted forms for that app. Use `--author rules`
or `--author llm` when the entire recording should force one backend.

When auto mode has no configured model (`flowproof config ai`, or
`FLOWPROOF_AI_PROVIDER` / `FLOWPROOF_AI_API_KEY`), the CLI emits a visible
warning before trying the deterministic grammar. This fallback is identified as
its own `fallback` route in
the per-step human and structured diagnostics, so ordinary prose is never
silently mistaken for deliberate rule syntax.

When a step is too *ambiguous* to author at all ("make required field
changes", which fields?, or `Enter it` with several remembered values),
recording fails with a structured **clarification payload**: the stuck step
plus the relevant live-scene fields or remembered-value candidates. It is
available via `record --json`, the MCP record tool, or Python's
`ClarificationNeeded`. The driving agent rewrites the step more precisely
and re-records; see [self-help.md](self-help.md) for the loop.

Whichever route authors a step, recording persists grounded selectors and
actions in the trace. `flowproof run` executes those deterministic artifacts
directly and makes zero authoring-model calls. See
[getting-started](../getting-started/agent-flows.md#authoring-with-a-model-arbitrary-steps).

## Drafting a spec from a requirement document (`author-from-doc`)

QA teams already write test cases in a test-management tool and export them
as a document (today: an HP ALM/Quality Center PDF, with `Step Name` /
`Description` / `Expected` / `Actual` fields per step). `flowproof
author-from-doc` reads that PDF and drafts a `.flow.yaml` spec from it,
instead of hand-translating the same steps twice:

```bash
flowproof author-from-doc uat-export.pdf --app sap --name "Manage purchasing info records" --out draft.flow.yaml
```

- `--app`: the target app id the draft's `app:` field is written with
  (`sap`, `web`, …).
- `--name`: the flow name written into the draft's `name:` field.
- `--out`: where the draft `.flow.yaml` is written.

When the document contains concrete non-secret business data, such as a
material, supplier, plant, customer, or order id, the draft can use
`${NAME}` placeholders and `author-from-doc` writes a sibling values file
next to the flow:

```text
draft.flow.yaml
draft.values.yaml
```

`flowproof record draft.flow.yaml` and `flowproof run draft.flow.yaml` load
that values file automatically. Keep passwords, tokens, and login
credentials in `flowproof config` or the caller's secret environment, not in
the generated values file.

The PDF's text is extracted and segmented into per-step records, then each
`Description`/`Expected` pair is translated into this page's grammar by a
model call, the same "flag, don't guess" discipline as live authoring
above, extended to three distinct outcomes instead of a binary
action-or-not:

- A `Description` that maps cleanly becomes a real step in the grammar
  above, and its `Expected` becomes a real `assert:`, something a video
  recording never has, since a document already states a belief about
  correctness.
- A `Description` that implies an action *within* the app under test, but
  too ambiguously to map with confidence, is flagged as a `# TODO` plus a
  freeform step for the next live `flowproof record` pass to resolve
  against the real screen, never a forced guess.
- A `Description` whose action clearly targets a *different* app or tool
  entirely (e.g. reviewing a downloaded export in a spreadsheet program) is
  flagged as its own distinct `# TODO` rather than silently dropped or
  confused with in-app ambiguity: it isn't automatable here at all, and
  needs a human decision, not a screen to search.

The output is a **draft**: review every step, then run `flowproof record`
to resolve any flagged ones against the live app before trusting it.
