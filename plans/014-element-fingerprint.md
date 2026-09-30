---
status: proposed
---
# Plan 14 — Element fingerprints and "found at" for page-wide checks

A recording today remembers *how to find* each target (the selector ladder)
but not *what the target was*. And a page-wide check (`page shows X`)
remembers that X was somewhere on the screen, not where. Both gaps cost
flowproof on its own terms (silent wrong matches, false passes, vague
failures), and both happen to be the largest gaps in the Tosca export
(`crates/flowproof-trace/src/tosca.rs`). This plan closes them once, as
evidence in the trace that replay, repair and every exporter read, rather
than as Tosca-specific fields.

## Evidence

Measured on every committed web and SAP trace in the repository (16 traces,
317 steps) with `flowproof export --format tosca-json`, 0.23.19:

- 131 steps (41%) cannot be exported. 98 of them are page-wide checks, 31 are
  web targets without a plain DOM id, 2 are API assertions.
- Only 13 of 99 web selector rungs carry an `a11y` rung (role + name). The rest
  are a bare `css`, so nothing records whether `#make` was a dropdown, a text
  box or a button.
- `Select Volvo from the "id:make" field` is recorded as `type_text`
  (`crates/flowproof-agent/src/rules.rs`, the Select arm). The trace cannot say
  a choice was made from a list.
- `surface_text()` (`sap_com.rs`, `web.rs`) returns one concatenated string.
  Where the matched text sat is discarded at the moment it is known.

## What goes wrong without it, export aside

1. **False passes on page-wide checks.** On SAP, `surface_text()` includes
   field values (`SapElement.text`, `sap_com.rs`), so `Type 4711 into "Order"`
   followed by `page shows 4711` passes on the field itself. On web an
   input's value is not in `innerText`, but the accessible-name list is, so
   `page shows "Search"` passes on the search box's own label. Nothing in the
   trace tells a reviewer, or a replay, that the only match was the flow's own
   control.
2. **Silent wrong matches.** A selector that still resolves after the element
   changed meaning (`#make` is now a free-text field) replays green. The
   ladder answers "found something"; nothing checks it is the same kind of
   thing.
3. **Vague failures.** "Element not found" when the real story is "you are on
   Login, not Checkout", because no page title was recorded for the step.

## Design

### The fingerprint

A per-step, optional description of the resolved target at record time:

| Field | Web | SAP GUI |
|---|---|---|
| `kind` | tag plus input type or role (`select`, `input[text]`, `button`) | control type (`GuiCTextField`, `GuiComboBox`) |
| `name` | `name` attribute | technical `Name` (`VBAK-AUART`) |
| `label` | accessible name or visible label, capped at 80 chars | tooltip or attached label |
| `title` | page title, raw, capped at 80 chars | caption of the active `GuiMainWindow`/`GuiModalWindow` (SAP has no `page_title`) |
| `app` | origin | transaction code |

Stored raw, never normalised at record time: a trace is diffable evidence
(`docs/trace-format.md`), and committed traces already assert volatile titles
verbatim (`examples/sap/audit-sales-order.trace.jsonl` checks "Display
Standard Order 314: Overview"). Normalisation happens at the reader: one
`title_shape()` helper in `flowproof-trace` (whitespace and digit runs
collapsed) used by replay's comparison and by the Tosca exporter, which turns
digit runs into `*` in `context`.

Captured through one new `AppDriver` hook, `fingerprint(&UiaSelector)`, with
the same contract as `a11y_hint` (`crates/flowproof-driver/src/app.rs`): best
effort, `Ok(None)` when unavailable, never an error. Web reads it over CDP
from the element the selector just resolved. SAP reads `SapElement.kind`,
`name` and the active window's caption from the tree it already walks,
honouring `active_window` so a modal's caption wins. Other adapters keep the
default.

On Fiori, the web fingerprint also records `ui5_type` (`sap.m.Input`) and
`ui5_id` when `window.sap.ui.core` exists and the id does not start with
UI5's generated `__` prefix. They are descriptive fields, never a selector
rung, with no `sap.ui.test` dependency: the same line the `a11y` tier drew
(`docs/fiori-reliability/FINDINGS.md`). They are what the migrator's
`Html_CBTA` strategy keys on, and they give the 31 id-less web targets
something stable.

### "Found at" for page-wide checks

When a surface check passes at record time, the driver is asked once more:
`locate_text(needle)` returns one element, deterministically: the first match
in document order (what `nth` already means in the format) that is not a
control this flow typed into, plus `matches: N` when there was more than one.
`where` is a closed enum: `element`, `statusbar`, `title`, `own_input`.

- SAP: the element whose text or tooltip matched, with the status bar
  (`wnd[0]/sbar`) and the window title as named cases.
- Web: the smallest visible element whose rendered text contains it, as a
  selector (id if it has one, otherwise the same ladder a target gets).

If the only match is a control the flow itself typed into (the recorder's
`typed_at` journal), it is recorded as `where: "own_input"` and record warns.
That is the false pass in (1), caught when it is authored instead of never.
On SAP the own-input exclusion is load-bearing: the status bar sits last in
tree order, so plain document order would pick the field.

### In the trace

One new optional step field, strictly additive like `guards`:

```json
"observed": {
  "fingerprint": {"kind": "select", "name": "make", "label": "Make", "title": "Enter Vehicle Data"},
  "found_at": {"where": "element", "selector": {"tier": "native_id", "provenance": "web", "payload": {"css": "#quote-status"}}}
}
```

A step field, not more selector payload: a rung is a way to *find* an
element and replay tries each one; the fingerprint describes the element and
must never be tried as a locator. Traces without the field replay exactly as
today, and committed cassettes are not rewritten. The field appears when a
flow is next recorded. `docs/trace-format.md` and
`crates/flowproof-trace/schema/` change in the same PR (CI ratchet). The
schema's `step` is `additionalProperties: false`, so `observed` gets its own
`$defs` entry with closed enums, not an open object like `payload`.

Two `heal` consequences: `diff_steps` must not compare `observed`, or
volatile titles make every heal report "changed"; and an older engine's
`heal --from-run` re-serialises through `Step` and silently drops the field
from promoted steps (forward-compat note in the format doc).

### Privacy: labels, never values

Captured values are kept out of traces on purpose (`Action::Capture` stores
only the name). The fingerprint follows the same rule:

- Stored: tag, type, role, `name` attribute, id, accessible name or label.
- Never stored: an input's value, textarea content, or the matched text of a
  `found_at` (the needle is already in the step's `expect`, raw).
- A fingerprint string that contains any value the recorder resolved from a
  `${VAR}` in this run (at least `MIN_SECRET_LEN` long) is refused, not
  stored. No such set exists yet: `secret_scan` only resolves variables
  declared in `assert_no_secret_leak`, so slice 1 collects the value of every
  `resolve_refs` call the recorder makes, and ships a red-path test (every
  Fiori trace types `${FIORI_USER}` at s0001).

### Replay: advisory drift, never a changed verdict

- After a target resolves, its live fingerprint is compared with the recorded
  one. A different `kind`, or a different `title` on the step's page, is a
  drift warning in the report, next to the existing "matched via <tier>
  fallback" line (`crates/flowproof-replay/src/report.rs`, `cli/src/lib.rs`).
- After a surface check passes, the text is looked for at `found_at`. Found
  elsewhere is a drift warning, not a failure: text that moved from the status
  bar to a popup still satisfies what the author wrote.
- Verdicts do not change, and there is no fail-on-drift mode. The verdict is
  about what the author asserted (`CHARTER.md` §1); a green step whose title
  changed is not a false green, and making verdicts depend on titles would
  turn every volatile page into a red run. Drift is exposed structurally
  instead: `drift: [..]` per step and a run-level `drifted` in the report and
  `result.json` (mirrored in the Python `RunResult`), next to `degraded`. A
  consumer that wants red reads that field; an author who wants the title in
  the contract writes `page title is …`. The `result.json` addition is a
  public-API change and stays additive with defaults.

### Consumers

- **Tosca export** reads `found_at` to turn `page shows X` into
  `Verify *X*` on that element, or the migrator's `statusbar` keyword for the
  SAP status bar, with the migrator's `existpage` as the web fallback. It
  reads the fingerprint for `context` (title), `business_type` (kind, so a
  dropdown exports as ComboBox) and identification properties for targets
  without an id, through a `Flowproof_Html` steering entry the migrator adds.
- **Other exporters** later read the same data.

## Slices

Each under the 400-line cap, each merged before the next starts. Replay
comparison comes before `found_at`: it is what proves a captured field is
stable across runs.

1. `observed` trace type, schema `$defs`, format docs, the driver hook's
   default, recorder plumbing, the resolved-value refusal, all tested with the
   mock driver.
2. Web fingerprint capture. Run the web E2E suite locally before merging; it
   runs only on `main`.
3. SAP fingerprint capture (active window caption, type, Name), tested
   against the fake SAP engine.
4. Replay drift: `drift` and `drifted` in the report, `result.json` and
   `RunResult`, shown on the same line as the fallback note.
5. `observed.found_at` for SAP and web surface checks, with the `own_input`
   warning.
6. UI5 fields in the web fingerprint, tested on `examples/fiori/fixture` plus
   a Fiori E2E run.
7. Tosca export reads both.

Re-measuring export coverage needs the corpus re-recorded, which is
human-only (committed cassettes are never rewritten by a change). The SAP
traces need Windows and a licensed system.

## Decided (2026-09-30)

- Titles are stored raw and normalised only by readers (`title_shape()`).
  Drift is warned once per run of consecutive steps sharing a title.
- `found_at` records one deterministic element plus a match count.
- No fail-on-drift mode; drift is a structured field consumers can gate on.
- UI5 type and stable id are fingerprint fields, in their own slice.

## Open questions

- **Windows desktop (UIA).** Out of scope here; the hook's default keeps it
  unchanged.
- **Title shape tolerance.** "3 items" and "12 items" compare equal by shape,
  which is the intent; is there a title class where digits carry the meaning
  and the warning should still fire?
