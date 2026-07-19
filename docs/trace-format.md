# flowproof trace format (v1) — PROPOSAL

Status: **proposed, pending review**. The serde types in `flowproof-trace` are
implemented against this document and the JSON Schema at
[`crates/flowproof-trace/schema/trace-v1.schema.json`](../crates/flowproof-trace/schema/trace-v1.schema.json).

A trace is what the recording agent writes while performing a flow once, and
the only thing the deterministic replayer reads. Design constraints:

- **Replayable with zero LLM calls.** Every step carries the full selector
  ladder with recorded payloads; replay walks the ladder top-down.
- **Diffable and reviewable.** JSON-lines, one step per line, stable key
  order, content-addressed artifacts — so healing produces a small, readable
  diff instead of a silent mutation.
- **Provenance-tagged.** Every selector says which perception source produced
  it (`uia`, `sap-com`, `web`, `vision`), so a step records *why* replay may
  trust it.

## File layout

UTF-8 JSON-lines (`.trace.jsonl`). Line 1 is the **header**; every following
non-empty line is a **step**. Consumers must reject a file whose first line
has `format != "flowproof-trace"` or an unsupported `version`.

## Header line

```json
{"format":"flowproof-trace","version":1,
 "trace_id":"5f0f2f6e-6f0a-4c25-9b1c-1a2b3c4d5e6f",
 "recorded_at":"2026-07-18T10:12:33Z",
 "spec":{"name":"Create sales order","path":"flows/create-order.yaml","hash":"sha256:…"},
 "app":{"name":"SAP GUI for Windows","adapter":"sap-com","window_title":"SAP Easy Access","version":"7.70"},
 "agent":{"backend":"anthropic","model":"<model-id>"},
 "env":{"os":"windows-11","resolution":[1920,1080],"dpi_scale":1.25,"locale":"en-US"}}
```

- `spec` links the trace to the YAML flow spec it was recorded from; `hash`
  lets replay detect drift between spec and trace.
- `adapter` is the *primary* perception/adapter mode: `uia`, `sap-com`,
  `web`, or `vision` (vision = Citrix/RDP mode where only pixels exist).
- `app.url` is how replay reaches the app again: the URL for `web`, the
  SAP Logon connection description for `sap` (absent = attach to the
  running session). Either may be a `${VAR}` reference, stored raw and
  resolved at every launch.
- `agent` records provenance of authorship only; replay never uses it.
- Optional `recording` references the authoring execution's recording bundle
  (`{"format": "filmstrip/1", "dir": "...", "started_at"?}`); each step's
  `artifacts.recording {start_ms, end_ms}` maps it into that bundle. Optional
  `redaction` carries the masking rules copied from the spec at record time,
  so every replay masks identically without the spec (see docs/recording.md).

## Step line

```json
{"id":"s0004",
 "intent":"Enter order type ZOR in the Order Type field",
 "action":{"type":"type_text","params":{"text":"ZOR","submit":false}},
 "selectors":[
   {"tier":"native_id","provenance":"sap-com","confidence":1.0,
    "payload":{"id":"wnd[0]/usr/ctxtVBAK-AUART"}},
   {"tier":"structural","provenance":"uia",
    "payload":{"path":[{"control_type":"Window","index":0},{"control_type":"Edit","index":3}]}},
   {"tier":"text_anchor","provenance":"vision",
    "payload":{"text":"Order Type","relation":"right_of","max_distance_px":220}},
   {"tier":"visual_template","provenance":"vision",
    "payload":{"template":"sha256:…","region":[412,318,180,24]}},
   {"tier":"ai_relocation","provenance":"vision",
    "payload":{"context":"The Order Type input in the Create Sales Order header section"}}
 ],
 "sync":{
   "pre":[{"kind":"element_exists","selector_ref":0,"timeout_ms":10000}],
   "post":[{"kind":"ocr_text_present","text":"ZOR","region":[412,318,180,24],"timeout_ms":5000}]
 },
 "artifacts":{"pre_screenshot":"sha256:…","post_screenshot":"sha256:…"}}
```

### Fields

- `id` — unique within the trace, monotonically ordered (`s0001`, `s0002`, …).
- `intent` — the natural-language step description. Never executed; used for
  review, reporting, and as the prompt seed for `ai_relocation`/healing.
- `action.type` — one of `launch`, `focus_window`, `click`, `double_click`,
  `right_click`, `drag`, `scroll`, `type_text`, `press_key`, `wait`,
  `assert`. `params` is action-specific (see schema `$defs`).
  Text params (`type_text` text, assert expectations) may contain `${VAR}`
  **secret references**: the engine resolves them from the environment at
  execution time — recording and every replay — and the trace only ever
  stores the reference, never the value. An unset variable fails closed
  with an error naming it.

  `type_text` variants: an **empty `selectors` array** means "type into the
  element that currently has keyboard focus"; `params.replace: true` marks
  fill semantics — the input's current value is cleared before typing (a
  bare `Clear the … field` step is a replace-typing of the empty string).
  `press_key` carries `{key, modifiers[]}` and never has selectors — it
  goes to the focused element by definition.
- `selectors` — the ladder, ordered deterministic-first. Tiers:
  1. `native_id` — UIA AutomationId, SAP GUI Scripting ID, DOM id/CSS.
  2. `structural` — path through the accessibility/DOM tree.
  3. `text_anchor` — OCR text anchor + spatial relation
     (`left_of|right_of|above|below|inside`).
  4. `visual_template` — content-addressed image patch + expected region.
  5. `ai_relocation` — NL context for model-assisted relocation. Replay
     treats reaching this tier as a **failure that proposes a heal diff**,
     never a silent fix.
  A step records only the tiers its perception sources could produce (a
  Citrix recording may have tiers 3–5 only). `confidence` is optional,
  `[0.0, 1.0]`. Any rung's payload may carry `nth` (1-based) to address
  the nth matching element when a selector legitimately matches several
  (`Type email into the 2nd "Field Name" field`). `nth` indexes the
  adapter's natural match enumeration — document order on the web,
  tree-walk order on UIA, reading order for OCR — so the same trace means
  the same element on every provenance.

  **Replay semantics**: the engine walks rungs in order and acts on the
  first one that resolves to a live element. Tiers 1–3 execute today
  (`text_anchor` currently via accessible-name matching; OCR arrives with
  the vision mode, as does `visual_template`). Matching on any rung other
  than the recorded primary keeps the run green but marks the step — and
  the run — `degraded` in `result.json`, with the matched tier in
  `selector_tier`: the flow still works, the app has drifted, heal the
  trace.
- `sync.pre` / `sync.post` — conditions gating the action / confirming its
  effect. Kinds: `element_exists`, `element_state`, `window_title`,
  `ocr_text_present`, `visual_stable`. Each carries `timeout_ms`.
  `selector_ref` points into this step's `selectors` array by index.
- `artifacts` — content hashes (`sha256:<hex>`) of screenshots taken
  immediately before/after the action. Blobs live outside the trace in the
  artifact store (`.flowproof/artifacts/<hash>`), keeping traces small and
  diffable.

### Assertions

`action.type == "assert"` covers checks as first-class steps. `params.kind`:

- `element_state` — selector resolves and matches `{property: value}`.
  `expect` keys in use: `value_contains`, `value_equals` (+`normalize:
  numeric`), `value_not_contains` (text must be absent), `count` (with
  `value_contains`: exact occurrence count of the TEXT, not an element
  count — provenance-neutral, an OCR adapter counts occurrences in the
  scene the same way), `element_present` (true/false — presence itself is
  the assertion; note this means "the target resolves", not visual
  visibility — a tree-present-but-hidden element counts as present until
  the vision mode adds a true visual check), and `timeout_ms` (the
  auto-wait bound; the resolver runs inside the poll, so the target may
  legitimately appear — or disappear — during the wait).

  `expect.scope: "surface"` marks a **surface-scoped** assertion: no
  selector ladder (the step's `selectors` is empty, `selector_ref` null) —
  the expectation runs against everything readable on the app's surface.
  Each adapter answers its own way: the page text for a browser, the
  foreground window's subtree for UIA, the OCR'd frame for a vision
  adapter. This is how `page shows X` serializes without baking any
  provenance into the trace.
- `ocr_text` — OCR of `region` (or the resolved element bounds) matches
  `text` (`equals|contains|regex`).
- `visual_diff` — region matches `baseline` (a `sha256:` hash) within
  `threshold` (0.0–1.0 normalized difference).
- `sql` — out-of-band DB probe: named `connection`, `query`, `expect`
  (`equals`: first column of the first row as text; `timeout_ms`).
  Credentials are **never** stored in the trace; `connection` is a name
  resolved from `FLOWPROOF_SQL_<NAME>` in the environment at run time
  (recording and every replay), failing closed when unset. The query may
  carry `${VAR}` references, resolved at execution.
- `api` — out-of-band HTTP probe: `request {method,url,body?}`, expected
  `status` (default: any 2xx) and `expect` (`body_contains`, `timeout_ms`).
  The url may carry `${VAR}` references — base hosts and tokens resolve at
  execution and never persist.

## Versioning

`version` bumps only on breaking changes; additive optional fields may land
within v1 (the schema allows unknown extra fields on `payload` and `params`
but nowhere else). Replayers must refuse newer major versions.
