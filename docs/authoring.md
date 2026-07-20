# The authoring grammar — every accepted form

`record` resolves each spec step with deterministic rules first; anything
the rules cannot parse falls back to the LLM author (when a model backend
is configured). This page is the **complete rules grammar**. Nothing here
requires a model call, and everything here is covered by a test that
parses the exact examples shown (`documented_grammar_examples_all_resolve`
in `crates/flowproof-agent/src/rules.rs` — if the doc and the code drift,
CI fails).

Conventions: forms are case-insensitive in their keywords. `<text>` is
literal text (may carry `${VAR}` secret references). A quoted `"<label>"`
is a **text anchor** — matched against visible text, accessible label, or
placeholder, exact first then prefix. Two escape hatches work inside any
quoted label: `"css:<selector>"` (web) and `"id:<native id>"` (DOM id,
UIA AutomationId, SAP scripting id). `[2nd ]` marks an optional 1-based
ordinal (`2nd`, `3rd`, `10th`) for when several elements match.

## Actions (web, sap, vision — the generic grammar)

| Step | Notes |
|---|---|
| `Type <text> into the [2nd ]"<label>" field` | text anchor / `css:` / `id:` |
| `Type <text> into the <id> field` | bare native id |
| `Type <text>` | types into the FOCUSED element |
| `Replace the [2nd ]"<label>" field with <text>` | clear + type, one step |
| `Replace the <id> field with <text>` | |
| `Clear the [2nd ]"<label>" field` / `Clear the <id> field` | fill-with-empty semantics |
| `Select <option> from the [2nd ]"<label>" field` | native `<select>`: committed via the value setter, fires `input`+`change` (React-safe). `in the` and `… dropdown` also accepted |
| `Press the [2nd ]"<label>" button` / `Press the <id> button` | |
| `Click [the [2nd ]]"<text>"` | tabs, links, menu options, rows |
| `Press <Key>` / `Press <Mod>+<Key>` | `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Space`, arrows, `Home`/`End`, `PageUp`/`PageDown`; chords `Control+V`, `Alt+Shift+Backspace` |
| `Go to <path-or-URL>` / `Navigate to <path-or-URL>` | relative paths resolve against the flow URL's origin; on SAP this is transaction navigation (`Go to /nVA01`) |
| `Reload the page` | web |
| `Wait until page shows <text> [within <N>s]` | long-bound auto-waiting assert (default 60s) |

## Assertions (every app — the shared grammar)

All assertion forms **auto-wait** (default 10s, recorded into the trace);
append `within <N>s` to any form to change the bound.

| Assert | Meaning |
|---|---|
| `page shows <text>` | the whole surface (page text / window subtree / SAP session / OCR frame) contains `<text>` — `the page shows <text>` also accepted |
| `page shows <text> <N> times` | exact occurrence count of the TEXT |
| `page does not show <text>` | waits for it to be GONE |
| `the [2nd ]"<label>" field contains <text>` | input VALUE, by label |
| `the <id> field contains <text>` | input VALUE, by native id |
| `the [2nd ]"<target>" shows <text>` | element-scoped substring |
| `the [2nd ]"<target>" is visible` / `is not visible` | target resolves / does not resolve |
| `the [2nd ]"<target>" is enabled` / `is disabled` | platform enabled state (`disabled`/`aria-disabled` on web, UIA IsEnabled on desktop) |

## Out-of-band assertions (any app; structured steps, not prose)

```yaml
- assert_sql:
    connection: reporting        # resolved from FLOWPROOF_SQL_REPORTING
    query: SELECT count(*) FROM orders WHERE ref = '4711'
    equals: "1"
- assert_api:
    request: GET ${API}/orders/4711
    status: 200
    body_contains: "confirmed"
- assert_api:                    # authenticated JSON POST
    request: POST ${API}/connections/test
    headers:
      Authorization: Bearer ${SESSION_TOKEN}
    body:
      provider: postgres
      connectionString: ${TEST_CONN_STRING}
    status: 200
    body_contains: "Database not yet supported!"
```

`headers` values and `body` string values may carry `${VAR}` refs — the
trace stores only the raw reference; tokens and connection strings resolve
when the probe fires (record and every replay). `body` is any YAML
(mapping, list, or string), sent as JSON with `content-type:
application/json` unless you set your own `content-type` header — yours
wins. A `body` on GET/HEAD/DELETE is rejected at parse time.

## App sugar

- **calc**: `Type <digits>` (one press per digit), `Press
  plus|minus|times|divided by|equals`, `assert: display shows <number>`.
- **notepad**: `Type <text>`, `assert: document contains <text>` (plus the
  shared grammar).

## When a step doesn't parse

The error names the accepted forms for that app. Anything freeform (e.g.
`Smash the shiny button`) is handled by the LLM author when a model
backend is configured (`FLOWPROOF_AI_PROVIDER` / `FLOWPROOF_AI_API_KEY`)
— the model grounds the step against the live scene and can never invent
a selector; replay stays zero-model either way. See
[getting-started](getting-started.md#authoring-with-a-model-arbitrary-steps).

When a step is too *ambiguous* to author at all ("make required field
changes" — which fields?), recording fails with a structured
**clarification payload**: the stuck step plus the live screen's field
inventory, via `record --json`, the MCP record tool, or Python's
`ClarificationNeeded`. The driving agent rewrites the step into concrete
grammar and re-records — see [self-help.md](self-help.md) for the loop.
