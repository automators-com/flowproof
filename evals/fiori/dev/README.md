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
one real app family (Purchasing Info Records), reached from Home via one
of the account's three real tiles: "Display Purchasing Info Record by
Supplier", "Change Purchasing Info Record", or "Procurement Overview"
(touched only for navigation - its own screen content is unexplored). That
narrowness is because this is the only family with confirmed-safe,
already-proven-live navigation as of this writing — see
`examples/fiori/*.flow.yaml` for the flows this corpus builds on. Variety
across specs comes from varying the **navigation path** and **interaction
pattern** (which tile, value help on one field vs. both, a wrong search
before the real one, cross-app confirmation of the same record touched via
a third tile in between, out-of-band OData checks), not from different
business data — there is exactly one confirmed-real test record
(`values.yaml`: Material `TG10`, Supplier `10300001`, Plant `1010`,
Purchasing Org `1010`), reused throughout.

## What is unverified, on purpose

Some steps are still plain-language guesses about real UI mechanics this
session has not yet driven live — the value-help (F4) interaction's exact
mechanics, and the "Clear the ... field" grammar's own iframe-scoping
correctness (a real gap the corpus's first baseline run found and now
deliberately exercises in `medium-05-search-retry-after-no-match.flow.yaml`,
rather than being avoided everywhere). Guessing a concrete selector instead
would record a lie, not a step — `flowproof record` resolves these against
the live screen, and whether it resolves them correctly on the first
attempt is exactly what FAA measures.

## Status

**Scored once** (`evals/fiori/20260914T113725Z.json`, FAA 16.67%) — see
`docs/fiori-reliability/FINDINGS.md`'s "Phase 0c" section for the full,
honest account of that run, including two real confounds it was not clean
of (a since-fixed Chrome-process leak degrading the machine mid-run, and
one corpus-authoring mistake below) and two clean findings it surfaced.

**Corrected after that run**: five specs (`short-02`, `short-03`,
`long-02`, `long-03`, `medium-03`) originally assumed a "PTP Process Area
BU Apps" group tile existed on this account's Home page. It does not — the
real Home page has exactly three My Apps tiles, named above. That
assumption was inherited from `examples/fiori/purchase-info-records-report.flow.yaml`,
which, on closer reading, never actually confirmed it live either (its own
TODO flags it as unresolved). All five specs are rewritten around the
three tiles confirmed real; two were renamed (`git mv`) since their old
names described the removed approach. A clean re-baseline against the
corrected corpus (and the process-leak fix) has not been run yet.

## Holdout

`evals/fiori/holdout/` is empty and stays that way from this side — per
the brief, holdout specs are hand-written by the person running the brief,
in their own words, and never read, debugged against, or tuned for during
the fix loop. Nothing here should populate it.
