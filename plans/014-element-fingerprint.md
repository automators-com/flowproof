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

1. **False passes on page-wide checks.** `Type 4711 into "Search"` followed by
   `page shows 4711` passes on the search box itself. Nothing in the trace can
   tell a reviewer, or a replay, that the only match was the flow's own input.
2. **Silent wrong matches.** A selector that still resolves after the element
   changed meaning (`#make` is now a free-text field) replays green. The
   ladder answers "found something"; nothing checks it is the same kind of
   thing.
3. **Vague failures.** "Element not found" when the real story is "you are on
   Login, not Checkout", because no page title was recorded for the step.
4. **Repair ranks blind.** `heal` choosing between candidates has only the
   selector strings to compare.

## Design

### The fingerprint

A per-step, optional description of the resolved target at record time:

| Field | Web | SAP GUI |
|---|---|---|
| `kind` | tag plus input type or role (`select`, `input[text]`, `button`) | control type (`GuiCTextField`, `GuiComboBox`) |
| `name` | `name` attribute | technical `Name` (`VBAK-AUART`) |
| `label` | accessible name or visible label, capped at 80 chars | tooltip or attached label |
| `title` | page title | window title |
| `app` | origin | transaction code |

Captured through one new `AppDriver` hook, `fingerprint(&UiaSelector)`, with
the same contract as `a11y_hint` (`crates/flowproof-driver/src/app.rs`): best
effort, `Ok(None)` when unavailable, never an error. Web reads it over CDP
from the element the selector just resolved. SAP reads `SapElement.kind`,
`name` and the window caption it already walks (`sap_com.rs`). Other adapters
keep the default.

### "Found at" for page-wide checks

When a surface check passes at record time, the driver is asked once more:
`locate_text(needle)` returns where the text was found:

- SAP: the element whose text or tooltip matched, with the status bar
  (`wnd[0]/sbar`) and the window title as named cases.
- Web: the smallest visible element whose rendered text contains it, as a
  selector (id if it has one, otherwise the same ladder a target gets).

If the only match is an editable field the flow itself typed into, record
says so as a warning at record time. That is the false pass in (1), caught
when it is authored instead of never.

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
`crates/flowproof-trace/schema/` change in the same PR (CI ratchet).

### Privacy: labels, never values

Captured values are kept out of traces on purpose (`Action::Capture` stores
only the name). The fingerprint follows the same rule:

- Stored: tag, type, role, `name` attribute, id, accessible name or label.
- Never stored: an input's value, textarea content, or the matched text of a
  `found_at` (the needle is already in the step's `expect`, raw).
- Titles and labels go through the resolved-secret scan
  (`flowproof-trace/src/secret_scan.rs`) and are refused, not stored, when a
  secret value appears in them.

### Replay: advisory drift, never a changed verdict

- After a target resolves, its live fingerprint is compared with the recorded
  one. A different `kind`, or a different `title` on the step's page, is a
  drift warning in the report, next to the existing "matched via <tier>
  fallback" line (`crates/flowproof-replay/src/report.rs`, `cli/src/lib.rs`).
- After a surface check passes, the text is looked for at `found_at`. Found
  elsewhere is a drift warning, not a failure: text that moved from the status
  bar to a popup still satisfies what the author wrote.
- Verdicts do not change. A strict mode that fails on drift is an open
  question, opt-in if it ever lands.

### Consumers

- **heal** ranks candidates by fingerprint agreement before confidence.
- **Tosca export** reads `found_at` to turn `page shows X` into
  `Verify *X*` on that element, or the migrator's `statusbar` keyword for the
  SAP status bar, with the migrator's `existpage` as the web fallback. It
  reads the fingerprint for `context` (title), `business_type` (kind, so a
  dropdown exports as ComboBox) and identification properties for targets
  without an id, through a `Flowproof_Html` steering entry the migrator adds.
- **Other exporters** later read the same data.

## Slices

Each under the 400-line cap, each merged before the next starts:

1. `observed.fingerprint`: trace field, docs, schema, the driver hook, web
   capture. Run the web E2E suite locally before merging; it runs only on
   `main`.
2. SAP fingerprint capture, tested against the fake SAP engine.
3. `observed.found_at` for SAP and web surface checks, plus the record-time
   "matched only your own input" warning.
4. Replay drift warnings in the report and `run --json`.
5. Tosca export reads both; coverage re-measured on the committed traces.

## Open questions

- **Volatile titles.** Titles with counts, dates or user names would drift on
  every run. Store raw and compare with a tolerance, or normalise digits at
  record time? Normalising loses evidence; raw comparison is noisy.
- **Several matches for `found_at`.** Record the first in document order, or
  all of them up to a cap?
- **Strict mode.** Is there a real user for "fail on drift", or does the
  warning suffice?
- **Fiori/UI5.** Should the web fingerprint read UI5 control types
  (`sap.m.Select`) where present? They are more stable than tags on Fiori.
- **Windows desktop (UIA).** Out of scope here; the hook's default keeps it
  unchanged.
