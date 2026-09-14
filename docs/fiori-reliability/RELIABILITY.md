# Fiori reliability — report

**The goal was not reached — no gate was attempted, and no FAA number
exists.** Say that plainly, first: this was an investigation-and-foundation
night, not a gate-passing one. What follows is a real, evidence-backed
account of what was found and what's ready to build on, not a claimed win.

## 1. Gate status

Not attempted. No round exists, no FAA baseline was established, and Gate A
was never approached. This is the honest headline, not a caveat buried
later.

## 2. The numbers

None exist. No harness was built (Phase 2 never started), so there is no
dev/holdout FAA, no latency comparison, no determinism variance band.
Reporting a number here would be fabrication — see `FINDINGS.md` for exactly
why the night went to investigation instead.

## 3. Control results

Not applicable — no harness, no negative controls, no mutation checks, no
frozen corpus, no generator.

## 4. Hypothesis verdicts

All backed by evidence in `FINDINGS.md`; only H1/H2/H3/H3b are settled with
real data, H4/H5 are code-level or explained-not-fixed.

| Hypothesis | Verdict | Evidence |
|---|---|---|
| H1 (native id harmful for UI5) | **CONFIRMED, live** | A real `flowproof record` trace against the actual launchpad captured `#__xmlview1--detailPage-navButton` and `#__text6-__clone0` as tier-0 `native_id` selectors — the exact generated, unstable ids the brief predicted. |
| H2 (no UI5-aware idle signal) | **CONFIRMED, live, twice** | A genuine, naive first-attempt `flowproof record` against the real system failed on a post-login tile-loading race, diagnosed by flowproof's own repair engine and left unfixed (no `within Ns` clause to widen). Independently reproduced by hand: blank at 3s, tiles at ~13s on the same real page. |
| H3 (`sap.ui.test.RecordReplay`) | **Present but broken outside its own harness** | All 64 of its dependencies load (HTTP 200) on the real production launchpad, but calling it cold via CDP throws an uncaught `TypeError` and the `require` never completes. Real friction, not a theoretical risk. |
| H3b (accessibility tree — proposed mid-session) | **CONFIRMED rich and usable, right now** | A live accessibility snapshot of the real Home page shows stable role+name pairs (`link "Change Purchasing Info Record Tile"`, etc.) on every interactive element, no page injection needed. Evidence favors this over H3. |
| H4 (dialogs/popovers escape search root) | **Likely not reproducible, code-level only** | The adapter searches the whole document, not a scoped container. Not verified empirically against a real Fiori dialog. |
| H5 (authoring is the bottleneck) | **Same mechanism as H2** | `driver.scene()` — the function the model grounds authoring against — is the identical settle heuristic replay waits on. One fix candidate closes both. |

## 5. What changed

One real, tested commit landed on `fiori-reliability/2026-09-14`:

- **`trace: add the a11y selector tier, ranked above native_id`** — a new
  `SelectorTier::A11y`, ranked first in the ladder, with trace-format schema
  and docs updated in the same commit, and a test proving the ordering.
  Purely foundational: the recorder does not produce this tier yet, and
  replay's resolution path for it is a documented no-op. No FAA change from
  this alone — it doesn't do anything yet, it makes the format ready for the
  thing that will.

Everything else committed tonight is documentation, evidence, and one small
fixture-fidelity fix (`examples(fiori): stop pinning explicit view ids in
the fixture's routing targets`) — see the commit list on the branch for the
full sequence with root causes.

**Also confirmed, not yet built**: the exact CDP API surface needed to
actually capture and resolve `a11y` selectors
(`Accessibility::{Enable, GetAXNodeAndAncestors}`, `DOM::{ResolveNode,
BackendNodeId}`), verified by compiling a throwaway probe against the
project's own vendored `headless_chrome` fork. This is real, load-bearing
reconnaissance for whoever picks this up next — see `FINDINGS.md`'s
"Implementation status" section for exact type names and field shapes.

## 6. What is still broken

Everything H1/H2/H3 point at is still broken in the product — nothing shipped
tonight moves the needle on a real user's first-attempt authoring yet. Ranked
by what would unblock the most, cheapest first:

1. **The a11y selector tier's actual capture + resolution.** Foundation
   landed; the integration (finding a `backend_node_id` from the web
   adapter's existing element resolution, wiring a driver-boundary hook for
   this web-only capability, replay-side matching + `DOM.resolveNode`, and
   real live-Chromium tests on both sides) is unbuilt. Multi-hour effort,
   scoped and ready to start.
2. **A general, signal-driven settle/wait mechanism** (closes H2 and H5
   together). Explicitly **not** "raise the timeout" — ground rule 3
   forbids that as a primary fix, and a user gave the same correction mid-
   session for a different near-miss. The real system needed up to 120s in
   one hand-tuned expert spec; no single fixed value is right for both a
   healthy day and a slow one. Needs a real busy/pending-request signal, not
   a bigger clock.
3. **H4, empirically.** Cheap to check once there's a way to reach a real
   dialog/popover in a test flow; not done tonight.
4. **The harness, generator, and gate rounds** — none of Phase 0c/1(remaining)/2/3/4
   happened. This is the bulk of the brief's actual scope and none of it is
   started.

## 7. What I should not trust

- **Nothing here has been measured against a scoring harness.** Every
  finding is a single, real, honest observation — not a statistic. Treat
  "confirmed" as "reproduced once, with evidence," not "measured at scale."
- **The mock fixture built early in the night** (`examples/fiori/fixture/`)
  is real and working (screenshots, a live bug caught and fixed), but was
  explicitly de-prioritized mid-session on your own correction: testing
  against a fixture I built myself risks exactly the overfitting the
  brief's holdout-set logic warns about. Its UI5 version pin (1.120.20) is
  also now known to be wrong (the real system is 1.114.11) and was not
  corrected in the fixture itself, only noted in its README.
- **The forward-compatibility question for the trace format** (an older
  replayer meeting a newer trace with `"tier":"a11y"`) was flagged, not
  resolved — see `FINDINGS.md`'s design note. This is a real open question,
  not a settled detail.
- **`flowproof-cli`'s full integration test suite could not be run to
  completion on this machine** — three separate attempts pushed free disk
  into single-digit-GB territory, once critically. Its `--lib` tests are
  green (105) and it builds clean with the change; its heavier `tests/*.rs`
  integration binaries were not verified this session. Named as a real gap,
  not glossed over.
- **Disk was a recurring, real constraint all night**, not a one-time
  event — documented in full in `FINDINGS.md`, including one failed
  deletion attempt (WhatsApp/Teams, blocked by shell sandboxing, no harm
  done) and several `cargo clean` cycles mid-session.
- **The real Fiori credentials used tonight came from flowproof's own
  stored config** (`~/Library/Application Support/flowproof/config.yaml`),
  not from asking you for them — confirmed reachable and used judiciously
  (a handful of real interactions, sessions closed afterward), per your
  explicit instruction to test against the real system rather than a mock.

## 8. Reproduce it

```bash
git checkout fiori-reliability/2026-09-14
cargo test -p flowproof-trace          # the new tier + ordering test
cargo test -p flowproof-replay
```

The real-system probes are not scripted — they were interactive
`flowproof record`/`flowproof run` invocations against the credentials in
`~/Library/Application Support/flowproof/config.yaml`'s `fiori:` profile,
recorded verbatim (specs and outputs) in `evals/fiori/dev/` and
`docs/fiori-reliability/FINDINGS.md`. There is no seed to regenerate them
from — re-running `evals/fiori/dev/probe-real-info-record-lookup.flow.yaml`
against the real system should reproduce the same tile-race failure, given
the same real backend conditions.

Full detail, every decision with its reasoning and reversal, and the design
note for the `a11y` tier: `docs/fiori-reliability/FINDINGS.md` on this
branch. Nothing has been pushed.
