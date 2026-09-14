# Fiori Phase 0c dev corpus

14 `.flow.yaml` specs against the **real** corporate Fiori system (not a
local mock fixture — see `docs/fiori-reliability/FINDINGS.md`'s Decisions
log for why that pivot happened), written the way a QA person would write
them: plain language, no selectors, no flowproof internals. Composition
follows the overnight brief's Phase 0c rules:

| Kind | Count | Steps | Files |
|---|---|---|---|
| Short | 4 | 3–6 | `short-01`…`short-04` |
| Medium | 5 | 10–20 | `medium-01`…`medium-05` |
| Long | 3 | 30+, crossing 3+ views, dialog + value help | `long-01`…`long-03` |
| Negative control | 2 | — | `negative-control-*` |

## Read-only, deliberately

Every spec here only navigates, searches, and reads. None of them Save,
Create, or Delete a real record. This corpus works almost entirely within
one real app family (Purchasing Info Records, reached either directly from
Home or through the "PTP Process Area BU Apps" group tile) because that is
the only family with confirmed-safe, already-proven-live navigation as of
this writing — see `examples/fiori/*.flow.yaml` for the flows this corpus
builds on. Variety across specs comes from varying the **navigation path**
and **interaction pattern** (direct tile vs. group tile, value help on one
field vs. both, a wrong search before the real one, cross-app confirmation
of the same record, out-of-band OData checks), not from different business
data — there is exactly one confirmed-real test record
(`values.yaml`: Material `TG10`, Supplier `10300001`, Plant `1010`,
Purchasing Org `1010`), reused throughout.

## What is unverified, on purpose

Several steps are plain-language guesses about real UI mechanics this
session has not yet driven live — the value-help (F4) interaction, the
exact wording of the "PTP Process Area BU Apps" group's sub-tiles beyond
what `examples/fiori/purchase-info-records-report.flow.yaml`'s own TODO
already names, and whether "Navigate to `<url>`" cleanly returns to the
launchpad mid-flow from inside an app. Guessing a concrete selector here
instead would record a lie, not a step — `flowproof record` resolves these
against the live screen, and whether it resolves them correctly on the
first attempt is exactly what FAA measures. Some of these specs are
expected to need correction after their first live `record` attempt; that
outcome is data, not a mistake in how the corpus was written.

## Status

Written, not yet scored. `scripts/fiori-eval.py` has not been run against
this corpus yet (deliberately deferred — see the commit that added these
files) — there is no baseline FAA number for it, and per-spec sha256
freezing (the brief's Verification §2) happens naturally at that first
scoring run, not before.

## Holdout

`evals/fiori/holdout/` is empty and stays that way from this side — per
the brief, holdout specs are hand-written by the person running the brief,
in their own words, and never read, debugged against, or tuned for during
the fix loop. Nothing here should populate it.
