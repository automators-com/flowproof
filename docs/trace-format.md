---
title: "Trace format (v1)"
description: "The JSON-lines trace format the recording agent writes and the deterministic replayer reads."
---

The serde types in `flowproof-trace` are implemented against this
document and the JSON Schema at
[`crates/flowproof-trace/schema/trace-v1.schema.json`](../crates/flowproof-trace/schema/trace-v1.schema.json).

A trace is what the recording agent writes while performing a flow once, and
the only thing the deterministic replayer reads. Design constraints:

- **Replayable with zero LLM calls.** Every step carries the full selector
  ladder with recorded payloads; replay walks the ladder top-down.
- **Diffable and reviewable.** JSON-lines, one step per line, stable key
  order, content-addressed artifacts, so healing produces a small, readable
  diff instead of a silent mutation.
- **Provenance-tagged.** Every selector says which perception source produced
  it (`uia`, `sap-com`, `web`, `vision`), so a step records *why* replay may
  trust it.

| Flow shape | Trace representation | Primary sections below |
| --- | --- | --- |
| UI, SAP, vision, and API flows | JSON Lines: one header followed by step lines | [Header line](#header-line), [Step line](#step-line) |
| Agent flows | One JSON document with cassette and optional MCP lanes | [Cassette lane](#cassette-lane-app-agent), [Side-effect lane](#side-effect-lane-app-agent) |

## File layout

UTF-8 JSON-lines (`.trace.jsonl`). Line 1 is the **header**; every following
non-empty line is a **step**. Consumers must reject a file whose first line
has `format != "flowproof-trace"` or an unsupported `version`.

**`app: agent` traces are a different shape.** An agent-boundary flow
(see [agent-testing.md](agent-testing/index.md)) records a self-contained JSON
document (`{"app":"agent","mocks":{…},"cassette":{…}}`), not JSON-lines and
without the header below, because a new app kind gets a new trace shape
rather than bending the step-log format. A step-log reader never opens one.

An agent flow that mocks MCP tool servers adds one more key, `mcp`, a map
from server name to that server's lane. Each lane is
`{"mocks":{…},"calls":[…],"events":[…]}`, where the fields are additive and
skipped when empty, so a trace with no MCP servers (or one recorded before
these fields existed) is byte-identical:

- `calls` is the strict-positional JSON-RPC lane: an ordered list of
  `{"method":…,"params":…,"result":…}`, matched by position at replay (method
  first, then for `tools/call` the tool name, then a field-level diff of the
  arguments). This is the only lane the verdict judges.
- `events` is the server-initiated NOTIFICATIONS captured on the lane (a
  JSON-RPC message with a `method` and no `id`), re-emitted at replay. Each is
  `{"after":<n>,"method":…,"params":…}`, where `after` is the count of client
  calls answered when the notification crossed toward the agent. `after` is an
  emission cue, RECORDED and REPLAYED but never MATCHED, so a notification
  racing at call n versus n+1 changes only the byte ordering, not the verdict.
  (Two further fields, `id` and `answer`, are reserved for the v3.4
  server-initiated REQUEST slice and stay absent until then.)

### Cassette lane (`app: agent`)

The `cassette` key is the recorded trajectory itself: `{"turns":[…]}`, one
entry per model-boundary exchange, matched strictly by position at replay.
Schema:
[`crates/flowproof-trace/schema/cassette-v1.schema.json`](../crates/flowproof-trace/schema/cassette-v1.schema.json).

A multi-turn [`conversation:`](agent-testing/status-and-scope.md#multi-turn-conversations)
flow adds two ADDITIVE fields, both omitted at their defaults so every
single-delivery cassette - including every one recorded before these fields
existed - serializes byte-identical:

```json
"cassette": {
  "turns": [
    {"request": {…}, "response": {…}},
    {"request": {…}, "response": {…}, "delivery_index": 1}
  ],
  "deliveries": [
    {"user": "Cancel order A-4471.", "turn_count": 1},
    {"user": "Yes, go ahead.", "turn_count": 1}
  ]
}
```

- `delivery_index` on a turn (omitted when `0`) is which **delivery**
  produced it. A delivery is a coarser unit than a turn: one `conversation:`
  user message plus every turn it provoked before the agent settled. The
  windows **tile the turn list exactly**: each delivery's turns are
  contiguous, each delivery has at least one turn, indexes are dense and
  ascending, and no turn falls outside a window. A recording that violates
  any of those is refused rather than written, which is what stops a timeout
  or an agent that exited early from minting a passing cassette.
- `deliveries` (omitted when empty) is one entry per delivery in order,
  each `{"user":…,"turn_count":…}`. The `user` text is stored verbatim and
  is NOT redacted: it is the operator's own flow-file content, already
  visible in the `.flow.yaml`.

**`deliveries` is reporting, not authority.** It is never matched against at
replay - the wire never re-sends it - and exists so a cassette or `heal`
diff reads as an actual transcript instead of a bare turn count. What replay
enforces is the turns and their `delivery_index` grouping; editing a
`turn_count` changes what a diff *says*, not what a run *verdicts*.

### Side-effect lane (`app: agent`)

A run the seccomp observation mechanism ran for (Linux, `command:` driver,
supervision engaged) records one more additive key, `side_effects` - schema:
[`crates/flowproof-trace/schema/side-effect-v1.schema.json`](../crates/flowproof-trace/schema/side-effect-v1.schema.json):

```json
"side_effects": {
  "observation": "observed (linux seccomp)",
  "effects": [
    {"kind": "fs_write", "target": "./exports/2025.csv", "op": "unlinkat", "at_ms": 412},
    {"kind": "http_request", "target": "198.51.100.9:443", "op": "tcp", "at_ms": 610}
  ],
  "faults": ["openat2: could not read open_how: EPERM"]
}
```

- `observation` (required) is the tag the recording ran under, in
  observation's vocabulary, never containment's: `observed`, never
  `enforced` - nothing here was prevented (see
  [agent-testing.md](agent-testing/security.md#filesystem-observation)).
- `effects` (skipped when empty), ordered by `at_ms`. Each record carries
  `kind` (`fs_write` or `http_request`; `db_change`/`sap_transaction` are
  RESERVED - in the schema's enum, never emitted), optional `target` and
  `target_note` (why the target is absent or weakened), optional `op` (the
  syscall name for fs, `tcp`/`udp` for http), optional fs-only `flags`
  (the closed renderings, e.g. `O_WRONLY|O_TRUNC`), and `at_ms` -
  monotonic ms since agent spawn, never wall clock, so a re-record does
  not churn on timing alone. Three further fields - `before`, `after`,
  `diff` - are reserved and stay absent: capturing an image of a change
  would be prevention, not observation.
- `faults` (skipped when empty) are the supervisor's adjudication
  failures - an empty `effects` under one is silence, not evidence.

**A record is an observed attempt, never an outcome claim.** An `fs_write`
record means the supervisor saw the syscall and replied CONTINUE before
the kernel ran it, so an `unlinkat` of a file that was not there reads
identically to one that destroyed data. A kept `target` is the NAME the
syscall used, workspace-relative - never a resolution claim: a symlinked
intermediate component can carry the actual victim elsewhere. An
`http_request` record is a connect/send the supervisor itself performed
for the child - stronger evidence, but still the destination, not the
bytes.

**Absence means "no observation mechanism", never "nothing happened".**
A `url:` flow, a non-Linux host, or an unengaged flow serializes
byte-identical to before the lane existed; a present lane with no
`effects` is positive evidence - observed, and clean. One asymmetry is
intended: on macOS an engaged `allow_egress` flow still gets an `egress`
lane (not-enforced containment tag) but no `side_effects` lane, because
observation never ran there - the rule, not a defect.

**Path hygiene.** A raw absolute path never enters the lane. An fs
`target` is kept only when it is workspace-relative by construction: the
captured path minus the workspace prefix and any bare `.` components,
`./`-prefixed, no component ever rewritten. Everything else - outside the
workspace, `..`-bearing traversal forms, unanchored relative paths -
redacts: `target` stays absent, `target_note` carries `sha256:` plus 12
hex of the captured path string, stable per identical input so "the same
file every run" still correlates. The residual is named, not hidden: the
unkeyed hash is a **confirmation oracle** - a reader who can already
guess a candidate path can confirm it against the hash. Not literal
disclosure - and a keyed hash would need a stable key that is itself a
new secret-management surface. The full path still prints to
stderr at run time - the ephemeral channel keeps full fidelity, the
committed artifact does not. An http `target` is `ip:port`, or the
UNRESOLVED `${VAR}` spelling when the destination was admitted by a
`${VAR}`-bearing allow entry - recording the resolved address would leak
what the variable pointed at, the rule the egress lane's `allowed`
already follows.

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
  `web`, `vision` (vision = Citrix/RDP mode where only pixels exist), or
  `api` (no UI at all: the flow is out-of-band assertions only). The
  reserved value `multi` appears only on a multi-surface header (below),
  never as selector provenance.
- `app.url` is how replay reaches the app again: the URL for `web`, the
  SAP Logon connection description for `sap` (absent = attach to the
  running session). Either may be a `${VAR}` reference, stored raw and
  resolved at every launch.
- Optional `app.login_user` (sap) is the user the recording logged in as,
  stored raw like `url`. The identity is part of what a recording *means*:
  an order created by a clerk and one created by an approver are different
  evidence, so a trace that could not name it would not be reviewable.
  There is deliberately **no password field**: the password lives in the
  spec's `login:` block and is resolved fresh at every launch, so a
  committed trace has nothing to leak and nothing to redact. Absent =
  the flow took whatever session was open, from `SAP_USER`/`SAP_PASSWORD`
  or from a human's own login.
- `agent` records provenance of authorship only; replay never uses it.
- Optional `recording` references the authoring execution's recording bundle
  (`{"format": "filmstrip/1", "dir": "...", "started_at"?}`); each step's
  `artifacts.recording {start_ms, end_ms}` maps it into that bundle. Optional
  `redaction` carries the masking rules copied from the spec at record time,
  so every replay masks identically without the spec (see docs/recording.md).
- Optional `session` seeds pre-launch state (`cookies`, `local_storage`) so
  an authenticated flow starts without a login walk; values may be `${VAR}`
  references, resolved at apply time and never stored.
- Optional `mock` is the network-mock ruleset (web flows), copied from the
  spec and applied identically at record and replay: each rule matches a
  request-URL substring (and optionally `method`) and answers it locally.
- Optional `browser` is the launch/emulation config (web flows):
  `viewport` (device emulation), `user_agent`, extra Chrome `args`, and
  `clock` (`{at, timezone}`: a pinned `Date` offset plus a CDP timezone
  override, so a date-dependent flow replays deterministically), and
  `random` (`{seed}`: a seeded `Math.random`, the clock's sibling, so a
  flow against a page that mints random values is deterministic), and
  `downloads_dir` (where downloaded files land, applied via CDP at launch so
  `capture_download` has a fixed place to look; absent means the driver
  creates its own per-launch temp directory). All travel in the header so
  record and every replay run the SAME browser shape.
- Optional `apps` is the surface map of a **multi-surface trace**
  (docs/multi-surface.md): `name -> app object`, each entry the same shape
  as `app`: `{"gui": {"name": "SAP GUI for Windows", "adapter":
  "sap-com", "url": "${SAP_CONNECTION}"}, "portal": {"name": "web",
  "adapter": "web", "url": "${PORTAL_URL}/orders"}}`. When `apps` is
  present, `app` carries the reserved name `multi` with adapter `multi`,
  deliberately not a copy of any one surface, so an engine predating
  multi-surface fails LOUDLY at load (an unknown adapter) instead of
  replaying every step against whichever surface happened to be first.
  Surface names match `[a-z][a-z0-9_-]*`; each step names its surface
  (see `surface` below). A web surface's entry may carry its own
  `browser` block (the same shape as the header-level one, which stays
  the single-surface spelling), applied identically at record and every
  replay so that surface keeps the shape it was recorded on.
- Optional `control` is the named security control this flow validates,
  copied from the spec's `control:` block: `{"id": "...", "title"?: "...",
  "description"?: "..."}`. `id` is the author-chosen dotted lowercase
  identifier, stable across renames and re-records; `title`/`description`
  are free text. Absent for flows without a `control:` block. `flowproof
  audit` reads this to report which controls a repository's traces cover.

## Step line

```json
{"id":"s0004",
 "intent":"Enter order type ZOR in the Order Type field",
 "action":{"type":"type_text","params":{"text":"ZOR","submit":false}},
 "selectors":[
   {"tier":"a11y","provenance":"web","confidence":1.0,
    "payload":{"role":"textbox","name":"Order Type","ancestor_role":"group","ancestor_name":"Header"}},
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

- `id`: unique within the trace, monotonically ordered (`s0001`, `s0002`, …).
- `intent`: the natural-language step description. Never executed; used for
  review, reporting, and as the prompt seed for `ai_relocation`/healing.
- `surface` (optional): the named surface (a key of the header's `apps`)
  that executed this step: how a multi-surface replay knows which driver a
  step belongs to. Absent on single-surface traces, where the header's one
  `app` is the surface; those serialize byte-identically to before the
  field existed. Optional PER STEP even in a multi-surface trace: an
  out-of-band assertion (`assert_api`/`assert_sql`/`assert_spreadsheet`)
  drives no UI and may carry none.
- `guards` (optional): the `when:` blocks the step was recorded inside,
  outermost first, each `{id, condition, expect, selectors}`: the authored
  text, an `element_state` expectation with `timeout_ms: 0` (a condition
  reads state, it never waits) over the guard's own selector ladder or
  `scope: "surface"`. Replay reads each `id` once per run, before the first
  step carrying it, and skips every step whose guard did not hold, naming
  the condition. Every step of one `when:` expansion shares the id, so a
  block that changes what its condition read still runs to its end. Absent
  outside any block; an engine predating it runs the step unconditionally.
- `repeat` (optional): the `repeat:` block the step was recorded inside,
  `{id, pass, max, condition, expect, selectors}`: the reading in the same
  form as a guard, plus the loop's bound and which recorded pass this step
  belongs to. Every step of one expansion shares the id. Replay reads the
  condition before each pass, re-runs the first recorded pass's steps as the
  body while it does not hold, and fails at `max`, so the pass count is the
  app's under replay, not the recording's. Only the outermost `repeat:` is
  carried; a nested one is settled at record time inside the body. Absent
  outside any loop; an engine predating it replays the recorded passes flat.
- `action.type`: one of `launch`, `focus_window`, `click`, `double_click`,
  `right_click`, `hover`, `drag`, `scroll`, `type_text`, `press_key`,
  `upload`, `capture`, `capture_download`, `set_checked`, `wait`, `assert`.
  `params` is action-specific (see schema `$defs`).
  Text params (`type_text` text, assert expectations) may contain `${VAR}`
  **secret references**: the engine resolves them from the environment at
  execution time (recording and every replay) and the trace only ever
  stores the reference, never the value. An unset variable fails closed
  with an error naming it.
  A `type_text` text may also contain a `${captured.<name>}` **capture
  reference**, resolved the same way but from the flow's own captures
  (`action.type == "capture"`) rather than the environment. This is what
  makes a value the app generates per run enterable at all: there is no
  literal to record, so the trace stores the reference and each replay reads
  the value fresh. A name that was never captured fails closed, naming what
  was in scope. Captures resolve before secrets, because a `${VAR}` name may
  not contain a dot.

  A `capture` step carries `{"name": "<name>"}` and, for the **counted**
  reading, `"count": true`: how many elements match, rather than one
  element's text. A new param key rather than a new action type, so a trace
  written before counting existed still loads and an old reader meeting one
  does not misread it as a text capture. Either way the trace holds only the
  name: the number is taken at execution time on record and on every replay,
  so a page that grew a row does not need the trace rewritten. A counted
  capture of **zero** fails rather than remembering `0`: a selector typo
  matches nothing and so does an empty table, and the step that means zero
  is an `assert` with `element_count: 0`.

  A `capture_download` step carries `{"name": "<name>"}` and an optional
  `{"timeout_ms": …}`, and has no selectors: a download belongs to the
  surface, not an element on screen, the same reasoning `press_key` uses for
  the focused element. Like `capture`, only the name is stored: the
  download's resolved path is read at execution time on record and on every
  replay and lives only in that run's captures, never in the trace.

  A `kind: "cell"` payload may carry `row_anchor_also: [...]`, and a
  `kind: "scoped"` payload `anchor_also: [...]`: the ADDITIONAL anchors
  that must all be present in the same row or container, beside the primary
  `row_anchor`/`container_anchor`. A new key rather than a changed one, so a
  reader that knows only one anchor still finds the field it expects; absent
  in every trace written before conjunction existed, which decodes to the
  single-anchor behaviour unchanged.

  A `scroll` step carries `to: "top"|"bottom"`, `into_view: true`, or
  **`to_px: <n>`**: an exact offset from the top of the scroll container.
  A new key rather than a new action, so a trace written before offsets
  still loads.

  A `type_text`, `scroll` or `capture` step whose selector payload is
  `kind: "framed"` acts INSIDE that frame. The action is performed through
  the frame's own document rather than at composited coordinates, which is
  why value-driving actions are recordable there and pointer actions are
  not; see docs/authoring.md.

  A `type_text` step may carry `params.values: [...]`: a **multi-selection**,
  the whole set committed at once. Where `values` is present it is
  authoritative; `params.text` repeats only the first option, so a reader
  that shows text still names something concrete. A consumer that honours
  `text` alone would under-select, which is why `values` is the field to
  read. A new param key rather than a new action type, so a trace written
  before multi-selection existed still loads.

  `type_text` variants: an **empty `selectors` array** means "type into the
  element that currently has keyboard focus"; `params.replace: true` marks
  fill semantics: the input's current value is cleared before typing (a
  bare `Clear the … field` step is a replace-typing of the empty string).
  `press_key` carries `{key, modifiers[]}` and never has selectors; it
  goes to the focused element by definition.

  A **trigger** action (`click`, `double_click`, `right_click`, `hover`) may
  carry an optional `params.dialog` object folding in how a native
  JavaScript dialog (`alert`/`confirm`/`prompt`/`beforeunload`) that the
  trigger opens is answered. A JS dialog blocks JS synchronously, so it
  cannot be a step AFTER the trigger; the disposition is armed BEFORE the
  trigger dispatches and a one-shot listener answers it the instant it opens.

  ```json
  {"disposition":"accept","message":"Are you sure?","match":"contains","reply":"New name"}
  ```

  `disposition` is `accept` (OK, supplying any `reply`) or `dismiss`
  (Cancel/close); `message` is the recorded text matched per `match` (always
  `contains` in v1), omitted to match any message; `reply` is a prompt answer,
  authored input like `type_text` text (a `${VAR}` reference resolves at
  execution, so only the reference travels, never the value the page
  received). The object is **strictly additive**: a trace without it is
  **byte-identical** to before the field existed, and only a trigger action
  ever carries it. A trace that USES `dialog` needs an engine at least the
  version that introduced it: an older replayer ignores the field and never
  arms the handler, so the declared dialog would hang rather than be answered.
  That is a forward-compat note, not a format break. Web-only: non-web adapters
  reject a step carrying `dialog`, since a native desktop message box is a
  real window driven by ordinary steps.
- `selectors`: the ladder, ordered deterministic-first. Tiers:
  1. `a11y`: role + accessible name (+ nearest named ancestor when that pair
     alone isn't unique), read from the browser's own computed
     accessibility tree (Chrome DevTools Protocol
     `Accessibility.getFullAXTree`) rather than anything the page had to opt
     into. Web-only for now. Ranked above `native_id`: on a real SAPUI5 app,
     a generated `native_id` (view-instance- and clone-index-bearing,
     e.g. `__xmlview1--...`) proved less stable across a session than the
     accessible name for the same control (see
     `docs/fiori-reliability/FINDINGS.md`). Payload:
     `{"role", "name"}` required, `{"ancestor_role", "ancestor_name"}`
     optional. Promotion requires a unique match to the originally selected
     DOM node. Replay checks both ancestor name and role when recorded; an
     ambiguous accessibility match falls through to the remaining selectors
     instead of choosing the first control with that name.
  2. `native_id`: UIA AutomationId, SAP GUI Scripting ID, DOM id/CSS.
  3. `structural`: path through the accessibility/DOM tree.
  4. `text_anchor`: OCR text anchor + spatial relation
     (`left_of|right_of|above|below|inside`).
  5. `visual_template`: content-addressed image patch + expected region.
  6. `ai_relocation`: NL context for model-assisted relocation. Replay
     treats reaching this tier as a **failure that proposes a heal diff**,
     never a silent fix.
  A step records only the tiers its perception sources could produce (a
  Citrix recording may have tiers 4–6 only, and never `a11y`, which is
  web-only). `confidence` is optional,
  `[0.0, 1.0]`. Any rung's payload may carry `nth` (1-based) to address
  the nth matching element when a selector legitimately matches several
  (`Type email into the 2nd "Field Name" field`). `nth` indexes the
  adapter's natural match enumeration (document order on the web,
  tree-walk order on UIA, reading order for OCR), so the same trace means
  the same element on every provenance.

  A `structural` rung may instead carry a **cell** payload
  (`{"kind":"cell","column_text":"Status","row_anchor":"Grace Hopper"}`),
  addressing a
  table cell by its column-header text and a row anchor rather than a tree
  path, so a row insert or a column reorder does not move the target (see
  [authoring.md](authoring/assertions.md#scoped-targets-table-cells-and-list-items-by-identity)). Record may attach
  `column_field` / `row_id` hints read from the live grid, used as fallbacks
  if the header text or the anchor later fails to resolve.

  Its sibling is the **scoped** payload, which addresses an element inside a
  container identified by an anchor:

  ```json
  {"kind":"scoped","container":"item","container_anchor":"Invoice 4711",
   "inner_text":"Amount","container_id":"transaction-183VHWyuQMS"}
  ```

  `container` is the literal `item` (the closed list of list-ish roles) or
  the `css:…`/`id:…` string exactly as the spec wrote it; `container_id` is
  the record-time hint (the `row_id` analog), harvested from the container's
  first present of `id`/`data-id`/`data-test`/`data-testid`. The inner
  target's keys are **prefixed** - `inner_text`, `inner_css`, `inner_id`,
  `inner_name` - and that is load-bearing, not cosmetic: an engine that
  predates this rung reads bare `css`/`text` off any structural payload, so
  bare keys here would make it resolve the inner target PAGE-WIDE and pass
  on the wrong element. Prefixed, the old decode yields an empty selector,
  skips the rung, and fails loudly. For the same reason a scoped target
  records **no fallback rung**: an unscoped text anchor would match any
  "Amount" on the page and pass green-degraded on a lie.

  The **framed** payload is the third of the family, for an element inside a
  same-origin iframe:

  ```json
  {"kind":"framed","frame":"checkout","inner_css":"#total"}
  ```

  `frame` is the quoted anchor matched against the iframe's own
  `title`/`name`/`id`/`aria-label`, or the `css:…` string exactly as the
  spec wrote it. The inner keys are prefixed for exactly the reason above,
  and here the stakes are the same: an old engine resolving a bare `css`
  would read the MAIN document, which is precisely the element the frame
  scope exists to exclude. Framed rungs are recorded for assertions only
  (see [authoring.md](authoring/iframes-and-cookies.md#iframes-same-origin-assertions)).

  **Replay semantics**: the engine walks rungs in order and acts on the
  first one that resolves to a live element. Tiers 1–3 execute today
  (`text_anchor` currently via accessible-name matching; OCR arrives with
  the vision mode, as does `visual_template`). Matching on any rung other
  than the recorded primary keeps the run green but marks the step, and
  the run, `degraded` in `result.json`, with the matched tier in
  `selector_tier`: the flow still works, the app has drifted, heal the
  trace.
- `sync.pre` / `sync.post`: conditions gating the action / confirming its
  effect. Kinds: `element_exists`, `element_state`, `window_title`,
  `ocr_text_present`, `visual_stable`. Each carries `timeout_ms`.
  `selector_ref` points into this step's `selectors` array by index.
- `artifacts`: content hashes (`sha256:<hex>`) of screenshots taken
  immediately before/after the action. Blobs live outside the trace in the
  artifact store (`.flowproof/artifacts/<hash>`), keeping traces small and
  diffable.

### Assertions

`action.type == "assert"` covers checks as first-class steps. `params.kind`:

- `element_state`: selector resolves and matches `{property: value}`.
  `expect` keys in use: `value_contains`, `value_equals` (+`normalize:
  numeric`), `value_not_contains` (text must be absent), `count` (with
  `value_contains`: exact occurrence count of the TEXT, not an element
  count; provenance-neutral, an OCR adapter counts occurrences in the
  scene the same way; the ELEMENT count `the "Row" appears N times`
  serializes instead as `element_count` over the step's resolved selector
  ladder), `element_present` (true/false: presence itself is
  the assertion; note this means "the target resolves", not visual
  visibility, since a tree-present-but-hidden element counts as present until
  the vision mode adds a true visual check), and `timeout_ms` (the
  auto-wait bound; the resolver runs inside the poll, so the target may
  legitimately appear or disappear during the wait).

  `expect.scope: "surface"` marks a **surface-scoped** assertion: no
  selector ladder (the step's `selectors` is empty, `selector_ref` null);
  the expectation runs against everything readable on the app's surface.
  Each adapter answers its own way: the page text for a browser, the
  foreground window's subtree for UIA, the OCR'd frame for a vision
  adapter. This is how `page shows X` serializes without baking any
  provenance into the trace.
- `ocr_text`: OCR of `region` (or the resolved element bounds) matches
  `text` (`equals|contains|regex`).
- `visual_diff`: region matches `baseline` (a `sha256:` hash) within
  `threshold` (0.0–1.0 normalized difference).
- `sql`: out-of-band DB probe: named `connection`, `query`, `expect`
  (`equals`: first column of the first row as text; `timeout_ms`).
  Credentials are **never** stored in the trace; `connection` is a name
  resolved from `FLOWPROOF_SQL_<NAME>` in the environment at run time
  (recording and every replay), failing closed when unset. The query may
  carry `${VAR}` references, resolved at execution.
- `api`: out-of-band HTTP probe: `request {method,url,body?,headers?}`,
  expected `status` (default: any 2xx) and `expect` (`body_contains`,
  `timeout_ms`). The url, header values, and body string leaves may carry
  `${VAR}` references: base hosts, tokens, and connection strings resolve
  at execution and never persist. `body` is any JSON, sent for
  POST/PUT/PATCH with an auto `application/json` content-type unless a
  user `content-type` header is present.
- `spreadsheet`: out-of-band file probe: `path` (may carry
  `${captured.x}`/`${VAR}` references; the export this checks is often
  itself a captured download path, resolved at probe time), optional
  `sheet` (the workbook's first sheet when absent), and a cell addressed
  EITHER by `at` (an absolute `A1` reference, e.g. `"B2"`) OR by
  `column`+`row_contains` (a header/anchor pair resolved against the sheet
  like a table cell on a live page: `column` matched exact-after-trim then
  unique-contains against the first row, `row_contains` the unique row where
  any cell contains it), exactly one form, a parse-time error otherwise.
  `expect` (`equals`, `contains`, `timeout_ms`): with neither set, resolving
  the cell is the whole assertion, mirroring `sql`'s bare row-exists check.
  Read via `calamine` directly against the file on disk, not through UI
  Automation over Excel's grid; untested and known-flaky there.

## Versioning

`version` bumps only on breaking changes; additive optional fields may land
within v1 (the schema allows unknown extra fields on `payload` and `params`
but nowhere else). Replayers must refuse newer major versions.

**Open forward-compat question, flagged rather than resolved**: `selectors[].tier`
is a closed schema enum, unlike `payload`/`params`. Adding `a11y` to it (see
above) is safe for every *existing* trace (they never contain it), but an
*older* engine reading a *newer* trace that does contain `"tier":"a11y"` will
fail schema validation on that selector, unlike the `dialog` object above
(which an old engine can simply ignore). Whether that should instead degrade
gracefully — skip the one unrecognized selector and try the next tier in the
list — is a real policy decision this repository has not made yet; see
`docs/fiori-reliability/FINDINGS.md`'s design note for the `a11y` tier.

### Agent segments in multi-surface traces

A surface may use adapter `agent` or `api`. An agent block records one step
with its named `surface` and action `agent_run`. Its params contain `steps`
(the authored agent steps array) and `cassette` (the standalone agent trace
object, including model turns and boundary observations). The embedded
object travels with the trace and run bundle. Replay resolves the named
agent's current configuration from the flow and re-executes against the
recorded cassette without an upstream model. Old readers fail on
`agent_run` rather than skipping it.
