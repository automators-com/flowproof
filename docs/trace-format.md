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
  `[0.0, 1.0]`.

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
- `ocr_text` — OCR of `region` (or the resolved element bounds) matches
  `text` (`equals|contains|regex`).
- `visual_diff` — region matches `baseline` (a `sha256:` hash) within
  `threshold` (0.0–1.0 normalized difference).
- `sql` — out-of-band DB probe: named `connection`, `query`, expected
  `equals`/`rows` shape. Credentials are **never** stored in the trace;
  `connection` is a name resolved from local config at run time.
- `api` — out-of-band HTTP probe: `request {method,url,body?}`, expected
  `status` and optional `json_path` expectations. Secrets are referenced by
  name, resolved locally, never inlined.

## Versioning

`version` bumps only on breaking changes; additive optional fields may land
within v1 (the schema allows unknown extra fields on `payload` and `params`
but nowhere else). Replayers must refuse newer major versions.
