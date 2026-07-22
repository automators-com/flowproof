# flowproof field-hardening loop - state

Loop state lives here, never in an agent's context. Read this file FIRST every
iteration, write it BEFORE finishing. Untracked on purpose: it is loop
bookkeeping, not product source, and it lives in the main checkout
(`/Users/aminchirazi/Projects/flowproof/.loop/`) so it survives worktree churn.

## Current

- **PHASE**: `IMPLEMENT` - working the TRACKER BATCH per verdict 6.
- **flowproof version last shipped**: 0.2.5 (PyPI)
- **Open PR**: [#79](https://github.com/automators-com/flowproof/pull/79) -
  GAP-E checkbox verbs + `is checked` assertion. CI running. Verified against
  the live field app (MUI Checkbox) and the full web E2E (18 passed).
- **Target 1** `cypress-io/cypress-realworld-app` reached SOLID: 15 of 15 on
  released 0.2.5. Target 2 is `documenso/documenso`, then **nocodb** (which
  verdict 6 names as the forcing function for #58 table-cell addressing).

## TOOLCHAIN TRAP (2026-07-22) - local clippy was not authoritative

PR #80 failed CI on `clippy::question_mark` while local clippy passed. Cause:
CI uses `dtolnay/rust-toolchain@stable` (1.97) and this machine was on
**1.95**. Lints differ between releases, so a local clippy pass on an older
toolchain proves nothing. This repo had already been bitten by the SAME lint
once (`fix(clippy): satisfy question_mark lint in decode_step on Rust 1.97`).

Fixed by `rustup update stable` - now 1.97.1, matching CI. **Re-run
`rustup update stable` whenever CI reports a lint that passes locally**, and
treat a local clippy pass as meaningless if `rustc --version` does not match
what CI is running.

Cheap habit that avoids the whole class: prefer `?` over a trailing
`else { return None }`, which is what the lint wants anyway.

## WORKTREE TRAP (2026-07-22) - cost a wrong-branch commit

`git checkout -B <name>` in a worktree SILENTLY FAILS when that branch name
is already checked out in another worktree (including the main checkout).
The capture commit landed on the already-merged checkbox branch, and the
push went to a stale branch that GitHub then reported as having "no commits
between main and ...".

Rule: after `git checkout -B` in a worktree, verify with
`git branch --show-current` before committing. `git worktree list` shows
which branch each checkout holds. Recovery is a cherry-pick onto
`origin/main`, which also drops the already-merged parent commit.

## TRACKER BATCH progress (verdict 6 sequence)

| # | Item | State |
|---|---|---|
| GAP-E | checkbox verbs + assertion | **MERGED** (#79, main `515f70c`) |
| GAP-O | element count `appears N times` | ruled (verdict 5), next after #57 |
| #57 | computed assertions (captures) | **BUILT**, PR #80, CI running |
| #66+#68 | Windows app mapping + `window:` config | ruled, needs a windows-latest E2E as merge gate |
| #67 | generic UIA actions | ruled, after #66 |
| #69 | vision word-level OCR | ruled, testable on any platform |
| #70 | UWP docs (+ timeboxed uwp-e2e job attempt) | ruled, rides along |
| #60/#61 | agent-boundary testing | #60 after the Windows batch; #61 gated on it |
| #58 | table-cell addressing | parked until nocodb evidence |
| #32 | real SAP E2E | parked on org infrastructure |

**Nothing on the tracker gets closed** - verdict 6 looked for deletions
deliberately and found none.

## RE_VERIFY PASSED## RE_VERIFY PASSED - 2026-07-22, against RELEASED 0.2.5

`flowproof run specs/` -> **15 of 15 flows passed, exit 0.** This is the run
that lets the round's gaps be called FIXED: the hard rule requires an
empirical test against a released wheel, and this is it.

| Gap | Verdict | Evidence on the released wheel |
|---|---|---|
| GAP-A transport death | **FIXED** | all 6 authenticated flows replay; 04-auth-signup runs 25 steps in 210 s |
| GAP-B suite abort | **FIXED** | 15 flows all ran; merged junit written |
| GAP-N below-the-fold gate | **FIXED** | 05-user-settings and 04-auth-signup both pass |
| GAP-J UIA name leak | **FIXED** | no `UIA query failed` in any web run |
| GAP-F case sensitivity | **FIXED** | 08-transaction-feeds asserts `page shows Friends` against a "FRIENDS" tab |
| GAP-D url assertion | **FIXED** | 01-auth-redirect uses `page url is /signin`, a one-to-one Cypress port |
| GAP-G env_from ordering | **FIXED** | verified SEPARATELY: stripped the shell fallbacks from `mint-session.sh` so the script depends entirely on the suite's `env:`, and 09-api-users still passes |

Note GAP-G needed its own test - the suite run alone could not prove it,
because the mint script's fallback defaults masked the fix. Removing them
was the actual experiment.

## STILL OPEN on this target (carried to the next cycle)

- **GAP-O** element-count assertion - syntax ruled (verdict 5), NOT built.
- **GAP-E** checkbox verbs - syntax ruled (verdict 4b), NOT built.
- **GAP-P** no clock control (`cy.clock`) - NEW, unruled, P1. Blocks any
  date-dependent test from being deterministic.
- **GAP-Q** computed navigation in a picker - NEW, unruled, likely REJECT.
- **GAP-M** computed assertions - parked by design (issue #57).
- 165 of the target's 180 tests remain unported. The suite is representative,
  not exhaustive, and that is a deliberate stopping point rather than a claim
  of completeness.

## ISSUE TRACKER vs REALITY (2026-07-22)

Checked all 10 open issues against this cycle's work: **none are solved by
it.** They are Windows (#66, #67, #68, #70), vision (#69), SAP (#32),
agent-boundary testing (#60, #61), and two deliberately parked designs
(#57 computed assertions, #58 table-cell addressing). This loop touched none
of those areas.

The real mismatch runs the other way: **seven fixes shipped across 0.2.4 and
0.2.5 with no issue trail at all**, because the field loop goes finding ->
PR directly. If the tracker should mirror the changelog, the work is
back-filling issues for what shipped, not closing what did not. Raised with
the maintainer; no issues were opened or closed by the loop.

## WHY THE LOOP STOPPED TOO EARLY## WHY THE LOOP STOPPED TOO EARLY (2026-07-22) - do not repeat

I paused the loop while five PRs sat unreviewed, reasoning that "further
iterations cannot make progress". That was WRONG and cost real time.

- **The blocked path was SHIP, not the loop.** MIGRATE and HARVEST - port
  tests, hit gaps, write them down - never needed a merge. That is the
  loop's engine, and it was available the whole time.
- **A correct constraint became a wrong conclusion.** "Do not open a sixth
  PR touching files the queue already modifies" was right. "Therefore do no
  work" did not follow.
- **I rationalised instead of measuring.** I wrote "the obvious gap surface
  is mapped". The actual numbers: the target has **180 tests** (127 UI
  across 7 files, 53 API across 9 files). I had ported **13**. Whole
  categories were untouched: notifications (25 tests, multi-user),
  transaction-feeds (34, date pickers), and 50 API tests.
- **Wrong tool for pacing.** `ScheduleWakeup(stop)` ends the loop; the
  bounded-increment rule only means "finish this slice and re-arm".

**Rule going forward: never stop while unported tests remain.** If SHIP is
blocked, migrate more. Only stop when the target is genuinely exhausted AND
the release path is clear, or when the user says so.

## MIGRATE round 3 (2026-07-22) - 15 specs, new gap classes

Two more specs, chosen for ground the first 13 never touched. Both record
and both pass twice:

| Spec | Probes | Result |
|---|---|---|
| 14-date-range-picker | calendar widget, cell addressing by aria-label | PASS, PASS (47.1s / 47.7s) |
| 15-notification-across-users | TWO identities in one flow: like as A, log out, log in as B, read the notification | PASS, PASS (108.2s / 108.0s) |

**GAP-P (P1): no clock control.** `cy.clock(startDate, ["Date"])` freezes
"now" so a date-dependent test is deterministic. flowproof has nothing.
Any test whose expectations depend on the current date (a "last 7 days"
filter, an expiry, a relative timestamp) cannot be made deterministic. This
is the single most common Cypress feature the port could not express.

**GAP-Q (P2, likely REJECT): computed navigation.** `pickDateRange` reads
the calendar's month label, computes a month delta, and clicks prev/next
that many times. That is control flow in a test, the same register as the
rejected `page.evaluate`. Recommend rejecting and documenting the answer:
pick a date in the visible month, or seed the data so the target month is
current. Needs a brain ruling before it goes in the backlog as rejected.

**Not a gap: mid-flow identity switch.** Cypress uses
`cy.switchUserByXstate(userB)`. flowproof walks the UI - logout, log in as
B - which is slower (~108 s for the flow) but is what a user does, and it
exercises logout too. Spec 15 proves the story works end to end.

### What is waiting### What is waiting

| PR | What | CI |
|---|---|---|
| [#74](https://github.com/automators-com/flowproof/pull/74) | 0.2.5 version bump (all four locations) | green |
| [#75](https://github.com/automators-com/flowproof/pull/75) | GAP-N: scroll before hit-testing | green |
| [#76](https://github.com/automators-com/flowproof/pull/76) | docs: rejected blur verb, dev-server watchers | green |
| [#77](https://github.com/automators-com/flowproof/pull/77) | GAP-G: env_from sees suite `env:` | green |
| [#78](https://github.com/automators-com/flowproof/pull/78) | GAP-D: `page url is\|contains` | green |

Verified to merge in ANY order (see BATCH INTEGRATION CHECK). #74 carries the
bump, so merging it LAST makes 0.2.5 ship the whole cycle.

### Exactly what to do on resume

1. Merge whatever is approved. If all five: 0.2.5 covers the cycle.
2. `gh workflow run publish.yml --repo automators-com/flowproof --ref main`,
   then verify `pip install flowproof==0.2.5` in a clean venv and that
   `--version`, `flowproof.__version__` and `_native.__engine_version__` all
   agree.
3. RE_VERIFY: `cd ~/Projects/flowproof-field/cypress-realworld-app/flowproof
   && <venv-025>/bin/flowproof run specs/`. **Expect 13 of 13.** Anything
   less is a REGRESSION and outranks all other work.
4. Then the gaps may finally be marked FIXED (the hard rule requires a
   released wheel), the retrospective below becomes final, and SOLID is
   reached.
5. SELECT_TARGET: `documenso/documenso` per the shortlist below.

### Built but NOT yet started (no blockers, fully specified)

- **GAP-E checkbox** - syntax fixed in verdict 4b (`Check`/`Uncheck the
  "<label>" checkbox`, `is checked`/`is not checked`, set-state semantics).
  Deliberately not built: it touches `driver/app.rs`, which #78 also
  modifies, so building it now would manufacture a conflict in a queue
  that currently has none.
- **GAP-O element count** - syntax fixed in verdict 5
  (`the "<target>" appears <N> times`). Build after GAP-E, per Fable's
  sequencing.

## RETROSPECTIVE - target 1, cypress-io/cypress-realworld-app

Written pre-SOLID: the suite is green and the P0s are fixed in code, but
SOLID is NOT reached, because nothing is released and the hard rule says a
gap is not fixed until it is re-verified against a released wheel. This
section is ready for the moment 0.2.5 lands.

**Result.** 13 specs ported from a real Cypress suite; 13 of 13 pass,
double-replayed, deterministic. Started at 3 of 8 with 5 driver errors and
one spec that could not even record.

**What the round actually taught, beyond the individual fixes:**

1. **The two P0s were both flowproof misdiagnosing itself.** A transport
   that killed its own connection after an idle, and a gate that hit-tested
   a position the click would never use. Neither was an app quirk, and
   neither was findable from unit tests - it took a real app with a login
   redirect and a form below the fold.
2. **Record/replay asymmetry is the highest-value bug class.** Every serious
   defect this round showed up as "records clean, cannot replay". That
   asymmetry should be a first-class thing to hunt: any code path where
   record and replay read the same thing differently (two matcher copies,
   different timing around a navigation) is a candidate.
3. **A workaround in a spec is a finding, not a fix.** Each time I wrote one
   (uppercase text for the case gap, text presence for the count gap), the
   spec kept passing while the product stayed broken. Logging the exact
   error before applying the workaround is what kept them visible.
4. **The engine's own diagnostics were usually right and occasionally
   misleading.** "element exists but is obscured" was precise about what it
   measured and wrong about what it meant. Worth auditing messages that
   assert a CAUSE rather than an observation.
5. **Slow is not stuck.** Cost ~35 min of dead CI waiting plus two false
   alarms from my own monitors before the rule was written down.

**What did not get exercised:** anything non-web. `app: sap`, `app: vision`
and UIA desktop remain untested by this loop because the host is macOS.
That is now the single biggest coverage hole in the field programme.

## NEXT TARGET SHORTLIST (for SELECT_TARGET after 0.2.5 lands)

Checked live, not from memory:

| Repo | Suite | Size | Why |
|---|---|---|---|
| **documenso/documenso** (recommended) | Playwright, `packages/app-tests/e2e` with document-flow, document-auth, envelope-editor-v2, auto-placing-fields, admin, api | 280 MB, AGPL-3.0, pushed 2026-07-22 | **Brand-new gap class**: drag-and-drop field placement on a PDF, signature CANVAS drawing, multi-signer flows with per-signer auth. flowproof has NO drag vocabulary at all, and canvas is invisible to text anchors. Lightest of the serious candidates; needs postgres. |
| nocodb/nocodb | Playwright | 1.4 GB, pushed 2026-07-21 | A grid/table UI, so it would produce the field evidence the PARKED table-cell addressing design (docs/design.md round 2) has been waiting for. Pick this when that design is next up; the 1.4 GB clone and heavy bring-up are the cost. |
| calcom/cal.com | Playwright | 1.1 GB, MIT | Booking flows, timezones, embeds/iframes. Iframe addressing is untested ground. Heavy monorepo + DB. |

Note all three need a real database, unlike RWA's JSON file - budget bring-up
time accordingly, and prefer whichever has a documented docker-compose.

## BATCH INTEGRATION CHECK (2026-07-22) - the five PRs compose

With everything blocked on review, the useful work was proving the queue is
coherent rather than opening a sixth PR. Merged all five branches into a
local `integration/all-open-prs` off main:

- **All five merge CLEAN**, in listed order, no conflicts.
- Three files are touched by TWO PRs each and git auto-merged them:
  `crates/flowproof-adapters/src/web.rs` (#75 scroll + #78 url),
  `docs/authoring.md` (#76 + #78), `docs/getting-started.md` (#76 + #77).
  Auto-merge is textual, so these were the ones worth checking semantically.
- On the combined tree at 0.2.5: `cargo fmt --check`, `clippy -D warnings`
  and `cargo test --workspace --all-features` ALL PASS, and the full web E2E
  suite is **18 passed / 0 failed** in 1075 s (it includes both new
  regression tests, the idle-then-navigate one and the below-the-fold one).

- **Field suite on the integrated build: 13 of 13 PASS**, exit 0, the whole
  suite green for the first time this cycle (`flowproof 0.2.5`). Includes
  `01-auth-redirect` in its one-to-one `page url is /signin` form, which
  only parses with #78 in the tree - so the run also proves the new
  vocabulary works end to end against a real app, not just the mock.

**Conclusion: the batch can be merged in any order.** No rework is hiding in
the queue, and the maintainer does not need to sequence it carefully. Note
#74 carries the 0.2.5 bump, so merging it LAST means 0.2.5 ships everything;
merging it first means the rest waits for 0.2.6.

To reproduce: `git checkout -B integration/all-open-prs origin/main` then
merge `origin/claude/flowproof-{gapn-scroll,docs-housekeeping,gapg-envfrom,gapd-url,v0.2.5}`.
The branch is local only, never pushed - it exists to verify, not to land.

## GAP-D: BUILT (2026-07-22) - `page url is|contains`

Implemented exactly per verdict 4a. `url_matches` lives in
**flowproof-driver** beside `numeric_value` so record and replay share ONE
matcher - two copies drift, and a drifted matcher is how this round produced
traces that recorded clean and could not replay. New trait method
`current_url` with a default that refuses by naming the reason (a window has
no URL). Trace keys `url_equals` / `url_contains`, additive in v1. Secrets
masked in both trace and failure message, with a test.

Field proof: `01-auth-redirect` is now a ONE-TO-ONE port of the Cypress
original (`cy.location("pathname").should("equal","/signin")`) and passes.
The old workaround asserted visible text, which passes even when the route
is wrong.

**Remaining vocabulary batch, in Fable's sequence:** GAP-E (checkbox, syntax
in verdict 4b), then GAP-O (element count, syntax in verdict 5). Both are
fully specified - no further escalation needed to build them.

## SUITE STATUS - 13 of 13 once #75 merges (2026-07-22)

Double-replayed, both rounds byte-identical. On a build of **main** it is
11 of 13: `04-auth-signup` and `05-user-settings-update` both fail with
`element exists but is obscured`. Rebuilt on the GAP-N branch, **both
pass** - 04 at 210 s (25 steps: signup, onboarding, GraphQL bank account,
logout) and 05 at 112 s.

So the only thing between the suite and 13 of 13 is merging #75. Worth
noting 04 failed on the Sign Up button of a five-field form, a completely
different element from the settings field that first surfaced GAP-N: the
below-the-fold gate was affecting a broad class of flows, not one page.

Cycle arc on this target:

| | 0.2.3 (start) | main | main + #75 |
|---|---|---|---|
| specs | 9 | 13 | 13 |
| passing | 3 of 8 | 11 of 13 | **13 of 13** |
| driver errors | 5 | 0 | 0 |
| blocked at record | 1 | 0 | 0 |

## MIGRATE round 2 (2026-07-22) - 4 more specs, 1 new gap

With the PR queue blocked on review, the useful work was widening the
exercise rather than stacking a fifth PR. Four more tests ported from
`transaction-view.spec.ts` and `transaction-feeds.spec.ts`, chosen to probe
ground the first nine did not: list-item clicks, typing plus Enter into a
comment box, element counts, visibility of a nav region, and a filter
drawer. Recorded against a build of main (GAP-A fixed, GAP-N/GAP-G not yet
merged).

| Spec | Probes | Record |
|---|---|---|
| 10-transaction-like | list-item click, like button, `is disabled` | ok, 8 steps |
| 11-transaction-comment | type + `Press Enter`, element counts | ok, 14 steps (with workaround) |
| 12-transaction-detail-tabs-hidden | `is visible` / `is not visible` on a region | ok, 8 steps |
| 13-amount-range-filter | filter drawer opens, slider present | ok, 7 steps |

**NEW GAP-O (P1): no ELEMENT-count assertion.**
`cy.getBySelLike("comments-list").children().should("have.length", 2)` has
no flowproof equivalent:

    error: cannot resolve step 'the "css:[data-test=comments-list]" has 2
    children': expected 'the "<label>" field contains <text>', ... 

`page shows X N times` counts TEXT occurrences, not elements, so it cannot
express "this list has exactly two rows". The workaround (assert each
comment's text is present) is strictly weaker - a duplicate or a stray row
still passes. `should("have.length", N)` is among the most common Cypress
assertions, so this is worth designing rather than working around. It is
NEW VOCABULARY, so it needs the brain before implementation; batch it with
GAP-D/GAP-E in the next escalation.

**Observation worth keeping, not a gap:** the Cypress amount-range test does
not touch the UI at all - it calls the MUI slider's React `onChange` through
`reactComponent().its("memoizedProps")`. That is not a UI test, and it is
deliberately unportable for the same reason `page.evaluate` was rejected.
Spec 13 is the honest UI-driven equivalent (open the filter, assert what a
user sees). Some Cypress suites contain tests that a UI-level tool
structurally cannot and should not reproduce; say so in FINDINGS rather than
counting them as gaps.

## GAP-G: FIXED (2026-07-22) - env_from ordering

Implemented per the DESIGN verdict 4e, three phases: resolve each `env:`
entry against the ambient process environment, pass the resolvable ones to
the env_from CHILD (`Command::env`, never `set_var`), then apply `env:` to
the process as before. `${VAR}` precedence in flows is unchanged and pinned
by a test.

One deviation from the verdict, deliberate: it said "let stderr inherit".
Doing exactly that broke the existing fail-closed contract - the error
message degraded to "see above", which is useless to `--json`. TEEING
satisfies both intents (audible on success, still in the message on
failure). Recorded here because it is a knowing departure, not an oversight.

## Next iteration - start here

1. If PRs merged: publish 0.2.5 (`gh workflow run publish.yml`), verify
   `pip install flowproof==0.2.5` in a clean venv, then RE_VERIFY the whole
   exercise suite against that wheel. Expect **8 of 8**; anything less is a
   REGRESSION and outranks everything.
2. If still unmerged: do NOT stack more engine work on unmerged branches.
   Take Fable's iteration-2 list on items that do not touch web.rs -
   GAP-G (`env_from` cannot see suite `env:`, exact three-phase mechanism in
   the verdict below) is CLI-only and conflict-free. GAP-C
   (error artifacts + the `--json` always-one-object invariant) is next.
3. After that: GAP-D url assertion and GAP-E checkbox, both new vocabulary
   with syntax already fixed by the verdict - docs rows plus CI-parsed
   examples required.

## GAP-N: ROOT-CAUSED AND FIXED (2026-07-22)

`05-user-settings-update` failed 2/2 with `element exists but is obscured
(another element would receive the click) after 5000ms`, while Cypress types
into that same field happily.

**It was not obscured.** A CDP probe showed the viewport at 756x413 and the
field at y=431 - BELOW THE FOLD. `elementFromPoint` outside the viewport
returns null, and the gate reads null as "something else would receive the
click". After `scrollIntoView` the field moved to y=190 and the hit test
returned the field itself (`same: true`).

**The gate was wrong, not the action.** headless_chrome's `Element::click`
already starts with `scroll_into_view`, so the gate was hit-testing a
position the click would never use, and blocking a click that would have
worked. Cypress and Playwright both scroll before acting.

**Fix**: `element_receives_events` scrolls into view before hit-testing.
Verified with a negative control - reverting the fix makes the new E2E test
fail with the exact field error message, restoring it passes. The real field
flow now passes end to end (112 s, 19 steps).

**Field suite is now 8 of 8** (04 records since the GAP-A fix, 05 passes
since this one). Confirm on the released wheel at RE_VERIFY before calling
either gap fixed.

## GAP-A: ROOT CAUSE FOUND AND FIXED (2026-07-21, iteration 4)

Found by running flowproof itself under headless_chrome's transport logging
(scratchpad crate `fpprobe` depends on flowproof-cli by path and just calls
`env_logger::init()` before `run_cli` - no product code touched):

```
ERROR headless_chrome::browser] Got a timeout while listening for browser events
WARN  transport] Couldn't send browser an event: "TargetInfoChanged(... url: /)"
INFO  transport] Shutting down message handling loop
```

**Mechanism.** headless_chrome reaps its browser-event listener thread after
`idle_browser_timeout` (default 30s; flowproof never set it). A flow that
spends 30+ seconds on page-level work emits NO browser-level events, so the
thread exits. The next navigation fires `TargetInfoChanged` - a browser-level
event that can no longer be delivered - and the transport treats that as
fatal, shutting the whole message loop down permanently.

A login redirect is exactly that shape. This explains every observation:
authenticated flows died, unauthenticated ones never did, record survived
more often than replay (different timing around the 30s boundary), and no
synthetic repro worked because every synthetic flow finished inside 30s.

**Fix**: raise `idle_browser_timeout`. NOT re-attach -
Fable's part 2 is now moot, because nothing was actually broken to re-attach
to. Silence is not evidence of a dead browser; a browser that truly dies
closes the socket, which surfaces immediately through a different path.
Recorded here so the held decision is not revisited from scratch.

### INCIDENT: the first version of this fix hung CI for 3h15m

`idle_browser_timeout` was set to 24 hours. It is **overloaded** in
headless_chrome 1.0.22 across three jobs with opposite needs:

| use | wants | at 24h |
|---|---|---|
| `browser/mod.rs:406` event-listener reap | LONG | fixed GAP-A |
| `transport/mod.rs:252` transport idle reap | LONG | fixed |
| `transport/mod.rs:179` wait for a call's RESPONSE | SHORT | **a lost response hangs for 24h** |

The `web E2E (ubuntu)` job sat on one test for 3h15m before I cancelled it.
A hanging run is strictly worse than a failing one: it burns runner time and
reports nothing. Corrected to **300 s** - comfortably past any real gap
between browser-level events (field flows idled 30-90 s, `Wait until`
defaults to a 60 s bound) and short enough that a lost response fails
visibly.

**Process lesson, apply every time:** an E2E-affecting change must be run
through the E2E suite LOCALLY before pushing:
`FLOWPROOF_E2E=1 cargo test -p flowproof-cli --test web_e2e -- --nocapture
--test-threads=1`. `cargo test --workspace` does NOT cover it - every web
E2E test is gated behind `FLOWPROOF_E2E=1` and silently returns otherwise,
so a green workspace run says nothing about browser behavior. That gap is
exactly what let a hang reach CI.

**Watch for hangs, not just failures.** The fail-fast CI watcher keys on
`bucket=="fail"`, and a hung job is neither fail nor complete - it stays
`pending` forever, so the watcher waits with it. Add a wall-clock ceiling
to CI watches: if a run exceeds the known job budget (web E2E ~32 min, the
whole CI ~35 min), treat it as a failure, cancel it, and investigate.

**Effect on the field suite** (same specs, same app):

| | released 0.2.4 | with the fix |
|---|---|---|
| flows passing | 3 of 8 | **7 of 8** |
| driver errors | 5 | **0** |
| spec 04 (blocked at record 3/3) | blocked | **records, 25 steps** |

**New finding, GAP-N (P1, real failure not a fault):** `05-user-settings-update`
now fails 2/2 at `Replace the "First Name" field` with
`element exists but is obscured (another element would receive the click)
after 5000ms`. The actionability gate is doing its job and the message is
good; the question is whether it is RIGHT - Cypress types into that same
field happily. Debug bundle captured (`dom.html` shows the input present,
24 MuiDrawer nodes, no backdrop). Next step: use CDP `elementFromPoint` at
the field centre to see what actually receives the click, then decide
whether this is an over-strict gate or a genuine overlay.

## RE_VERIFY results## RE_VERIFY results - 2026-07-21, against the RELEASED 0.2.4 wheel

Empirical, not from release notes. Suite re-run in full plus a targeted
synthetic check for the matcher change:

| Gap | Verdict | Evidence |
|---|---|---|
| GAP-B suite abort | **FIXED** | all 9 flows ran (was 2 of 9); merged junit written with `tests="19" failures="0" errors="5" skipped="1"` (was: no junit at all) |
| GAP-J UIA name leak | **FIXED** | every fault now reads `driver transport fault: reading page text: ...` |
| GAP-F case sensitivity | **FIXED** | `page shows Friends` passes against a tab rendered "FRIENDS"; the workaround is removed from `08-transaction-feeds` and it re-records clean |
| GAP-F compat (negative form) | **HELD** | `page does not show friends` still passes on the same page - the fallback is widening-only, so no previously-passing trace flipped |
| non-ASCII through the matcher | **OK** | `page shows Überweisung` passes on a text-transformed element |
| GAP-A transport death | **STILL OPEN (P0)** | the 5 authenticated flows still error identically; part 1 tolerates faults but the transport death is PERMANENT, so the budget expires with zero readings |

Net: 3 of 8 recorded flows pass, 5 error, 1 skipped - the same pass count as
0.2.3, but the failures are now *visible to CI* instead of hiding behind an
aborted run. That is the honest read: the reporting is fixed, the blocker is not.

## RELEASE RUNBOOK - there are FOUR version locations, not three

The loop's hard rule names three. Bumping only those ships a wheel whose
`flowproof.__version__` lies while `_native.__engine_version__` is right:

1. root `Cargo.toml`
2. `sdk/python/pyproject.toml`
3. `Cargo.lock` (via `cargo update -p <each flowproof crate>`)
4. **`sdk/python/flowproof/__init__.py`** - the one the runbook forgets

`sdk/python/tests/test_api.py::test_every_version_location_agrees` now pins
them together, so this cannot silently recur.

Publish flow: merge to main -> `gh workflow run publish.yml` (workflow_dispatch,
`Publish to PyPI`; it fails fast if the version already exists on PyPI) ->
verify `pip install flowproof==<new>` in a clean venv and `--version` matches.

### Why this target

Two prior rounds were both Playwright (DataMaker Next.js monorepo -> PRs #43-#46;
actualbudget/actual -> the round-2 P2 notes in `docs/design.md`). This picks
**Cypress** for framework variety: 16 e2e specs (7 UI + 9 API), self-contained
React + Express with a JSON-file DB, no Docker, one-command bring-up.

## Host constraints (standing - do not re-litigate each iteration)

- Host is **macOS** (darwin 25.5.0, arm64). `app: sap`, `app: vision`, and UIA
  desktop (`calc`/`notepad`) all need Windows (SendInput, UIA, SAP GUI Scripting
  COM). Only `web` and `api` flows are recordable here. The loop's
  "variety across surfaces" goal is blocked on a **Windows runner**, not on
  target selection. Revisit if a Windows VM/runner becomes available.
- Available: Chrome, cargo/rustc, uv, node (nvm; v22.23.1 installed for this
  target - the repo demands `^22 || ^24`), yarn 1.22.22 via corepack, gh.

## Target bring-up recipe (repeat verbatim next iteration)

The dev server was left RUNNING (log: `/tmp/rwa-dev.log`). Check
`curl -sf localhost:3000` before starting another - a second `yarn dev` just
fails with EADDRINUSE. The exercise branch is at `b215358`
(`flowproof-exercise`, no upstream configured, never pushed).

```bash
source ~/.nvm/nvm.sh && nvm use 22 && corepack enable
cd ~/Projects/flowproof-field/cypress-realworld-app
yarn install --frozen-lockfile      # once; already done
nohup yarn dev > /tmp/rwa-dev.log 2>&1 &    # vite :3000, express API :3001
curl -fsS -X POST http://localhost:3001/testData/seed   # = cy.task("db:seed")
cd flowproof && ~/Projects/flowproof-field/venv-024/.venv/bin/flowproof run specs/
```

Seeded users are deterministic: `Heath93` (Ted Parisian, users[0]),
`Arvilla_Hegmann` (Kristian Bradtke, users[1]); password `s3cret`.
One local edit to the target: `vite.config.ts` `server.watch.ignored` for
`data/`, `flowproof/`, `.flowproof/` (flowproof writes run bundles inside the
repo, and vite's watcher full-reloads the app under test when it sees them -
the reload is visible in the vite log). Not the cause of GAP-A; keep it anyway.

## Migration result (2026-07-21)

9 specs ported from `cypress/tests/`. Recorded with `--author rules`
deliberately, so every unparseable idiom surfaces as an error instead of being
silently papered over by the LLM author. Replayed twice, per spec, with a
reseed before each run. **Both rounds identical** - the failures are
deterministic, not flaky:

| Spec | Ported from | Record | Replay x2 |
|---|---|---|---|
| 01-auth-redirect | ui/auth.spec.ts | ok | PASS, PASS |
| 02-auth-login | ui/auth.spec.ts | ok | FAIL, FAIL (GAP-A) |
| 03-auth-login-errors | ui/auth.spec.ts | ok | PASS, PASS (45.7s / 45.6s) |
| 04-auth-signup | ui/auth.spec.ts | **BLOCKED** (GAP-A at record, 3/3) | - |
| 05-user-settings-update | ui/user-settings.spec.ts | ok | FAIL, FAIL (GAP-A) |
| 06-new-transaction | ui/new-transaction.spec.ts | ok | FAIL, FAIL (GAP-A) |
| 07-bankaccounts-create | ui/bankaccounts.spec.ts | ok | FAIL, FAIL (GAP-A) |
| 08-transaction-feeds | ui/transaction-feeds.spec.ts | ok | FAIL, FAIL (GAP-A) |
| 09-api-users | api/api-users.spec.ts | ok | PASS, PASS (6 ms) |

**Every flow that logs in fails at replay while its record passed.** The three
that pass never authenticate. That is the headline for HARVEST.

What worked well and should be said so in FINDINGS: `${VAR}` secrets never
entering the trace (the captured frame shows the password field blacked out),
`suite.yaml` `before_each` as a clean `cy.task("db:seed")` replacement,
`assert_api` with `headers:` for the api-flow port, `Replace the … field`,
`the "…" is disabled`, `css:` escape hatches, and error messages that print the
whole surface text when an assertion misses.

## Unrelated backlog (noticed, not scheduled)

- GitHub reports **5 dependabot vulnerabilities on the default branch**
  (2 high, 3 moderate), surfaced on every push. Not caused by this cycle and
  not in the field-hardening scope, but it is security-relevant and someone
  should triage it. Raise with the maintainer rather than silently fixing:
  dependency bumps can move engine behavior, which this loop then has to
  re-verify.

## Gap backlog

Ranked. Evidence is empirical: every entry below was hit during this migration
against the released 0.2.3 wheel. No panics were observed at any point.

### P0

- **GAP-A - a CDP transport fault during a page-level read aborts the flow, and
  record/replay disagree.** Verbatim, every time:
  `error: driver error: UIA query failed: web: reading page text: Unable to
  make method calls because underlying connection is closed`
  Reproduction: 5 of 8 specs, 2/2 rounds, plus 3/3 at record for 04. Fails
  immediately after the app's post-login full-document navigation
  (`window.location.pathname = "/"` in `src/machines/authMachine.ts:244`).
  The last captured frame shows the filled signin form, so the death is at the
  press/navigate boundary. Recording survives it (slower, so the read lands
  after the navigation settles); replay does not. Source observation:
  `crates/flowproof-adapters/src/web.rs:296` `with_element` retries once on
  `is_transport_fault`, but `surface_text` (`web.rs:670`) calls `evaluate`
  with no such retry - every `page shows` / `Wait until` read is unprotected.
  NOT reproducible with a synthetic navigation page (4 variants, delays
  0/200/800/1500 ms, http and file://, all PASS) - the exact trigger still
  needs a from-source instrumented run, which belongs to DESIGN/IMPLEMENT.
- ~~**GAP-B - one driver error kills the whole suite run.**~~ **FIXED in
  0.2.4, re-verified against the released wheel.** Original evidence: `flowproof run specs/`
  passed flow 1, hit GAP-A on flow 2, and stopped: the remaining 6 flows never
  ran and no merged junit was written. `docs/getting-started.md` promises "a
  failing flow doesn't stop the rest". Holds with and without
  `FLOWPROOF_NO_SHARED_BROWSER=1`.

### P1

- **GAP-C - driver errors produce no machine-readable artifact.** The failing
  run left `.flowproof/runs/<ts>/recording/` with 7 frames and **no
  `result.json`, no `report.html`, no `junit.xml`**. `run --json` printed
  **zero bytes** to stdout and exited 2. flowproof's primary surface is
  programmatic (MCP/agents), so a driver fault is invisible to the caller.
- **GAP-D - no URL / location assertion.** `cy.location("pathname").should(
  "equal", "/signin")` is the single most common Cypress assertion in this
  suite. Error: `cannot resolve step 'page url is /signin': expected '[the ]page
  shows <text>' …`. Workaround used: assert on visible text, which is a weaker
  proxy (a redirect that lands on the right text but the wrong route passes).
- **GAP-E - no checkbox vocabulary.** `Check the "Remember me" checkbox` ->
  `cannot resolve step`. `Click "Remember me"` does toggle it (verified in the
  frame), but there is no `is checked` assertion, so the state cannot be
  asserted at all - `should("be.checked")` has no equivalent.
- ~~**GAP-F - `page shows` is case-sensitive over rendered innerText.**~~
  **FIXED in 0.2.4** (#65 plus the widening-only correction in #72).
  `assert: page shows Friends` FAILS on a MUI tab whose DOM text is "Friends"
  and whose rendered text is "FRIENDS" (`text-transform: uppercase`). Cypress's
  `should("contain","Friends")` passes on the same DOM. Element anchors already
  have an ASCII case-insensitive rung (`Click "Friends"` resolves); the surface
  assertion does not. A silent portability trap for every Bootstrap/MUI app.
- **GAP-G - `env_from` cannot see the suite's own `env:`.** The data command
  runs before `env:` is applied, so a mint script that needs `RWA_API` /
  `RWA_PASSWORD` gets empty values and fails closed downstream (401). Proven by
  hardcoding shell defaults, after which the flow passed. Minting test data
  almost always needs the suite's base URL and credentials. The command's
  stderr is also swallowed, so diagnosing it needs a side channel.

### P2

- ~~**GAP-H - no `Blur`/focus-out step.**~~ **REJECTED by DESIGN 4c** - the
  grammar describes what a user does, and `Press Tab` is that. Owes one
  sentence in docs/authoring.md, then drop. `Blur the "Username" field` ->
  `cannot resolve step`. `Press Tab` works as a workaround (validation fired),
  but blur-triggered validation is a very common form idiom.
- **GAP-I - non-semantic clickable rows are not text-addressable.**
  `Click "Kristian Bradtke"` -> `element for step … not found:
  [name=Kristian Bradtke]`, although `page shows Kristian Bradtke` passes: the
  MUI `ListItem` has an `onClick` but no role. Cypress:
  `getBySelLike("user-list-item").contains(name).click()`. Workaround: `css:`.
  Note the record-time error offers no "did you mean" suggestion.
- ~~**GAP-J - web flows report UIA errors.**~~ **FIXED in 0.2.4.** Every web driver fault is prefixed
  `UIA query failed:` - the Windows adapter's name leaking into a browser run.
- **GAP-K - misleading error when a relative `url:` does not exist.**
  `error: cannot write trace examples/web/greeter.html: No such file or
  directory` - names the URL as the trace, and blames writing rather than
  resolving.
- **GAP-L - no shared authenticated setup.** No equivalent of
  `cy.loginByXstate` / a Playwright setup project: every UI flow re-walks the
  login form (4 steps, ~10 s each). `session:` needs a cookie value you must
  mint yourself, and `env_from` (GAP-G) is the only place to mint it.
- **GAP-M - computed assertions.** Already recorded in `docs/design.md` round 2;
  hit again here (`new-transaction` asserts the balance dropped by the amount).
  Confirms the earlier finding rather than adding to it.
- **Perf note** - `03-auth-login-errors` takes ~45.7 s for 8 steps (both rounds),
  dominated by two `Press Tab` steps and 3 assertions. Not a gap yet, but the
  Cypress equivalent runs in ~3 s.
- **Hygiene note** - failed records leave orphaned Chrome processes
  (`--remote-debugging-port`) behind.

## Cross-cycle drift caught (2026-07-21) - keep checking for this

Another change (#65, `0c4127c`) landed on main DURING this cycle, fixing a
multibyte parse panic (found by a separate round-3 verification report) and
giving `page shows` the case-insensitive fallback of GAP-F. Two consequences
the loop had to handle rather than assume:

- **GAP-F is effectively addressed** by #65, but its NEGATIVE-form and count
  behavior CONTRADICTED the Fable ruling 4d recorded in this file: it
  mirrored the fallback onto `page does not show`, which makes a recorded
  trace that used to pass start failing. Fixed in PR #72 per the ruling
  (widening-only; negative stays case-sensitive; nonzero case-sensitive
  count wins). No escalation needed - the verdict already covered the case.
- **The matcher had drifted into two copies**: `text_matches` (replay) and
  `assert_holds` (record). They disagreed. That is precisely how a trace
  gets minted that cannot replay. Now aligned and commented.

Lesson for future iterations: always `git log main` for changes that landed
mid-cycle before bumping or re-verifying, and check them against the
recorded design verdict rather than trusting the commit title.

## IMPLEMENT progress (2026-07-21, iteration 2)

Done and pushed (PR #71):
- **GAP-A part 1** - `DriverError::Transport` + `is_transient()`; `web_err`
  classifies transport faults; the four poll loops in `check_assertion` and
  `check_text_expectation` tolerate them inside the recorded wait budget. A
  budget that expires with ZERO successful readings reports the transport
  fault verbatim instead of a fabricated "expected X, got ''".
- **GAP-B** - per-flow error isolation in `run_suite` (spec parse, missing
  trace, hooks, driver faults, bundle write), the `run_hook(...)?` bug, the
  new `StepStatus::Errored` outcome, junit `<error>` + `errors="N"`, and exit
  2 outranking 1.
- **GAP-J** - web faults no longer render as `UIA query failed`.

Empirical check against the live target with the source-built binary:
- Before: suite stopped after flow 2, no junit at all.
- After: **all 9 flows ran**; `3 passed, 1 skipped, 5 errored`; merged junit
  written with `tests="19" failures="0" errors="5" skipped="1"`.
- Error text now reads `driver transport fault: reading page text: ...`.

### New evidence for GAP-A part 2 (answers Fable's scoping question)

Part 1 is in and behaves exactly as ruled, but it does NOT rescue the five
authenticated flows, and the reason is decisive: **the transport death is
permanent, not transient.** Once headless_chrome's transport loop breaks it
sets `open = false`, so every later call returns `ConnectionClosed`
instantly - tolerating polls just burns the wait budget and then reports the
fault. Supporting findings from the instrumentation (~1h spent, Fable
timeboxed 2h):

- Chrome itself STAYS ALIVE (verified: process alive, only a benign
  `chrome://newtab` profile warning on its stderr). So the browser process is
  not the casualty.
- `headless_chrome` 1.0.22 closes the transport in exactly three places
  (`browser/transport/mod.rs`): an idle timeout of **30 s with no incoming
  message** (`LaunchOptions` default, which flowproof never overrides), a
  browser-listener channel send failure, and a lost response channel. The
  ~30 s stall observed before every failure matches the idle timeout.
- A standalone probe using the SAME library against the SAME app - navigate,
  type, click submit, `capture_screenshot(from_surface=true)`, then poll
  `evaluate` 80 times through the post-login navigation - **passes every
  time**. So plain CDP through this navigation is fine; something flowproof
  does around a step boundary is not. Probe kept at
  `<scratchpad>/cdpprobe` (session-scoped; rebuild from STATE if gone).
- Therefore the fault scope is most likely Fable's case **(b) browser
  websocket dead** (client side) while the browser lives - which its ruling
  says must NOT be papered over by re-attach. The remaining question for the
  next slice is what silences the socket for 30 s. Prime suspect: a CDP call
  issued mid-navigation whose response is never delivered, after which the
  idle timeout fires. Setting `idle_browser_timeout` explicitly is a
  candidate mitigation but must not be shipped as a "fix" before the cause
  is understood.

Next slice (IMPLEMENT, iteration 3): finish the GAP-A root cause with the
remaining timebox, then Fable's iteration-2 list (GAP-C, then GAP-D/E/F/G
with docs + CI-parsed examples).

## Host quirk (not a product bug)

`cargo build/test --workspace` fails to LINK `flowproof-python` on macOS:
PyO3 `extension-module` needs `-undefined dynamic_lookup`, which CI does not
need because it runs Linux/Windows. Workaround used for every local check:
`RUSTFLAGS="-C link-arg=-Wl,-undefined,dynamic_lookup" cargo test --workspace
--all-features`.

## Next iteration - start here

1. **GAP-A root cause** (~1h of Fable's 2h timebox left). Known so far: the
   transport death is permanent; Chrome survives; a standalone probe with the
   same library through the same navigation does NOT reproduce; headless_chrome
   closes the transport on a 30s idle timeout, a listener-channel send failure,
   or a lost response channel. Next probe should mimic flowproof's step
   boundary more exactly (isolated browser CONTEXT rather than a plain tab,
   the actionability gates, the per-step frame capture) until it reproduces.
   Only then choose between re-attach (Fable's case a) and erroring honestly
   (cases b and c).
2. Then Fable's iteration-2 list: GAP-C (error artifacts + the `--json`
   always-one-object invariant), then GAP-D url assertion, GAP-E checkbox,
   GAP-G env_from ordering - each with docs/authoring.md rows and CI-parsed
   examples.
3. Housekeeping owed: the docs line for the rejected blur verb, and the
   "dev-server watchers" note in getting-started.

## Iteration log

### 2026-07-21 - iteration 1

- Bootstrapped this file. No prior loop state existed anywhere; earlier field
  rounds (`claude/flowproof-field-{a,b,c,d}`) were driven ad hoc.
- SELECT_TARGET: chose cypress-realworld-app, cloned, branched
  `flowproof-exercise`, installed node 22 + yarn, brought the app up on
  :3000/:3001, installed released `flowproof==0.2.3` in a clean venv, and
  smoke-tested it (examples/web.flow.yaml record+run PASS; a 3-step flow
  against the target's signin page PASS).
- MIGRATE: ported 9 specs, recorded 8 (1 blocked), double-replayed all 8 with a
  reseed before each run. Results identical across rounds. Backlog above.
- Next iteration: **HARVEST** - write `FINDINGS.md` on the exercise branch from
  the table and backlog above, then DESIGN (escalate to Fable with the ranked
  backlog; GAP-A/GAP-B are the release-blocking pair, and GAP-A needs a
  from-source instrumented run to pin the exact CDP trigger before any fix).

### 2026-07-21 - iteration 2

- HARVEST: wrote `flowproof/FINDINGS.md` on the exercise branch (commit
  `94f3efb`) - the migration table, every gap with the Cypress idiom that has
  no flowproof equivalent, the determinism results, and an honest
  could-they-adopt verdict (no for the browser tier, yes for `app: api`).
- DESIGN: escalated the ranked backlog to Fable in one brief. Verdict recorded
  verbatim at the end of this file and is binding. Headlines: transport faults
  are poll misses (ship now), re-attach held pending root cause, suite
  isolation with an `errored` outcome, URL assertion and checkbox verbs
  approved with exact syntax, case-insensitive `page shows` as a
  widening-only fallback, `env_from` ordering is a bug fix, and five items
  explicitly REJECTED (blur verb, network-traffic observation, shared-login
  machinery, artifact relocation, row addressing as new vocabulary).
- IMPLEMENT slice 1: PR #71, green locally, verified against the live target.
- Next: finish the GAP-A root cause, then Fable's iteration-2 list.

### 2026-07-21 - iteration 3 (SHIP + RE_VERIFY)

- Caught #65 landing mid-cycle and contradicting DESIGN 4d; fixed the
  negative-form and count widening, and aligned the record/replay matcher
  copies that had drifted apart (PR #72).
- Found the FOURTH version location and pinned all of them with a test.
- Shipped 0.2.4: merged, dispatched `publish.yml` (all 4 jobs green),
  verified `pip install flowproof==0.2.4` in a clean venv with matching
  `--version` and engine version.
- RE_VERIFY against the released wheel: GAP-B, GAP-J, GAP-F confirmed fixed;
  GAP-A confirmed still open. Table above.
- Process fix: CI watching now wakes on the FIRST failure instead of waiting
  for the slowest job (cost ~35 min once).

## DESIGN verdict - 2026-07-21, Fable (recorded verbatim, follow it)

Code fact Fable flagged that reshapes GAP-A: the poll loop already exists.
`check_text_expectation` (crates/flowproof-replay/src/lib.rs:397-412) polls every
200ms until the recorded `timeout_ms` deadline, but line 398 is
`let text = read()?;` - a single `DriverError` from `surface_text` propagates
instantly out of the loop, out of `run_trace`, and up to `run_cli` as exit 2.
The same unprotected `?` sits in the `resolve` closure (line 437) and
`element_enabled` (line 487). GAP-A, GAP-B and GAP-C are all downstream of that
one propagation path. Also: `run_suite` line 578 calls `run_hook(...)?`, which
aborts the whole suite, contradicting its own comment "a failing hook fails the
flow, not the run" - a latent bug to fix inside GAP-B.

# DESIGN DECISIONS - field-hardening round 3 (cypress-realworld-app)

## 1. GAP-A: retry semantics for page-level reads

**VERDICT: three-part ruling. Part 1 ships now, before root cause. Part 2
(re-attach) is held until the instrumented run names the fault scope.
Determinism question: answered, re-attach to the same target is legitimate.**

**Part 1, ship now: a transport fault during an assertion poll is a poll miss,
not an error.** Every auto-waiting assertion already runs its reader inside a
poll loop bounded by the trace-recorded `timeout_ms` (`check_text_expectation`,
and the `resolve`/`element_enabled` loops in `check_assertion`). The assertion's
declared semantics are "this holds within N seconds". A CDP fault during one
poll iteration is an observation about the harness, not about the app, so it
must not terminate the loop. Rule: inside any assertion poll loop, a
`DriverError` matching `is_transport_fault` is treated as a miss and polling
continues; any other `DriverError` still propagates. If the deadline expires and
at least one read succeeded, report the normal assertion failure. If the
deadline expires and NO read ever succeeded, report the step as a driver error
carrying the transport message. Never convert a pure infra fault into a fake
"expected text X, got ''" message; that would send an agent healing a trace that
is not broken. This change is safe regardless of root cause and needs no
re-attach machinery: the retry budget is the flow's own declared patience,
already in the trace.

**Part 2, held: bounded re-attach.** Do not build it until the instrumented
from-source run answers one question: when the fault fires, is (a) the CDP
session dead but the target (tab) alive, (b) the browser websocket dead, or
(c) the target itself gone? The correct recovery differs per answer. If (a),
re-attach to the same target id and continue, bounded at 2 attempts inside the
current step's wait budget. If (b) or (c), there is nothing faithful to
re-attach to: relaunching the browser loses page state (the login you just
performed), so the flow must error. Building re-attach before knowing which case
you are in risks building the wrong one and masking real tab crashes. Timebox
the instrumentation to ~2 hours; the reproduction is deterministic (5 specs,
2/2 rounds), so it will yield.

**Determinism ruling, quotable:** re-attaching to the SAME target does not make
a replay non-equivalent to its record. The trace's equivalence contract is the
step sequence, the selector ladders, the sync conditions, and the recorded
timeouts. It says nothing about CDP transport continuity, and record itself
already survives this exact navigation: that is why record passes, its reads
happen to land after the swap settles. Replay re-attaching restores by design
the parity that record has by accident of timing. Conditions that keep it
legitimate: same target id (or the flow's sole tab in the same browser context),
within the current step's recorded wait budget, bounded attempts, and surfaced
in the run report as a step-level informational note. Do NOT reuse the
`degraded` flag for this: degraded means selector drift and must keep meaning
exactly that. Silently continuing into a DIFFERENT target (a popup, a fresh tab)
is forbidden.

Keep `with_element`'s one-shot retry as is for now; once the root cause is
pinned, unify element ops and page reads under one recovery policy so the
asymmetry the field report caught cannot recur.

## 2. GAP-B: suite isolation

**VERDICT: driver errors are per-flow-fatal and suite-survivable. The abort
boundary is the per-flow loop.**

Precise boundary for implementation:

- **Abort the whole run** only for faults before the per-flow loop begins or
  that invalidate the suite definition itself: spec discovery failure,
  unreadable or invalid `suite.yaml`, `min_version` violation, `env_from`
  failure, junit directory unwritable.
- **Fail the flow, continue the suite** for everything inside one flow's
  iteration: the spec's own parse error (today `FlowSpec::load` at run_suite
  line 523 aborts the run; make it per-flow), `before_each`/`after_each` hook
  failure (fix the `run_hook(...)?` bug at lines 578/586, which contradicts its
  own comment), driver construction failure, trace load failure, and any replay
  `ReplayError::Driver`.
- **No special "browser cannot launch" class.** If Chrome cannot launch at all,
  every flow errors identically. That is noisy but truthful, trivially
  predictable, and matches pytest/Playwright behavior. Any heuristic split
  ("this fault looks environmental, stop everything") is a boundary that rots.
  If the noise proves painful in the field, the right shape is an opt-in
  `--fail-fast` flag (stop after first error); do not build it yet.
- **Errored is a third flow outcome**, distinct from failed: junit `<error>`
  element, `[ERROR]` in console output.
- **Exit codes:** any flow errored -> 2; else any flow failed -> 1; else 0. This
  preserves the documented contract that 2 means "look at the engine or
  environment, not the app", while the suite still runs to completion with a
  full merged junit.
- **`--retries` applies to errored attempts too.** An errored attempt is
  strictly worse than a failed one; if a retry passes, the flow passes, with the
  attempt count in the report.

## 3. GAP-C: failure reporting

**VERDICT: yes to all of it. A driver error is a run outcome, not the absence of
one.**

- **Any run that started writes the full bundle**: `result.json`, `junit.xml`,
  `report.html`. `result.json` keeps `passed: false` and gains a run-level
  `"error": "<message>"`; the erroring step gets step status `errored` with the
  message as detail; unreached steps stay `skipped`. junit maps errored to
  `<error>`. These are additive changes to result.json and junit; the trace
  format is untouched, so no trace version bump.
- **The partial `recording/` dir must be referenced from `result.json`** so an
  agent can find the frames without globbing.
- **The `--json` contract, stated as an invariant: with `--json`, stdout always
  carries exactly one JSON object, on every exit path including exit 2.** Zero
  bytes on stdout with a nonzero exit is a contract violation, full stop. Error
  shape: `{"error": "<message>", "report": <report-or-null>, "report_path":
  <path-or-null>}`. Suite shape: errored flows appear in `flows` with their
  reports; a top-level `"error"` appears only for run-aborting faults from
  decision 2. stderr remains human prose. `record --json` already does this for
  clarifications (`record_failure_json`); this generalizes that precedent, it
  does not invent a new one.
- Errors with no run to report (bad args, missing trace, spec parse on a single
  run) still emit the JSON error object; they just have null report fields.

## 4. New vocabulary

### 4a. URL assertion: YES

Accepted forms (add to the assertions table in docs/authoring.md, with
CI-parsed examples):

- `[the ]page url is <expected>`
- `[the ]page url contains <text>`

Semantics:

- Both **auto-wait** like every assertion (default 10s, `within <N>s` accepted).
  Mandatory, not optional: SPA redirects land asynchronously, and a non-waiting
  URL assert would be the grammar's only racy assertion.
- `contains`: substring over the full current URL (href).
- `is`: if `<expected>` starts with `/`, it is compared against the pathname
  exactly; the query is included in the comparison only when `<expected>`
  contains `?`, the hash only when it contains `#`. If `<expected>` contains
  `://`, the whole URL must match exactly. This makes `page url is /signin` map
  `cy.location("pathname").should("equal", "/signin")` one-to-one, without
  growing separate pathname/search/hash sub-grammars.
- No normalization beyond that. Exact means exact.
- Expected values may carry `${VAR}` refs, stored raw in the trace like every
  other text param.
- **Web flows only.** The rules resolver rejects it for other apps with an error
  naming the reason (a UIA window has no URL).

Trace serialization: surface-scoped `element_state` with new expect keys
`url_equals` / `url_contains`, reusing the existing poll plumbing. Additive
within trace v1: old traces are unaffected; new traces using it require the new
engine, which is the normal forward-compat rule.

Docs examples to add: `assert: page url is /signin` and
`assert: page url contains checkout`.

### 4b. Checkbox: YES, verbs AND assertions, with set-state semantics

Accepted forms:

- `Check the [2nd ]"<label>" checkbox`
- `Uncheck the [2nd ]"<label>" checkbox`
- `the [2nd ]"<label>" checkbox is checked` / `is not checked`

Why a verb rather than Click plus an assertion: Click encodes a **transition**
(toggle), so its meaning depends on the state it finds, and a reseeded or
drifted environment silently inverts it. Check/Uncheck declare the **target
state** and are idempotent: Check on an already-checked box is a no-op. That is
the deterministic, reseed-proof form, and it is why Cypress's `.check()` has the
same semantics.

Execution semantics: resolve via the existing label ladder to the checkbox
control (`input[type=checkbox]`, `input[type=radio]`, `[role=checkbox]`,
`[role=switch]`, including the MUI pattern of a visually hidden input inside a
styled label); if the current state differs from the target, perform a real
click so the events a user would fire, fire; then verify the state took, and
fail the step if it did not. The assertion reads the `checked` property, or
`aria-checked` for role-based widgets, and auto-waits like everything else.

Trace: new `action.type` `set_checked` with params `{"checked": true|false}`;
the assertion is `element_state` with expect `{"checked": true|false}`.
Additive, same forward-compat rule as 4a.

### 4c. Blur: NO. Rejected.

Reasoning, in the same register as the page.evaluate rejection: the grammar
describes what a **user does**. Blur is not a user action; it is a DOM event
that user actions cause. `Press Tab` is the user action, it already works, and
it additionally tests what the user actually experiences (focus lands somewhere
real). A `Blur` verb would be the first step in the grammar with no
user-observable gesture behind it. Instead, add one sentence to
docs/authoring.md next to `Press <Key>`: blur-triggered validation is exercised
with `Press Tab`. Drop GAP-H from the backlog once the doc line lands.

### 4d. Case-insensitive `page shows`: option (i), constrained to widening-only

Ruling:

- **Positive containment** (`page shows X`, `the "T" shows X`,
  `Wait until page shows X`): case-sensitive first; only when there is no
  case-sensitive match, fall back to ASCII case-insensitive. This mirrors the
  element ladder exactly, where a case-sensitive match always wins and
  case-insensitive is the last rung.
- **Negative** (`page does not show X`): NO fallback; stays case-sensitive.
  Widening a negative assertion makes it stricter: a recorded trace where
  `page does not show friends` passed against a rendered "FRIENDS" would start
  failing. The constraint that no passing trace may start failing beats
  symmetry. Document the asymmetry in one sentence.
- **Counts** (`page shows X <N> times`): if the case-sensitive occurrence count
  is nonzero, it IS the count; only a zero case-sensitive count falls back to
  counting case-insensitively. Same widening-only argument: any trace that
  passed before had a nonzero case-sensitive count, which is unchanged.
- **Rejected (ii)** reading `textContent`: it silently changes what "visible
  text" means for every assertion (hidden nodes, `display:none` subtrees) in
  unbounded ways. flowproof's surface is what renders; text-transform is what
  the user sees. **Rejected (iii)** opt-in syntax: it pushes a silent
  portability trap onto every author, and the grammar's job is to match human
  intent by default. **Rejected (iv)** doing nothing: the field evidence shows
  this bites every MUI/Bootstrap app.
- Implement in the matcher (`text_matches` / `check_text_expectation`), NOT in
  `surface_text`: failure messages must keep printing the true rendered surface.
- **Compat consequence: no version floor needed.** Previously-passing traces
  cannot fail (positive forms only widen; negative forms and nonzero counts are
  untouched). Previously-failing traces may start passing, which is the fix
  working. A changelog note suffices.

### 4e. `env_from` seeing `env:`: bug fix, not a breaking change, via two phases

The two orderings people conflate must be separated. (1) Which value wins for
`${VAR}` at flow time: **unchanged**, process env < env_from < `env:`. (2) What
the env_from **child process** sees: **changed**, it now sees `env:`.

Mechanism: phase 1, resolve each `env:` entry against the ambient process
environment only, and inject the resolvable ones into the env_from command's
child environment only (`Command::envs`, not `set_var`), silently skipping
entries whose refs do not yet resolve (they may reference env_from's own outputs
and get their turn later). Phase 2, run env_from and export its stdout pairs as
today. Phase 3, apply `env:` to the process exactly as today: lazy per-entry
resolution, now able to see and override env_from outputs, preserving the
documented compose/override property.

This is a bug fix because the documented promise "env: wins over process env"
was simply not honored inside the env_from subprocess, and minting test data
almost always needs the suite's base URL and credentials, as the field run
proved.

In the same change: **stop swallowing the command's stderr.** Capture stdout
only; let stderr inherit to the terminal. Half the field diagnosis cost was that
a failing mint script had no voice. Doc wording: "the env_from command runs with
the suite's env: visible (each entry resolved against the process environment;
entries referencing env_from's own outputs apply after the command instead).
${VAR} precedence in flows is unchanged: process env, then env_from output, then
env:."

## 5. Prioritization

**Iteration 1 (next):**
1. GAP-A instrumentation, timeboxed ~2h: from-source run against RWA with
   transport logging (session ids, target lifecycle, ws close). Deliverable: the
   fault scope named in STATE.md.
2. GAP-A part 1: transport-fault-as-poll-miss (decision 1). Unit-test with a
   scripted driver that faults N times then answers, plus the deadline-with-zero
   -successful-reads path.
3. GAP-A part 2 only if the root cause is case (a): bounded same-target
   re-attach per decision 1.
4. GAP-B in full (decision 2), including the `run_hook` bug and the errored
   outcome + exit codes.
5. GAP-J while you are in the error paths anyway: web faults must not say
   `UIA query failed`. Give `DriverError` a web-appropriate rendering (new
   variant or reworded constructor in `web_err`).

**Iteration 2:**
6. GAP-C (decision 3): error artifacts + the `--json` invariant.
7. GAP-D url assertion, GAP-E checkbox, GAP-F case fallback, GAP-G env_from
   (decisions 4a, 4b, 4d, 4e), each with docs table rows and CI-parsed examples.
8. If room: GAP-K (the relative-url error must blame resolution, not
   trace-writing) and orphaned-Chrome cleanup on failed records.
9. Acceptance bar for the round: re-run the 9-spec RWA suite from a source-built
   wheel; 8/8 recorded specs replay, spec 04 records, one driver fault no longer
   suppresses the junit.

**Deliberately REJECTED (drop from the backlog, with reasons to record):**
- **Blur verb**: rejected per 4c; docs line instead.
- **Observing the app's own network traffic**
  (`cy.wait("@alias").its("response.statusCode")`): rejected, same register as
  page.evaluate. It asserts on the wire, an implementation detail a refactor
  changes without changing behavior. Cypress needs `cy.wait` mostly for
  synchronization, and auto-wait IS flowproof's synchronization answer; the
  observation answer is outcome assertions (surface, url once 4a lands,
  `assert_api`, `assert_sql`). None of the 9 ported specs had a wire assertion
  whose outcome was unobservable by other means. Revisit only if a future
  migration produces one that genuinely is.
- **Shared authenticated setup (GAP-L) as new machinery**: rejected as a
  feature; it is composition of existing pieces once GAP-G lands. Recipe:
  `env_from` runs a script that logs in once over HTTP and prints
  `RWA_SESSION=...`; flows apply it via `session:`. After GAP-G ships, port one
  RWA spec to this pattern and document it as the "shared login" recipe. If the
  pattern fails in practice, that evidence reopens the design; a hypothetical
  does not.
- **Run bundles inside the project / vite watcher**: no behavior change. Add a
  short "dev-server watchers" note to getting-started recommending
  `server.watch.ignored` (and equivalents). `.flowproof/` inside the project is
  conventional, like `.pytest_cache`, and relocating artifacts would break the
  report-next-to-spec discoverability contract. A relocation env var is deferred
  until a second field report asks.
- **GAP-I non-semantic clickable rows**: no new vocabulary; fold into the
  table/list addressing design already parked in design.md (round 2), where row
  anchoring belongs. `css:` stays the documented answer meanwhile. The one cheap
  slice worth keeping: extend the replay-time "did you mean" (`augment_failure`)
  to record-time not-found errors, when convenient.
- **Perf (45s for 8 steps)**: kept in backlog, not scheduled. Do not optimize
  before the GAP-A instrumentation, which may itself explain part of it (poll
  cadence, `FIND_TIMEOUT` interactions). Measure, then decide next round.
- **Computed assertions (GAP-M)**: stays parked in design.md as
  designed-not-scheduled; this round added confirmation, not new information.

Key file references for the implementing agent:
`crates/flowproof-replay/src/lib.rs` lines 397-412 (`check_text_expectation`
poll loop, the `read()?` at 398 is the GAP-A seam), 430-441 (`resolve` closure,
same seam), 483-508 (`element_enabled` loop);
`crates/flowproof-cli/src/lib.rs` lines 499-661 (`run_suite`: the per-flow loop
for GAP-B, the `run_hook(...)?` bug at 578 and 586, `FlowSpec::load` abort at
523), 663-757 (`cmd_run` for the `--json` invariant);
`crates/flowproof-adapters/src/web.rs` lines 22-24 (`web_err` wraps everything
in `DriverError::Uia`, the GAP-J seam), 295-323 (`with_element` +
`is_transport_fault`), 670-695 (`surface_text`).

## DESIGN verdict 5 - GAP-O element-count assertion (2026-07-22, Fable)

# DESIGN DECISION - GAP-O: element-count assertion

## VERDICT: ACCEPTED. One form, match-set cardinality semantics.

```
the "<target>" appears <N> times [within <M>s]
```

Ship it in the same vocabulary batch as GAP-D and GAP-E, not before them. It
does NOT belong to the parked table-cell design (ruling in section 3).

## 1. Why accepted

This passes the test that killed Blur and page.evaluate: it describes
something a **user observes** ("there are two comments"), declaratively, with
no code and no control flow. The workaround (assert each item's text) is
strictly weaker - a duplicated or stray row passes - and `have.length` is the
"did the list render right" assertion. A test engine that cannot say "exactly
N rows" cannot check list rendering, filtering, cart contents, or
validation-error counts, which is most of what real suites assert.

## 2. Exact syntax and semantics

### 2a. The form

`the "<target>" appears <N> times` - also accept the singular
`appears 1 time`, mirroring the existing `split_count` treatment of
` time`/` times`. `within <M>s` accepted like every assertion. **No
ordinal slot**: `the 2nd "X" appears 3 times` is a parse error (an ordinal
picks one element from a match set; this form asserts the set's size - the
two are contradictory). No `once`/`twice` words; digits only.

Why this wording beats the candidates: it keeps the assertion shape
`the "<target>" <predicate>` intact; it is the element-side twin
of `page shows <text> <N> times` (text occurrences use
`shows ... times`, element occurrences use `appears ... times`); and
"appears" already means "resolves" in this grammar, since `is visible` is
documented as "target resolves". `has <N> items` / `has <N>
children` are rejected because they hard-code container semantics and
children-vs-descendants is exactly the fuzziness I refuse to ship.
`<N> "<target>" are visible` is rejected: it breaks the assertion
shape and drags in English pluralization.

### 2b. What is counted: the match set of the target itself

**The number of elements the target resolves to** - Cypress's
`cy.get(sel).should("have.length", N)`. Not direct children, not a separate
descendant selector. Three reasons: it needs no second locator (the target
already denotes the match set that `[2nd ]` ordinals index into); on web,
children-counting is a SPELLING of match-counting (`css:[data-test=comments-list] > *`)
while the reverse is not expressible at all; and "children" is where the
fuzziness lives (element children only? hidden? text nodes?).

**Precise definition:** `the "<target>" appears N times` asserts that
the match list which `the [Nth ]"<target>"` ordinals index into has
exactly N entries. Same ladder, same rungs: the first rung yielding at least
one match supplies the set; if no rung matches anything the count is 0.
Visibility is not an additional filter beyond what the ladder applies.

Consequences that fall out for free, do not special-case them: `appears 0
times` is legal and means what `is not visible` means - do not advertise it,
do not forbid it. If the recorded primary rung yields zero and a lower rung
yields the set, count the lower rung and mark the step **degraded**, exactly
like any other ladder fallback.

### 2c. Auto-wait: yes, mandatory

Default 10s, `within <N>s` accepted, `timeout_ms` recorded. Poll until
the count equals N - this uniformly handles N=0 and growing lists. Decision 1
of this round applies verbatim: a transport fault during a poll is a miss.
On timeout the message must report the last observed count:
`expected "css:..." to appear 2 times, found 3 after 10s` - an agent healing
a trace needs the observed number, not just "assertion failed".

Documented caveat, one sentence: like every polling count assertion including
Cypress's, a list that grows through the expected count can latch a transient
match. Seeded, settled data is the author's job.

### 2d. Comparisons: exact only. `at least` / `at most` REJECTED for v1.

A range assertion against seeded data is a test smell - it says "I do not
know what my fixture contains". `page shows N times` is exact-only; this
stays symmetric. If the field produces a genuine case (unseedable
third-party data), `appears at least <N> times` is purely additive
later. Do not build it speculatively.

### 2e. Trace serialization

`element_state` with a **new expect key `match_count`** and
`selector_ref: 0`:

```json
{ "kind": "element_state", "expect": { "match_count": 2, "timeout_ms": 10000 }, "selector_ref": 0 }
```

Do NOT reuse `count`: it is defined as occurrences of `value_contains` in the
surface text and always travels with `value_contains` + `scope: surface`. A
bare `count` next to a `selector_ref` would overload one key with two
meanings. `match_count` is self-describing and additive within trace v1.

Engine surface: one new trait method
`fn element_count(&mut self, selector: &UiaSelector) -> Result<u64, DriverError>`
with a default returning an error that names the adapter and points
text-anchor cases at `page shows <text> <N> times`. The web
adapter implements it by taking the length of the same match list `nth`
resolution walks.

### 2f. Provenance: shared grammar, web-implemented, honest elsewhere

The FORM belongs to the shared assertion grammar, not the web-only section -
unlike a URL, "how many matches" is meaningful wherever there is a tree (UIA
FindAll, SAP collections). Do not implement UIA/SAP counting now: the default
error is truthful and implementing is mechanical when someone hits it. Vision
is the one adapter where element counting is genuinely ill-defined; for
vision the answer IS `page shows <text> <N> times` and the error
says so. This differs deliberately from 4a's resolve-time rejection: a UIA
window truly has no URL ever, so rejecting at resolve was honest; a UIA tree
DOES have countable elements, so rejecting at resolve would encode a
temporary implementation gap into permanent grammar.

## 3. Relation to the parked table-cell design: DISTINCT. Ship now.

Table-cell addressing is a **locator** design (WHERE a target is).
`appears N times` is a **predicate** over a match set. Locators and
predicates compose; they do not compete. No future table decision is
foreclosed, so parking this would cost real coverage now to protect nothing.

## 4. Implementation notes (binding)

- Rules: new resolved action (suggest `AssertMatchCount { target, count,
  timeout_ms }`); the unresolvable-step error gains the new form.
- Docs: one row in the assertions table -
  `the "<target>" appears <N> times` | count of ELEMENTS the
  target matches (the text-count form counts TEXT occurrences) - with a
  CI-parsed example, e.g.
  `the "css:[data-test=comments-list] > *" appears 2 times`. Add the
  Cypress mapping note: `.children().should("have.length", N)` is spelled
  with a `> *` child combinator; `have.length.greaterThan` has no
  equivalent by design, assert the exact count.
- Tests: rules resolution (plural, singular ` time`, `within` suffix,
  ordinal rejection, the "good times" suffix trap), trace round-trip of
  `match_count`, replay poll including transport-fault-is-a-miss,
  degraded-rung counting, and the documented examples in the CI grammar test.
- Replay stays zero-model; no em dashes anywhere.

## 5. Sequencing

Not urgent. Build GAP-D, then GAP-E, then this, as one coherent vocabulary
landing.


# DESIGN verdict 6 - the TRACKER BATCH (2026-07-22, Fable), recorded verbatim

Summary of rulings, then the sequence. Full text is long; the binding parts:

## 0. Windows-without-Windows: ACCEPTABLE, with a hard condition
CI runs `windows-latest` on every PR (workspace tests, a real-UIA Notepad
E2E, a Notepad author E2E, the SAP simulator). That IS real Windows. Binding
condition for every Windows-facing PR: grammar/trace/replay tested via the
mock driver AND **the new Windows code path exercised by a `windows-latest`
E2E against a real application before merge**. Mock-only verification of a
Windows feature is forbidden. Paths CI cannot reach (UWP until #70's job
exists, real SAP per #32) ship with the limitation stated in the docs.

## Rulings
- **#57 computed assertions - BUILD NOW.** Two field rounds, two frameworks,
  two apps produced the same need; the parking rule wanted confirmation and
  got it. Capture step: `Remember the [2nd ]"<target>" as <name>`
  (`[a-z][a-z0-9_]*`). Reference, ASSERTIONS ONLY, whole expected text is
  exactly one of `${captured.<name>}`, `${captured.<name>} + <number>`,
  `${captured.<name>} - <number>`; digits only, no second capture, no
  nesting, no `*`/`/`. That is the entire expression grammar, forever. Value
  NEVER enters the trace (same property as `${VAR}`). `${captured.*}` in an
  ACTION step is a parse error - captures feed assertions, never control
  flow. Trace: action `capture` `{"name"}`; expect `{"capture", "offset"}`.
  Heal may re-anchor selectors but must never alter name/capture/offset.
- **#58 table-cell addressing - KEEP PARKED**, with a forcing function:
  make **nocodb the target after documenso**, then escalate the real DOM
  evidence and the full three-layer design gets ruled in one verdict.
- **#66 arbitrary Windows apps - BUILD NOW.** `app:` accepts a scalar (as
  today) or a mapping `{command, window_title}`, both `${VAR}`-capable and
  stored RAW. Trace header keeps `app` as a string, uses reserved id
  `"windows"` plus a new optional `launch: {command, window_title}`.
  Non-Windows hosts reject mapping-form specs by name. `command` is executed
  code: same trust surface as `env_from`.
- **#67 generic UIA action grammar - BUILD NOW (after #66).** Not new
  vocabulary: wire the EXISTING forms to act under UIA. v1 covers click,
  press-button, type/replace/clear, focused type, keys and chords, ordinals,
  `id:` as AutomationId, text anchors over UIA Name with the standard
  ladder. `css:` under UIA is a parse error. Right-click/Upload/Go to/Reload
  get honest not-implemented errors. Honesty over symmetry in the docs.
- **#68 window geometry - BUILD NOW, but as CONFIG. The action-verb reading
  is REJECTED.** Stable geometry is a determinism precondition, not a user
  gesture. Top-level `window: {width, height, x?, y?}`, applied once before
  step 1, in the trace header, identical at record and replay. Failure to
  apply ERRORS the flow. Web flows reject it pointing at `browser:
  viewport`. UWP: walk up to the ApplicationFrameHost frame ancestor.
- **#69 vision word-level OCR - BUILD NOW.** Pure implementation, no API
  risk, testable everywhere (ocrs is pure Rust). Match over word AND line
  granularity; exact whole-token word match beats in-line substring;
  multi-word anchors match adjacent runs, click the union rect centre;
  ordinals order top-to-bottom then left-to-right. Test: synthetic 3x3+
  digit grid, every digit resolves uniquely.
- **#70 UWP docs - BUILD NOW (docs)**, marked honestly as verified once and
  not re-runnable by the loop. Timebox HALF an iteration to attempt a
  `uwp-e2e` CI job; if the runner fights back, keep docs, drop the job.
- **#60 agent-boundary v1 - BUILD, sequenced after the Windows batch**, as a
  contiguous 2-3 iteration run, and **done only when a real external OSS
  agent records and replays through the proxy**. Three open design questions
  settled: cassette matching is strict by position with envelope-first
  diffing and NO tolerance holes in v1; branching fails at the first
  divergent turn; "reply" is the final assistant message in the trajectory.
- **#61 - KEEP PARKED**, explicitly gated on #60 landing. Do not close: it
  is the recorded shape of the roadmap.
- **#32 real SAP - KEEP PARKED** (org infrastructure). Do not close: it is
  the honest record that the COM wire protocol is proven and SAP's own
  behavior is not. Zero iterations; ping the maintainer once per round.

## Closures: NOTHING closes
Every item is evidence-backed, twice-confirmed, correctly awaiting evidence,
correctly gated, or an honest record of an unverifiable claim. Two are
reshaped rather than closed: #68 ships as config, #58 stays parked with
nocodb as its forcing function.

## The sequence (one substantial feature per iteration)
1. **#57 computed assertions** - first, because it is the only item the
   ACTIVE field program hardens immediately (documenso is web, and
   payment/balance flows exercise captures within days).
2. **#66 + #68 together**, with #70's docs riding along in the same PR set.
   Notepad-via-mapping E2E is the merge gate.
3. **#67 generic UIA actions**; extended Notepad E2E is the merge gate.
   Timebox the #70 `uwp-e2e` job attempt into this iteration's slack.
4. **#69 vision word segmentation**; synthetic-grid tests are the merge gate.
5. **#60 agent-boundary v1**, the contiguous batch, ending with the OSS-agent
   field proof.
6. **#58** - not scheduled; triggered by nocodb evidence.

## Addendum: two round-3 gaps ruled here
- **GAP-Q computed navigation: REJECTED.** Control flow in a test, same
  register as page.evaluate. Documented answer: pick a date in the visible
  month, or seed so the target month is current.
- **GAP-P clock control: real, P1, NOT ruled.** Freezing "now" touches
  determinism guarantees, trace semantics and per-adapter feasibility (CDP
  virtual time vs a Date shim vs nothing on UIA). Needs its OWN brief with
  the concrete RWA cases attached. Escalate separately.
