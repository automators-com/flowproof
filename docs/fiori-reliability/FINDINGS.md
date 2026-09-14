# Fiori reliability — overnight run, findings

Branch: `fiori-reliability/2026-09-14`. Started 2026-09-14, halted in Phase 0
before any production code was touched. No commits beyond this write-up; no
push.

## Status: halted at Phase 0, on an explicit halt condition

The brief (`~/Downloads/fiori-reliability-overnight-brief.md`) lists as a halt
condition: *"free disk space stays below 15 GB after the cleanup allowlist is
exhausted."* That condition is met. See "Disk space" below. Ground rule 6 of
the brief is explicit that autonomy covers **decisions**, not **permissions**,
and that halt conditions are not something a judgement call can override — so
this halts rather than proceeding into Phase 0b/1/2, which is exactly the
"decide and proceed" choice ground rule 6 actually calls for here: the decision
is to stop, not to guess past a line the brief itself drew.

No FAA baseline was ever established. No production code, trace format, or
fixture code was written or modified. Nothing in this session should be read
as a measurement of anything — it is a pre-flight check that came back red.

## Ground rule 2 check — issues and PRs (done before anything else)

- `gh pr list`: 5 open PRs. None target Fiori reliability, the selector
  ladder, or the web adapter's UI5 handling. `#540` (`docs/plan-sap-gui-fiori-excel-config-demo`,
  open, HappyDevs1) is a **planning doc** for a SAP GUI → Fiori → Excel config
  demo — adjacent territory, not a conflict, but worth reading before design
  work on the ladder starts.
- `gh issue list`: only one open issue mentions Fiori — **#536** ("SAP GUI flow
  continuing into Fiori, with flowproof config"), opened by HappyDevs1
  (the account this session runs as), no labels, not `needs-human`. Related
  context for whoever resumes this; not a blocker.
- **No open PR conflicts with this brief's scope.** The brief's own
  instruction ("do not duplicate or conflict with in-progress work") is
  satisfied on the *open* side.

## A factual gap in the brief itself

The brief's Phase 0b says to build the UI5 fixture "under `examples/fiori/`
(new, does not exist yet)." **That is false as of this run.**
`examples/fiori/` already exists, with real content merged in the last five
days:

```
d1291f1 chore(ci): schedule SAP GUI E2E 2h after the Fiori E2E leg (#583)
99199a1 fix(examples): give manage-info-records' iframe assert an `assert:` key (#582)
142b182 examples(fiori): record manage-info-records trace, read-only (#581)
dcfba03 fix(ci): wire OData secrets into Fiori E2E workflow (#579)
e663fb5 feat(ci): add Fiori E2E workflow replaying live launchpad flows (#577)
```

Contents: `display-info-record-by-supplier.flow.yaml`, `login-smoke.flow.yaml`,
`manage-info-records.flow.yaml`, `purchase-info-records-report.flow.yaml`,
`purchasing-info-record-api.flow.yaml`, plus `.trace.jsonl` cassettes and a
shared `values.yaml`. These are traces against the **real launchpad**, wired
into a live Fiori E2E CI workflow (with OData secrets), not a local mock
fixture. This is a different thing from what the brief wants built (an
offline, MockServer-backed fixture for fast, credential-free iteration), but
it occupies the exact directory the brief told this session to create fresh.

**Decision (recorded here per ground rule 6, since it never got acted on):**
had Phase 0b been reached, the new fixture would **not** go into
`examples/fiori/` directly — it would go into a clearly-separated subdirectory
(e.g. `examples/fiori/fixture/` or a new top-level `examples/fiori-fixture/`),
so the real-launchpad E2E examples and their CI wiring are untouched. Whoever
resumes this should make that call explicitly rather than dumping mock-app
files alongside real recorded cassettes. This also means Phase 4 (the
real-system checkpoint) has a running start it didn't expect: `examples/fiori/`
already proves flowproof records and replays against the real launchpad for at
least five flows, which is itself a data point worth folding into the eventual
report rather than re-discovering from scratch.

Also relevant: the brief's charter context. `CHARTER.md` (constitution,
authoritative over `CLAUDE.md`) lists the **current milestone** as #187/#188
(agent-boundary diagnostics) and #61 (agent coverage) — Fiori reliability is
not on that list, and Tier 4 (SAP/desktop) work is marked "human-driven,
always" in the work-engine section, though this brief's scope is the `web`
adapter (Tier 3 territory: browser via CDP), not the `sap` GUI-scripting
adapter, so it isn't the same lane. Flagging the mismatch rather than either
silently deferring to the charter or silently overriding it — this is a
judgement call for whoever reads this in the morning, not one this session
should make unilaterally given it never got past Phase 0.

## Disk space

```
Before cleanup: /dev/disk3s1s1  228Gi total,  17Gi used,  6.2Gi avail, 73% capacity
After cleanup:  /dev/disk3s1s1  228Gi total,  17Gi used,  6.3Gi avail, 73% capacity
```

Allowlist items attempted, in the brief's order:
1. Own artifacts — none existed yet (no work had started).
2. `cargo clean` — no `target/` directory exists yet in this checkout; nothing
   to reclaim. `~/.cargo/registry/cache` doesn't exist either (no cargo build
   has run on this machine for this checkout).
3. `npm cache clean --force` — ran, cache was 313M, cleaned.
   `pnpm store prune` — ran, removed 778 packages / 42,763 files. Net effect on
   free space: +0.1 GB. Most of what `pnpm store` held was still referenced by
   other projects' lockfiles, so prune reclaimed less than the store's total
   size.
4. `brew cleanup -n` (dry run) — nothing eligible; all outdated-formula
   warnings were "not installed," not "installed but superseded."
5. Xcode DerivedData — directory doesn't exist. `xcrun simctl` — no
   unavailable devices.
6. Playwright/Puppeteer caches — neither directory exists on this machine.
7. `__pycache__` / stale venvs — none created this session (no Python work
   happened).
8. Docker — daemon (OrbStack) isn't running; nothing to prune.
9. Other repos' `node_modules`/`target` — not searched; moot once the halt
   condition was already met from items 1–8, and this item is explicitly
   "only if still short" after exhausting the rest of the list, which had
   already happened.

**Root cause, to the extent it's visible from user space:** `df` reports
`228Gi` total for this APFS container but only `6.2–6.3Gi` avail — the
`disk3` container's other volumes (System, Preboot, Recovery, VM/swap) and
three `com.apple.os.update-*` local snapshots (`tmutil listlocalsnapshots /`)
are consuming the rest. `diskutil apfs list` shows **six additional APFS
containers** on this Mac beyond the one holding the checkout
(`disk5`/`disk7`/`disk9`/`disk11`/`disk13`/`disk15`), each a separate
container — consistent with mounted disk images. Per the brief's protected
list ("UTM and all virtual machines... any disk image anywhere on the
machine"), none of these were inspected further or touched. OS update
snapshots are also not on the allowlist and were left alone.

This is a **machine-level** space problem, not a repo-level one, and not one
this session has a safe tool for. Reclaiming it needs either a human decision
about the other containers/VMs, or `softwareupdate`/OS-level cleanup of the
update snapshots — both outside what the allowlist authorizes.

## Phase 0a — build/test attempt, and a second disk scare

Resumed at 9.3GB free (see Decisions). `cargo build --workspace`:

- **All engine crates built clean**: `flowproof-driver`, `flowproof-trace`,
  `flowproof-replay`, `flowproof-agent`, `flowproof-adapters`, `flowproof-cli`.
  This is everything the Fiori/web-adapter work actually touches.
- **`flowproof-python` fails to link**, macOS-only, environment-specific:
  `ld: symbol(s) not found for architecture arm64` for CPython C-API symbols
  (`_Py_GetVersion`, `_Py_IncRef`, `_Py_InitializeEx`, ...) despite
  `extension-module` already being set in `crates/flowproof-python/Cargo.toml`
  (the correct config for a cdylib extension module). Tried the system
  `/usr/local/bin/python3` (python.org 3.14) and, via `PYO3_PYTHON`, Homebrew's
  `python@3.13` — **identical failure both times**, which rules out "wrong
  Python selected" as the cause. CI's `build`/`lint` jobs run
  `cargo build/test --workspace --all-features` on `ubuntu-latest`, where this
  class of macOS linker issue doesn't apply, so this is very likely a local
  toolchain/Python-framework quirk on this Mac, not a code regression — but it
  is genuinely unresolved, not dismissed; if it turns out to also fail in a
  clean macOS CI runner, that would be a real finding worth its own issue.
  **Excluded `flowproof-python` from the rest of Phase 0a** rather than debug
  it further — it's unrelated to Fiori/web-adapter reliability and CI already
  covers it on Linux.

`cargo test --workspace --exclude flowproof-python`: **aborted, not completed.**
While its test binaries were compiling and linking (a workspace with
`nalgebra`, an OCR crate, and `headless_chrome` among the dependencies — a
legitimately large build), free space fell from 9.3GB to 4.5GB in about 12
minutes, then to 3.6GB within another ~2-3 minutes — accelerating, not
linear. `target/` reached 5.7GB and was still climbing. Rather than wait for
the next scheduled disk check (which is exactly the gap that could have let
it cross zero), the test run was killed proactively
(`pkill -9 -f "cargo test"` + `pkill -9 -f rustc`) the moment the trend looked
dangerous rather than after it became critical. `cargo clean` (allowlist item
2) then reclaimed 6.1GB, restoring ~9.1GB free.

**This is a real, demonstrated finding, not a one-off**: 9.3GB free is not
enough headroom to run this workspace's full test suite once — peak `target/`
usage was still climbing past 5.7GB when killed, and the earlier `cargo build`
alone (without test binaries) had already reached 2.1GB. A conservative
estimate is the full `cargo test --workspace` needs on the order of 6-8GB+ in
`target/` at peak, which leaves little to no safety margin at 9.3GB free,
let alone anything close to the brief's original 15GB gate.

**No test results exist.** Not "tests failed" — the run never reached
completion, so there is no pass/fail count to report for Phase 0a's test
suite. Re-running it needs either more free space than is available right
now, or running it crate-by-crate (smaller peak footprint per invocation,
though more wall-clock time overall) as a workaround — noted for whoever
resumes this, not attempted tonight given the two close calls already spent
proving the same constraint.

## Phase 0a — resolved: engine green, one known-environment gap

Second attempt (12GB start, more headroom) completed without a repeat of the
disk near-miss — compilation was the disk-heavy phase, and once test
*binaries* started actually running (not compiling), disk usage flattened.
Result: **135 + 1 + 375 + 5 + 105 + 10 + 1 + 17 + 3 + 1 + 6 + 8 + 5 + 3 passed,
2 failed** across the workspace (excluding `flowproof-python`, still
unbuildable locally per the earlier note).

The 2 failures (`crates/flowproof-cli/tests/doctor_ai_e2e.rs`,
`doctor_ai_without_a_key_fails_without_a_model_call` and
`doctor_ai_openai_can_validate_against_a_local_compatible_endpoint`) are
**not a code defect and not introduced tonight** — root cause confirmed by
reading the failure output, not guessed: the first test expects `flowproof
doctor ai` to report "no key configured" with no model call made, but its
own stdout shows `api key: configured` / `model call succeeded.` — this
machine has a real, previously-configured flowproof AI config at
`~/Library/Application Support/flowproof` (this developer's own normal
flowproof usage, unrelated to tonight's work or to this being a Claude Code
session), and the test doesn't redirect/isolate the config path it reads
from, so it picks up the ambient real one and makes a real Anthropic API
call as a side effect of running the suite. The second failure
(`env lock: PoisonError`) is a consequence of the first panicking while
holding a shared env-mutation lock the two tests serialize on.

**Left untouched, per the ground rules**: did not edit the test (would be
"weakening an assertion" against the letter if not the spirit — the test's
logic is correct, its isolation is incomplete), and did not touch
`~/Library/Application Support/flowproof` (that's this developer's real,
valuable, working AI config from actual flowproof use — deleting or moving
it to "prove" the hypothesis would be a destructive action against something
outside this session's scope, for a test that's already explained). This is
recorded as a pre-existing test-isolation gap, out of scope for tonight
(unrelated to Fiori/web-adapter reliability), not something this session
fixed or should have fixed.

**Phase 0a verdict: the floor is solid enough to build on.** Every crate the
Fiori work touches (trace, replay, agent, adapters, cli) is green with zero
unexplained failures.

## Phase 1 — hypothesis verdicts (from reading code, pending fixture verification)

These are from reading the actual adapter/trace/agent source, done in parallel
with the Phase 0 build/test cycle. They are real code citations, not
speculation — but per the brief, "verify or kill, do not assume" ultimately
means running it against a fixture, which hasn't happened yet. Treat these as
strong leads, not closed verdicts.

- **H1 — native-id selector is harmful for UI5: CONFIRMED, AND FIXED.**
  [`web.rs:3867`](../../crates/flowproof-adapters/src/web.rs) `semanticCss()`
  returns `'#' + CSS.escape(el.id)` unconditionally whenever `el.id` is
  non-empty — checked *before* `data-testid`/`aria-label`/`name`, and before
  any class-based fallback. For a UI5 control this is exactly the generated,
  view-instance-counter-bearing id the brief describes
  (`__xmlview0--idTable-listUl`), and it becomes the recorded `native_id`-tier
  selector — [`lib.rs:36`](../../crates/flowproof-trace/src/lib.rs), tier 0,
  tried first at replay. **Fixed, not just diagnosed**: the `a11y` tier (see
  "Implementation status" below) is now captured, ranked above `native_id`,
  and resolves correctly at replay - proven end to end by a test where the
  native id changes (the exact failure this hypothesis describes) and replay
  still passes via `a11y`, never touching the dead `native_id` rung.
- **H2 — no UI5-aware idle signal: CONFIRMED, same mechanism as H5, AND
  FIXED.** [`web.rs:788`](../../crates/flowproof-adapters/src/web.rs)
  `settled_scene()`: waited for two DOM-shape reads 100ms apart to agree,
  capped at 20 rounds (~2s), then proceeded regardless. No concept of a
  busy indicator or an in-flight OData batch. **The same function backs
  authoring**: `driver.scene()` in
  [`recorder.rs:2327`](../../crates/flowproof-agent/src/recorder.rs) is called
  directly before handing the scene to the model for grounding, with no
  additional wait — so H5's "the model grounds against a half-rendered DOM"
  and H2's "replay acts on a control about to be destroyed" are one root
  cause wearing two symptoms, not two separate defects. **Fixed generically**:
  a CDP `Network` event listener tracks in-flight requests; `settled_scene`
  now requires `network_idle()` alongside shape/ready agreement, with the
  round budget only extending once a round actually observes a pending
  request (a quiet page still settles in ~200ms, unchanged). See "Implementation
  status" below for the full mechanism and its live-Chromium proof (a real
  2-second delayed fetch, the scene correctly waits for it).
- **H3 — missing UI5 selector rung: CONFIRMED ABSENT.** No reference to
  `sap.ui.test.RecordReplay` or `waitForUI5` anywhere in the codebase
  (`grep -rn` across `crates/`). The brief's proposed fix — a `ui5` selector
  tier above `native_id`, using `RecordReplay.findControlSelectorByDOMElement`
  / `findDOMElementByControlSelector`, plus `waitForUI5` as the inter-step
  barrier (subsuming H2/H5's fix) — is architecturally exactly what H1+H2
  jointly point at. Not yet designed in code; this needs the design-note
  treatment the brief calls for (new selector tier = trace-format-adjacent
  change) before implementation, not a quick patch.
- **H4 — dialogs/popovers escape the search root: KILLED, verified live.**
  The adapter's element search (`web.rs`, scene-building and `try_find`)
  operates against `document.querySelectorAll(...)` — the whole top-level
  document — with special-case handling only for iframes. `#sap-ui-static`
  is a sibling div in the *same* document, not a separate frame, so nothing
  in the code scopes search away from it. Confirmed live, not just read:
  `crates/flowproof-adapters/tests/static_area_e2e.rs` reproduces the exact
  DOM shape (an element appended as a SIBLING of the app root, not a
  descendant - the same relationship `#sap-ui-static` has to a UI5 app's
  root) generically, without needing the full UI5 fixture. Both css/
  native-id lookup and the `a11y` tier's own resolution reach it correctly.
  Kept as a permanent regression test, not just a one-off measurement.
- **H5 — see H2.** Not a separate mechanism; folded in above.

**H3b — user-proposed, not yet evaluated: the accessibility tree, not a
UI5-specific rung.** Chrome exposes role/name/state over CDP
(`Accessibility.getFullAXTree`). UI5 emits ARIA, so controls carry stable
semantic identity that survives re-render, routing, and version bumps -
without needing anything UI5-specific. Proposed: an `a11y` provenance rung
(role + name + ancestor scope) placed **above** `native_id`, evaluated against
H3's `sap.ui.test.RecordReplay`-based design rather than assumed inferior to
it. Advantages if it performs comparably: nothing injected into the page, no
dependency on `sap.ui.test` being present/enabled in a production (non-debug)
build - a real open risk H3 itself flags and this session has not yet
verified either way - and it generalizes to every `app: web` target, not only
Fiori. **Both designs need to be measured before choosing one**, not decided
by architectural taste. Not yet investigated; next concrete step once back in
investigation mode.

**Explicit constraint carried forward from here on, restated because it
matters more than any single finding**: every fix this session proposes must
address the general mechanism, not make one spec pass. A `within Ns` added to
a single flow, or any change scoped to one test case's shape, is not a fix
under this rule even if it turns that one spec green.

**Ranked implication for Phase 3** (specs unblocked ÷ risk, per the brief):
1. A UI5-aware idle/settle primitive (`waitForUI5` via CDP `Runtime.evaluate`)
   — closes H2 and H5 together, is additive (new wait path, doesn't touch the
   trace format), and is the highest-leverage single change.
2. A `ui5` selector tier above `native_id`
   (`findControlSelectorByDOMElement`/`findDOMElementByControlSelector`) —
   closes H1, but touches the trace format (new provenance tier) and needs
   the design-note + migration-path treatment the brief requires before
   landing, so it's higher-risk and comes second despite being the most
   "obviously right" fix.
3. Confirm or kill H4 empirically once the fixture exists — likely a
   non-issue, but cheap to verify and worth doing before spending effort on
   it.

## Real-system probe #1 - a genuine, honest first-attempt failure

Per the pivot above, ran a naive QA-style spec (never seen by anyone, not
copied from the existing expert-tuned `manage-info-records.flow.yaml`)
against the real launchpad, reusing only the already-known-safe read-only
search values from `values.yaml`. Originally written as:

```yaml
steps:
  - Log in with ${FIORI_USER} and ${FIORI_PASSWORD}
  - Open the Change Purchasing Info Record app
  - Search for material ${MATERIAL}, supplier ${SUPPLIER}, plant ${PLANT}, purchasing org ${PURCHASING_ORG}
  - assert: page shows General Data
```

**`flowproof record` has a live self-repair mechanism this session did not
know about going in.** It auto-rewrote the ambiguous "Search for
material X, supplier Y..." step into an explicit per-field version (naming
the mechanism: "the original step bundled four values into a single vague
instruction without naming the fields") - and **it mutated the `.flow.yaml`
file on disk while doing it.** This is a genuine, important nuance the brief
does not address: the FAA definition is "zero edits to the spec **between**
record and green" - a tool-driven in-flight rewrite during `record` itself is
arguably not the same thing as a human going back afterward to fix a
failure, but it does mean the file a user re-reads after `record` is not the
file they wrote, without them asking for that. Recorded here rather than
picked a side on it: whoever scores the actual FAA rounds needs to decide
whether an auto-repaired-but-still-failing recording counts as "zero edits"
before Phase 2's harness can score anything consistently.

**The real, unrepaired failure**: the very next step, "Open the Change
Purchasing Info Record app," failed immediately after login with "No
interactable elements are present on the current screen." The repair engine
correctly diagnosed this as `page-not-ready-transient` - "a launchpad/tile-
loading race condition after sign-in" - but could not apply a fix:
`"model did not return a usable patch: step has no `within Ns` wait window to
widen"`. Outcome: `budget-exhausted`. **This is a real, live, first-attempt
FAA failure on the actual production system**, and it is exactly the
mechanism H2 predicted (no UI5/shell-aware idle signal) manifesting exactly
where the brief's Mission section says it does: a competent user who has
never seen flowproof internals has no reason to write a `within Ns` clause
on their very first step, so the one repair path that could have saved this
recording was never reachable from an honest first attempt.

**What this session will NOT do about it**: patch this one spec with a wait
clause and call it fixed. Per the correction above, the only fix worth
making here is a **general** post-login/shell-settle wait in the adapter
(or recorder) that doesn't depend on the user having written a widenable
wait clause at all - closing the actual mechanism, not this one symptom.
That is now the top candidate for Phase 3, empirically confirmed on the
real system rather than only inferred from source.

## Real-system probe #2 - H3 vs H3b, decided empirically

Logged into the real launchpad directly (same credentials flowproof itself
uses, via its `~/Library/Application Support/flowproof/config.yaml` `fiori:`
profile - confirmed by reading `crates/flowproof-cli/src/config.rs:296`,
which injects `FIORI_USER`/`FIORI_PASSWORD`/etc. as real process env vars at
startup; this is also what explains probe #1's login succeeding without
`.env` defining those names - not a mistake, a different, legitimate
credential source than assumed at first).

**Real UI5 version confirmed: 1.114.11** (`sap.ui.version` in-browser), not
the 1.120.20 guessed for the mock fixture's `ui5.yaml`. Corrected there.

**Independently reproduced the tile-loading race** found in probe #1: the
Home page was blank at 3s after login, tiles rendered by ~13s. Consistent
with, and independent confirmation of, H2/the general-settle-mechanism
finding - this was observed by direct browser inspection, not only through
flowproof's own error message.

**H3 (`sap.ui.test.RecordReplay`) - real friction, not just a theoretical
risk.** All 64 of `RecordReplay`'s transitive dependencies load with HTTP 200
from this production instance (so the brief's "verify it's exposed in
production, non-debug builds" concern is not what bites here - it's genuinely
deployed). But calling `sap.ui.require(["sap/ui/test/RecordReplay"], ...)`
from an external, CDP-injected script **never completes** - the require
callback fires neither success nor error. Console shows the actual cause:
`Uncaught TypeError: Cannot read properties of undefined (reading
'findControlSelectorByDOMElement')`, thrown from inside
`sap/ui/test/autowaiter/_timeoutWaiter.js`. Reading this as: `RecordReplay`'s
own module body (or a dependency) expects state that OPA5's own test harness
normally sets up before anything touches it (its iframe-launcher wiring,
most likely) - it is not designed to be invoked cold from outside that
context, which is exactly how flowproof's CDP-based web adapter would have to
call it.

**H3b (accessibility tree via CDP) - confirmed rich and immediately usable,
right now, on the real system.** A full-page accessibility snapshot of the
real, live Home page (via this session's own `read_page`, which reads the
same class of data `Accessibility.getFullAXTree` would) shows every
interactive element with a stable, semantic role+name pair:
`link "Change Purchasing Info Record Tile"`, `button "Home - Show All My
Apps"`, `tab "Purchasing"`, `heading "My Apps"`. No page injection, no
dependency on a fragile module that crashes outside its intended harness,
and this generalizes to every `app: web` target the way H3b's proposer
argued, not only Fiori.

**Verdict on H3 vs H3b, from this evidence: H3b is the stronger candidate.**
Not a final decision - a real design comparison still needs writing (per the
brief's own rule that a new selector-ladder rung needs a design note before
implementation, given trace-format implications) - but the empirical
evidence from the actual production system points the same direction the
user's architectural instinct did: nothing to inject, nothing that can crash
outside a harness it wasn't built for, and one mechanism instead of two
(Fiori/UI5 plus everything else) if it measures comparably on real specs.

## Design note: the `a11y` selector tier

Required by the brief before touching the trace format ("a new selector-
ladder rung is [a design change]... interface, provenance ordering, trace-
format implications, backwards compatibility, migration path"). Written
before any implementation commit.

### Interface

A new `Selector.payload` shape, carried under the existing generic
`Params = Map<String, Value>` field - **no change to the `Selector` struct
itself**, since `payload`'s shape already varies by tier (`{"css": "..."}`
for `native_id`/`structural`/`text_anchor` today). Proposed shape:

```json
{
  "tier": "a11y",
  "provenance": "web",
  "confidence": 1.0,
  "payload": {
    "role": "link",
    "name": "Change Purchasing Info Record Tile",
    "ancestor_role": "list",
    "ancestor_name": "My Apps"
  }
}
```

- `role`/`name`: from Chrome's `Accessibility.getFullAXTree` (CDP), the same
  data this session's own `read_page` calls surfaced live against the real
  launchpad - not something that needs inventing, Chrome already computes it
  per the browser's own accessibility-tree algorithm (ARIA-aware, works
  whether or not the page authored ARIA explicitly).
- `ancestor_role`/`ancestor_name`: the nearest named ancestor (a landmark,
  a list, a heading region), optional - included when `role`+`name` alone
  isn't unique on the page, same spirit as the existing CSS-selector
  fallback-to-scoped-container logic already in `web.rs`'s `relativeCss`.
- **Capture** (record time): after a click/type resolves a target DOM node,
  fetch the accessibility node for that `backendNodeId` (CDP
  `Accessibility.getAXNodeAndAncestors` or an equivalent walk of
  `getFullAXTree`), extract role+name+ancestor.
- **Resolution** (replay time): fetch the current AX tree, find the node
  whose role+name (+ ancestor, if the selector carries one) matches, resolve
  its `backendNodeId`/`objectId` back to a DOM element via CDP
  `DOM.resolveNode`. No page-side injection - both directions are CDP calls
  the driver already makes (it already calls other CDP domains for
  screenshots, box models, dialog handling).

### Provenance ordering

Placed **above** `native_id` - tier 0, pushing the existing five down by one:

```
a11y (0) -> native_id (1) -> structural (2) -> text_anchor (3) -> visual_template (4) -> ai_relocation (5)
```

Justification, from this session's own evidence, not just the general
argument: `native_id` tier-0 was empirically shown capturing
`__clone0`/`__xmlview1`-style generated ids (probe #1) - exactly the least
stable identifiers in a UI5 app - while the accessibility tree on the same
real system carried stable, human-legible names for the same elements
(probe #2). Ordering the ladder to prefer the more stable signal first is
the fix H1 actually calls for; every other tier keeps its relative order.

### Trace-format implications

- **Wire compatible, not wire-breaking.** `SelectorTier` already serializes
  by name (`"native_id"`, `"structural"`, ...), not by its Rust discriminant
  - confirmed by reading `crates/flowproof-trace/src/lib.rs` and this
  session's own recorded traces. Renumbering the Rust `enum` discriminants
  (needed so `LADDER`'s array order and `Ord` stay meaningful) changes
  nothing about how `SelectorTier` values already on disk deserialize.
  **Every existing committed trace keeps validating and replaying exactly as
  before** - this addition changes what *new* recordings may contain, not
  what old ones mean.
- **One real compatibility risk, and it needs a decision from whoever
  implements this, not a guess from this session**: an *older*
  `flowproof-replay` binary reading a *newer* trace that contains
  `"tier": "a11y"` will hit an unrecognized enum variant. Whether that should
  be a hard parse error (current likely `serde` default, given no `#[serde(other)]`
  fallback on `SelectorTier` today) or a graceful "unknown tier, fall through
  to the next one in the trace" is a real forward-compatibility policy
  question the trace format doesn't currently have to answer (every tier
  added before this one shipped in the same commit as the readers that
  understand it, that same-commit constraint doesn't hold across independent
  installs of the CLI and old cassettes/new cassettes crossing paths).
  Flagging rather than deciding: this is exactly the kind of trace-format
  policy question CHARTER.md §8 lists under "any change to... the trace
  format" as something to label `needs-human`, not resolve unilaterally in a
  patch commit.
- **Schema and docs update in the same commit as the enum change** -
  `crates/flowproof-trace/schema/` and `docs/trace-format.md` - per
  `CLAUDE.md`'s own stated rule, mechanically enforced by the `adversary`
  gate.

### Backwards compatibility / migration path

- **Additive only.** No existing tier is removed, renamed, or reordered
  relative to each other - only inserted above. A replayer that has never
  seen `a11y` continues to correctly replay every trace that predates it.
- **No re-recording required for existing cassettes.** They keep whatever
  tier they were recorded with; `a11y` only appears in traces recorded after
  this ships, and only for elements where an accessible name/role could
  actually be captured.
- **Graceful degradation, matching the existing ladder's own philosophy**:
  when the current page's target has no meaningful accessible name (a bare
  `<div>` with no ARIA and no text, for instance), the recorder should
  simply not produce an `a11y` selector for that step and fall through to
  `native_id` as today - this is not a replacement for the rest of the
  ladder, it's a better tier-0.

### Scope check against CHARTER.md §3

CHARTER.md forbids "selector-engine growth to match another framework's
idioms." Read literally this could look adjacent to that line, so it is
worth being explicit: this is not adopting jQuery/Cypress-style selector
syntax, and it is not UI5-specific machinery either (unlike H3's
`sap.ui.test.RecordReplay`, which *would* be framework-specific). It is a
single, generic CDP capability (`Accessibility.getFullAXTree`) available on
every page Chromium renders, regardless of what UI framework built it. If
anything it *reduces* Fiori-specific surface area relative to H3's design,
which is the comparison this session's evidence argues for. Recorded here so
whoever reviews the eventual PR does not have to re-derive this reasoning
from scratch.

## Implementation status: H1 closed end to end, capture AND replay resolution

**Both halves are real, working, and tested against live Chromium.** Capture:
`WebAppDriver::a11y_hint` (a new `AppDriver` trait method, following the
exact `cell_hints`/`scope_hints` pattern) resolves the already-found
element's `backend_node_id` and calls CDP `Accessibility.getAXNodeAndAncestors`
to read the browser's own computed role + accessible name (+ nearest named
ancestor), prepending an `a11y` selector first in the ladder. Resolution:
`WebAppDriver::resolve_a11y` reads the WHOLE accessibility tree (CDP
`Accessibility.getFullAXTree`), finds the node matching role+name (narrowed
by the nearest named ancestor when that pair alone isn't unique), and turns
its `backendDOMNodeId` into a live `headless_chrome::Element` by hand -
`DOM.describeNode` for `node_id`/`attributes`/`tag_name`, `DOM.resolveNode`
for the remote object id, the same two calls the crate's own `Element::new`
makes internally, just starting from a backend id rather than the
querySelector-style node id `Element::new` requires.

**Three real bugs found by testing this live, not assumed from reading
code**:

1. The vendored `headless_chrome` fork's `AXPropertyName` enum is missing a
   variant real Chrome actually sends (`"uninteresting"`), which failed
   strict deserialization of the crate's own typed accessibility responses
   before this code ever saw a role or name. Fixed by defining small custom
   `Method` impls (`GetAxNodeAndAncestorsRaw`, `GetFullAxTreeRaw`) that
   decode `nodes` as raw `serde_json::Value` and read only `role.value`/
   `name.value` - same wire calls and params, no dependency on the crate's
   incomplete enum for fields this method never reads.
2. `Box<dyn AppDriver>` and `SurfaceRegistry` (`flowproof-driver`) each
   manually delegate every `AppDriver` trait method to their inner driver -
   both already carry a comment warning that a method missing from that
   list silently falls back to the trait's DEFAULT (`Ok(None)`) instead of
   erroring. `a11y_hint` was missing from both, caught only by testing
   through `flowproof_cli::driver_for("web")` rather than the driver method
   in isolation.
3. `UiaSelector` initially reused `control_type` for the a11y role (matching
   the design note's stated plan) - wrong, caught before it shipped: the
   structural tier already sets `control_type`+`name` together for web steps
   too (UIA-style role names like `"Button"`), so a structural selector's
   payload would have looked like an a11y one to the web adapter's locator
   dispatch and been misrouted into accessibility-tree lookup with the wrong
   casing (`"Button"` vs the real `"button"`). Fixed by giving `a11y` its own
   `role` field instead of overloading an existing one - caught by re-reading
   `flowproof-replay`'s existing `NativeId | Structural` conversion arm
   before wiring the new one in, not by a test catching a live collision.

**H1 is now closed end to end, not just its foundation**: a real, honest
proof that a `native_id` regenerating (the exact failure H1 describes) no
longer breaks a recorded flow. `flowproof-cli/tests/a11y_selector_e2e.rs`'s
`a_renamed_native_id_still_replays_via_the_a11y_rung` records against the
real fixture, then overwrites the SAME url's content (matching a real app
redeploy, not a different environment) so the button's id changes while its
visible text - and so its accessible name - stays put. `native_id`
(css `#greet`) is dead on arrival; the full replay still passes, and the
report's `selector_tier` for that step is `"a11y"`, not a degraded fallback
through `structural`/`text_anchor`. Combined with the capture-side tests
(`a11y_capture.rs`'s two tests, `a11y_selector_e2e.rs`'s first test) - four
tests total, all against live Chromium, `FLOWPROOF_E2E=1` required, each
seen failing for the right reason at least once before being fixed.

**What this does NOT yet establish**: whether this actually moves FAA on
real specs, at scale, against the real Fiori system. That needs the harness
(Phase 2, still unbuilt) and a real round (the acceptance gate, still
unattempted) - a fixed mechanism proven correct on one fixture is necessary,
not sufficient, for the brief's actual goal.

## Implementation status: H2/H5 closed - network-idle settle, generically

`WebAppDriver` gained a CDP `Network` event listener (registered at launch,
reset to zero each launch): `Network.requestWillBeSent` increments an
in-flight counter, `Network.loadingFinished`/`Network.loadingFailed`
decrement it (saturating at zero, so a response for a request the listener
missed the start of can't drive the counter permanently negative).
`settled_scene()` now requires `network_idle()` (the counter reads zero)
alongside the existing `ready` + shape-agreement checks.

**The budget itself is signal-driven, not raised unconditionally** - this is
the distinction ground rule 3 cares about. Two round budgets exist:
`SCENE_SETTLE_ROUNDS` (20, ~2s - unchanged from before) for a page NEVER
observed busy, and `SCENE_SETTLE_BUSY_ROUNDS` (300, ~30s) that only applies
once some round in THIS call actually saw a pending request. A quiet page's
behavior is identical to before this commit; a busy one now gets real
patience instead of a blind 2-second cap, and a page whose network never
truly quiets (websocket, polling widget) still gives up at the (much larger)
ceiling rather than hanging forever - same philosophy as the pre-existing
carousel/ticker bound, just calibrated to a real observed number: a live
login against the actual system took ~13s to render tiles after the DOM
itself reported ready, and the account's own hand-tuned expert spec budgeted
up to 120s as a safety margin.

**Proven at two levels**: two new unit tests on the pure settle logic
(`a_pending_request_is_not_settled_even_when_the_shape_already_agrees` -
shape agrees from the very first reading, but must keep polling until
`network_idle` reports true, which the OLD code had no way to check at all;
`a_request_that_never_finishes_is_still_bounded` - busy forever still gives
up at the busy ceiling) and one live-Chromium test
(`network_idle_e2e.rs::scene_waits_for_a_real_delayed_fetch_before_settling`)
against a real 2-second delayed HTTP response: the scene read after launch
contains the content that only exists after the fetch resolves, and the
total elapsed time actually spans the real delay - a real CDP listener
proven against a real page, not only the synthetic unit tests.

**Not Fiori-specific, deliberately** - same reasoning as H3b over H3: this is
a generic CDP capability, available for every `app: web` target, not
`sap.ui.test.autowaiter` or anything tied to UI5's own test infrastructure
(which H3's probe already showed is fragile outside its intended harness).

## What would need to be true to resume

- Free space at or above 15 GB (or the user says a build up to N GB is fine
  even below that, overriding the brief's own gate — that's their call, not
  one to infer).
- Then Phase 0a (build check) becomes the first real step, followed by 0b/0c
  as originally scoped, with the `examples/fiori/` collision decision above
  applied.

## Phase 2 — the harness exists now and ran for real, twice

`scripts/fiori-eval.py` is built: it drives `flowproof record --no-repair`
then `flowproof run` three times per spec, refuses to touch a failing spec's
file, and writes a scoreboard JSON (FAA numerator/denominator, per-spec
record/run detail, sha256 of the spec at time of scoring). It was pointed at
a 2-spec validation corpus (`evals/fiori/dev/`) — not the Phase 0c corpus
(12+ specs, negative controls, holdout), which does not exist yet — as a
smoke test of the harness itself, and to see, for the first time, whether
tonight's H1/H2/H5 fixes actually move a real number.

**First run: FAA 0/2, but one bug was the harness's own.**
`probe-real-info-record-lookup.flow.yaml` failed at `record` with
`secret ${PURCHASING_ORG} is not set in the environment` — not a flowproof
defect. The harness never passed `--vars evals/fiori/dev/values.yaml` to
`record`/`run`, so every `${VAR}` sourced from that file (not `.env`) was
simply absent. Root cause: `flowproof record --var KEY=VALUE` is an
*override*, not a base source — `--vars <file>` is the one that loads a
values file, and the harness only ever built the former. Fixed by having
`score_spec` look for a `values.yaml` next to the spec (this repo's existing
convention — see `examples/fiori/*.flow.yaml` headers) and pass
`--vars <that file>` to both `record` and every `run` call.

**Second run, after the fix: FAA still 0/2 — for two real, different, honest
reasons, not the same bug twice.**

1. `probe-real-info-record-lookup.flow.yaml` now records past the missing-
   secret error and fails on its own merits: the spec's final assertion
   (`page shows General Data`) does not hold — recording captured the page
   still on `Change Info Record: Initial Screen`, meaning the search step
   never actually reached a result. This is exactly what FAA is designed to
   catch: a naive first-attempt spec whose author (human or model) wrote an
   assertion for a screen the flow doesn't reach. Nothing to fix in
   flowproof here — the spec itself needs rework, which is the harness
   working as intended, not a flowproof bug.
2. `short-01-login-smoke.flow.yaml` (the known-good smoke spec) recorded
   fine and passed run 1, then failed run 2 on
   `Wait until page shows Home within 60s` —
   `expected element text 'Home', got '<element not found>'`. This is a
   **new, unresolved, genuinely intermittent finding**, not the same failure
   as the pre-fix baseline (which failed on run *3*, not run 2 — ruling out
   a fixed off-by-one and pointing at real non-determinism). The captured
   `debug/dom.html` for both the pre-fix and post-fix failures shows the
   real launchpad shell genuinely loaded (`spacesMyhome: true`, a `"My
   Home"` Spaces page assigned to this user) — this tenant has the Spaces
   feature on, and its landing page's title is `"My Home"`, not the plain
   `"Home"` the smoke spec asserts on. The passing runs presumably matched a
   different, still-present `"Home"` labelled element (a shell icon or
   breadcrumb) that this session did not track down. **Not claiming this is
   understood** — only that it reproduces, is captured with real evidence,
   and is a legitimate H2-adjacent gap the settle-mechanism fix from earlier
   tonight does not by itself close. Worth a dedicated look, not a guess
   fixed under time pressure here.

Both scoreboards are committed (`evals/fiori/20260914T094750Z.json`
pre-fix, `evals/fiori/20260914T095421Z.json` post-fix) so the harness bug
and its fix are each backed by a real, reproducible before/after, not
described from memory.

**Still true, unchanged from Section 6 of `RELIABILITY.md`**: this is a
2-spec validation run of the harness machinery, not a Phase 0c corpus, not a
gate attempt, and not an FAA baseline anyone should read as "flowproof's
real number on Fiori." It is evidence the harness itself works and already
surfaced one real spec-authoring problem and one real intermittent replay
gap — which is what a working harness is supposed to do on its first real
data.

### The `short-01-login-smoke` intermittency: root cause found, and it was
### simpler than the "Home vs My Home" hypothesis above

The paragraph above guessed the failure might be semantic — this tenant's
Spaces feature titling the landing page `"My Home"`, not the plain `"Home"`
the spec asserts on. That guess was never confirmed, and a closer look at
the actual grounding shows a simpler, fully explaining cause instead:
**the recorder's own timeout for this step was wrong, cut to a sixth of
what the spec asked for.**

`s0004` ("Wait until page shows Home within 60s") is a **model**-authored
step (the harness records with the default `Author::Auto`, and with a model
configured every plain natural-language step routes through the LLM
grounding path — `crates/flowproof-agent/src/recorder.rs`'s `use_model`
branch — never the deterministic rules grammar). The model-grounding code
path that turns an `assert_text` action into a `ResolvedAction::AssertText`
(`crates/flowproof-agent/src/author.rs`, the `"assert_text" =>` arm) hard-
coded `timeout_ms: crate::rules::ASSERT_TIMEOUT_MS` — **10 seconds,
always** — regardless of what the step's own natural-language text said.
The deterministic rules grammar (`rules.rs`) has always parsed an explicit
`within <N>s` (or defaulted to a 60-second `WAIT_STEP_TIMEOUT_MS` for
`wait until` phrasing specifically) — but that logic was never reused by
the model path, because a model-authored `assert_text` action carries no
timeout field to begin with (`AuthoredAction` in `author.rs` has none, by
design — a number the model invented would be no more trustworthy than one
it forgot). The recorded trace confirms it exactly: `s0004` was captured
with `"timeout_ms":10000`, silently discarding the spec's own "within 60s".
`s0005` (the very next step, `"page shows Home"`, no `wait until`, no
explicit `within`) was *correctly* `10000` by the same grammar's own
default for a plain assert — so the two steps side by side look like a
1/6th-of-what-was-asked bug on one line and correct behavior on the next.

This is a **universal flowproof bug**, not specific to this spec or to
Fiori: any spec, on any target, whose plain-language step includes an
explicit `within Ns` — recorded with a model configured — had that number
silently replaced by 10 seconds. It explains both prior failures without
needing any Spaces-related hypothesis: run 3 of the first harness run and
run 2 of the second each failed elsewhere, but always at *some* point past
10 seconds, well within a real 60-second budget for this shell element to
render.

**Fixed** in `crates/flowproof-agent/src/rules.rs` (a new
`pub(crate) fn timeout_ms_for_intent(intent: &str) -> u64`, reusing the
existing `split_within` parser and the same `wait until` vs. plain-assert
default split the deterministic grammar already had) and
`crates/flowproof-agent/src/author.rs` (the `assert_text` grounding arm now
calls it against the step's own `intent` instead of hardcoding
`ASSERT_TIMEOUT_MS`). Two new regression tests prove it:
`model_authored_wait_honors_an_explicit_within_clause` (an explicit
`within 60s` on a model-authored step grounds to `60_000`, not `10_000`)
and `model_authored_wait_until_defaults_to_the_long_timeout` (a bare
`wait until` with no explicit qualifier still gets the long default via
the model path, matching what the deterministic grammar already gave that
phrasing). All 377 `flowproof-agent` lib tests pass; `fmt`/`clippy` clean.

**Re-verified live, not just unit-tested.** Re-recording
`short-01-login-smoke.flow.yaml` against the real system now captures
`s0004` with `"timeout_ms":60000`, and a 3-run replay against the real
system passed all three times — run 2 of that replay took **18.7 seconds**
on `s0004` (well past the old 10-second budget, comfortably inside the new
60-second one) and still passed. That single data point is close to direct
proof: this exact scenario — the shell taking noticeably longer than 10s,
well under 60s — is what silently failed before, and the fix closes it.

**What this does and does not settle**: the timeout-honoring bug is
confirmed and fixed, and it fully explains the two failures actually
observed so far — there is no remaining evidence for a distinct "Home vs
My Home" semantic problem; that was an untested hypothesis, now superseded
by a cause that explains the same data more simply. It is not proof no
Spaces-related issue could ever occur on a different assertion phrased
differently — only that it was not needed to explain what was actually
seen here.

## Phase 0c — the dev corpus, written (not yet scored)

14 `.flow.yaml` specs now exist in `evals/fiori/dev/` — 4 short (3–6
steps), 5 medium (10–20 steps), 3 long (30+ steps, crossing 4+ real
screens, each including a value-help dialog), and 2 negative controls —
matching the brief's Phase 0c composition rule (4/5/3, plus at least 2
negative controls). Full composition, the read-only/single-app-family
rationale, and which steps are unverified guesses versus proven-live text
are documented in `evals/fiori/dev/README.md` rather than repeated here.

**Real system, not the mock fixture** — consistent with the pivot recorded
earlier in this log. **Read-only, everywhere** — the corpus works almost
entirely within the one app family (Purchasing Info Records, reached
directly from Home or through the "PTP Process Area BU Apps" group tile)
because that is the only family with confirmed-safe navigation as of
tonight; variety comes from varying navigation path and interaction
pattern (direct tile vs. group, one value-help field vs. two, a wrong
search before the real one, cross-app confirmation, out-of-band OData
checks at different filter shapes), not from different business data —
there remains exactly one confirmed-real test record.

`probe-real-info-record-lookup.flow.yaml` (the Phase 1 naive-first-attempt
diagnostic, already known broken and already written up above) was moved
into `_probes/` alongside the other historical diagnostics — it was never
a considered Phase 0c corpus member, and leaving a known-broken file
sitting in the scored corpus directory would be misleading.

**Written now, scored in a follow-up** — the user's own explicit choice,
given the real-system time cost of authoring across 14 specs (record + up
to 3 runs each, several of them 30+ steps). No baseline FAA exists for
this corpus yet, and per the brief's Verification §2, the per-spec sha256
freeze happens at that first scoring run, not before. Several steps
(value-help mechanics, the exact wording of two of the PTP group's
sub-tiles, whether "Navigate to `<url>`" cleanly returns to the launchpad
mid-flow from inside an app) are honest, flagged guesses — consistent with
the existing real examples' own TODOs for the same kind of ambiguity, and
consistent with the brief's own instruction to write specs before seeing
why they break. Some of these are expected to need correction on their
first live `record` attempt; that is the FAA experiment working, not a
mistake in how the corpus was written.

**Not done**: `evals/fiori/holdout/` remains empty. Per the brief, holdout
specs are hand-written by the person running the brief, in their own
words, never inspected or tuned against during the fix loop — writing them
myself would defeat the point of a holdout set entirely.

## Branch continuity: a colleague's automated PR-review fixes landed on the
## same branch, mid-session

Partway through this session, a colleague's automated PR-review-and-fix
workflow (real commits, author Amin Chirazi) merged onto the same
`fiori-reliability/2026-09-14` branch this session had been treating as its
own, then that merge landed on `main` and shipped as v0.23.0 - none of it
in conflict with this session's own work (`d28265d`, the last commit this
session had pushed, is an ancestor of `main`'s tip), but a real collision
of two independent workstreams sharing one branch name. On the user's
instruction, this session opened a fresh branch,
`fiori-reliability/2026-09-14-v2`, based on `origin/main`, and cherry-picked
its own pending commits onto it - both applied with zero conflicts, full
test suites green. Everything from here lives on that branch. Amin's own
merged work (a real, independent-of-this-session fix for the redirect
double-counting bug in `network_inflight`, plus xpath soft-hyphen
normalization and an `ancestor_role`/ambiguous-match tightening in the a11y
selector) is now the floor this session's own fixes build on, not
something this session authored or should take credit for.

## Real-system checkpoint #2: root-causing the corrected corpus's own
## remaining failures

With the leak fixed and the corpus's PTP-tile mistake corrected, a clean
re-baseline (`evals/fiori/20260914T185311Z.json`, taken after confirming
network connectivity - the corporate host had also been down for about two
hours mid-session, an unrelated VPN/routing drop, not an application
issue) scored **FAA 33.33% (4/12)**, the first number free of both earlier
confounds. All four short specs passed, including the two that directly
exercise the corrected tiles - clean confirmation the corpus fix worked.
Negative controls held.

The user then authorized fully autonomous continuation overnight: keep
polling for the corporate system (which started returning `503 Service
Unavailable` at the HTTP level partway through - a third, independent
infrastructure interruption tonight, after the VPN drop and an Anthropic
API `529 Overloaded`), work through the remaining findings without asking,
document every decision, and use judgment about whether a fix generalizes
rather than patching one spec. Three findings were root-caused and fixed
this way, all live-verified against real evidence (a real trace, a real
error message) even though the corporate system itself was unreachable for
the fixing work - only the final live re-confirmation is still pending.

**Finding: `medium-05`'s `rule_step` grammar cannot follow its own prompt's
instruction for a framed/scoped target.** The system prompt tells the
model to "copy listed target tokens into quoted targets" for `rule_step`.
A framed element's own scene token is a compound string containing its own
embedded quote marks (`framed:"<frame>" > <inner>`), and `rule_step`'s
quoting is naive - `quoted_label` (rules.rs) takes the substring up to the
FIRST `"`, not a balanced pair. Pasting the real token verbatim therefore
mis-parses into garbage, and dropping the scope clause instead (which is
what the actual model reply for this spec did) resolves to an unscoped
target the flat top-level scene never lists. This is not one model's
mistake or one spec's bad luck: it is a structural incompatibility between
the prompt's generic advice and the grammar's quoting, for every
framed/scoped token, universally. **Fixed**: the prompt now carves out an
explicit exception - use `scope.inner` in quotes plus the natural clause
the deterministic grammar already parses correctly (`in the iframe
"<frame>"` / `in the item containing "<anchor>"`), with a worked example.
Two new tests in `crates/flowproof-agent/src/author.rs` prove the
mechanism the new guidance points to actually resolves correctly end to
end, and that pasting the raw compound token fails loudly rather than
silently mis-resolving - a live model's own phrasing choice can't be
asserted by a scripted-backend test, so that is the honest limit of what
these tests claim to prove until a live re-record confirms the model
actually follows the new instruction in practice.

**Finding: `probe_frame` had no transport-fault retry at all.**
`medium-02`'s real failure - "driver transport fault: probing an iframe:
Unable to make method calls because underlying connection is closed" -
happened on a genuinely clean run (network confirmed, leak fixed, no
resource contention). `is_transport_fault` already recognized this exact
error string, and `with_element`-routed calls already get one automatic
retry on it, but `probe_frame`'s CDP call talks to the tab directly and
had never been wired into that retry at all - any transient CDP hiccup
during an iframe probe killed the whole flow immediately, with zero chance
to recover, while the identical hiccup during an element-scoped call
already recovered fine. **Fixed**: extracted the retry decision into
`retry_once_on_transport_fault`, a small pure function `probe_frame` now
uses. Deliberately did NOT touch `with_element`'s own inline version - far
more heavily relied upon, and this session had no live-system access
available to validate a change there tonight, so the fix stayed scoped to
the one call site with actual evidence behind it. Three new unit tests
prove the retry logic itself (recovers once, gives up after one retry, and
crucially never retries a non-transport error).

**Finding, found only because the fix above was tested properly: every
e2e test that touches the web adapter was independently leaking its own
Chrome process, and fixing that with a naive per-test guard immediately
introduced a NEW race.** Adding tests for the `probe_frame` fix meant
constructing real `WebAppDriver`s, which surfaced that no test file ever
called `shutdown_shared_browser()` - unlike `flowproof-cli`'s own
invocations (fixed earlier tonight), so every e2e run leaked a full Chrome
tree and profile dir independent of the CLI leak already closed (18
orphaned processes found after one ordinary test run). Exporting
`SharedBrowserGuard` from `flowproof-adapters` and adding it to each
affected test file looked like the fix - until running the new tests
repeatedly reproduced a second, genuinely new bug: `cargo test`'s default
parallelism runs `network_idle_e2e`'s two tests concurrently, both sharing
the one process-global browser, and the first test's guard unconditionally
killed it while the second was still mid-launch - the exact "connection is
closed" error, a third independent way of hitting it tonight. **Fixed**:
`SharedBrowserGuard` now reference-counts; only the last live guard
actually shuts anything down. Confirmed by running the previously-racing
test three consecutive times, clean each time, plus a new unit test proving
a held sibling guard keeps the count above zero. This is the kind of
self-correction the discipline of "run the fix, don't assume it from a
diff" is supposed to catch, and it did.

**Still open, honestly**: the value-help "accepted a different value after
commit" failure (hits `long-01`, `long-03`, `medium-04`, and a differently-
shaped variant in `long-02`) is the highest-count remaining failure and is
NOT yet root-caused - a live-system investigation is needed and the
corporate host has been unreachable for it all night. `medium-02`'s own
replay-time connection drop may or may not be closed by the `probe_frame`
fix above (it fires from `type_text`'s framed path too, a different call
site not yet checked for the same gap). `medium-03`'s "the title element
exists but shows ''" was checked against the actual polling loop
(`crates/flowproof-agent/src/recorder.rs`'s `AssertText` handling) and the
loop itself is correct - it keeps polling `element_exists` + `read_text`
against the real deadline, so this is either a genuine render-time
variance longer than 20s for this specific, previously-unexplored app, or
the recorded selector resolves to a structurally different (permanently
empty) element - which of the two it is cannot be told without a live
re-record, and guessing "just raise the timeout" without that evidence
would be exactly the kind of primary-fix-is-a-timeout-bump the brief
forbids. `long-02`'s "the previous step left a problem behind - the page
reports: Find Objects in Classe[s]" is unexplained; it needs to be seen
live before it can be explained at all.

## Decisions

- **Composed the corpus's variety from navigation path and interaction
  pattern rather than from different business records.** Reason: exactly
  one real record combination (Material `TG10` / Supplier `10300001` /
  Plant `1010` / Purchasing Org `1010`) has been confirmed to exist and
  resolve correctly tonight; inventing plausible-looking alternate business
  data for "variety" would risk specs that fail for the wrong reason (a
  nonexistent record) rather than the reason under test (flowproof's
  authoring reliability). Reversal: if more real records are confirmed
  later, later corpus revisions can add genuine data variety on top of
  this.
- **Kept every corpus spec read-only, including the three long flows that
  the brief's own long-flow language ("crossing multiple views, including
  a dialog and a value help") most naturally suggests a real edit-and-save
  path for.** Reason: the only confirmed-navigable app for a "Change"-style
  flow (`examples/fiori/manage-info-records.flow.yaml`) was itself
  deliberately narrowed to read-only earlier tonight because this
  particular app has no scratch/throwaway record to create — any edit
  permanently steers a real, shared reference record with no undo. The
  user's own choice, asked directly this turn, confirmed staying read-only
  over allowing edits to shared data. Reversal: if a real app with a
  create-your-own-scratch-record path is found later, a future long spec
  could legitimately add a write.
- **Went back and chased the "Home" vs "My Home" finding down after all,
  once the user asked directly whether it was universal or test-specific.**
  Reason for reopening a decision made just above (not chasing it "tonight"):
  the user's question was itself the signal that this was worth a bounded
  look before building anything further on top of it — and the actual root
  cause (the model-authoring path dropping an explicit `within Ns`, detailed
  above) turned out to be both real and universal, not Fiori-specific, and
  cheap to fix once found (two files, ~15 lines, two tests). This is the
  reversal the original decision explicitly left open for. Confirms the
  original caution was right too: guessing a fix without investigating
  first would likely have "fixed" the wrong thing (the assertion text, not
  the timeout) and left the real bug live for every other spec.
- **Opened the harness's first real runs against the live corporate system
  rather than waiting to build the full Phase 0c corpus first.** Reason:
  the user's explicit instruction was to "start the harness" now, on the
  same branch, to show colleagues real progress — a 2-spec smoke run
  proves the harness's mechanics (record → 3x run → scoreboard, `--vars`
  wiring, negative-control inversion logic though untested here since
  neither spec is one) without waiting on the much larger generator/corpus
  work. Reversal: if the user wants the full corpus built next, that is
  unstarted and explicitly flagged as the brief's largest remaining scope.
- **Did not chase the `short-01-login-smoke` run-2 "Home" vs "My Home"
  finding down to a fix tonight.** Reason: H1-H5 already consumed the
  night's live-system investigation budget, the finding was only just
  captured with real evidence, and guessing a fix under time pressure for
  something not yet understood risks exactly the "cheating toward green"
  the brief forbids — patching this one assertion string would fix the
  symptom on this one spec without knowing if the real cause (Spaces-
  dependent shell text) affects every other spec that asserts on shell
  chrome text. Recorded as a new open finding instead. Reversal: if this
  turns out to be a five-minute fix once someone looks at the accessibility
  tree at the moment of failure, it should be picked up next, not deferred
  indefinitely.
- **Halted before Phase 0a/0b/0c.** Rejected: proceeding with a disk-diet
  version of the fixture (e.g., skip the GIF captures, cap trace sizes) to
  route around the threshold. Reason for rejecting: the halt condition is
  about *free space*, not about *this session's own artifact footprint* — the
  build step alone (`cargo build --workspace`, before any fixture or eval
  artifacts exist) is the first thing that would consume meaningful disk, and
  at 6.3 GB free even a normal Rust workspace build is a real risk of hitting
  zero. Trimming *my own* output doesn't address that. Reversal: if the human
  judges 6.3 GB is enough headroom for a `cargo build`, say so explicitly and
  resume from Phase 0a directly — no other change needed to unblock.
- **Did not attempt to identify or clear space in the other APFS containers**,
  even read-only beyond `diskutil apfs list`. Rejected: mounting/inspecting
  them to see if they're actually VM images (as opposed to something more
  mundane) on the theory that confirming would let me safely ignore them.
  Reason: the brief's protected list is unconditional ("if a VM is the only
  thing standing between you and free space, you halt instead") — confirming
  they're VMs doesn't change the outcome, and poking at unfamiliar mounted
  containers overnight, unsupervised, is exactly the kind of thing ground
  rule 6's boundary exists to prevent. Reversal: trivial — a human runs
  `diskutil apfs list` and `diskutil info` on the other containers themselves
  in five minutes.
- **Resumed below the 15GB gate, at the user's explicit direction.** After the
  halt, the user freed some space by hand (6.5GB -> 9.3GB free) and asked
  directly, mid-conversation, to proceed with what's currently available. This
  is different from this session deciding on its own to ignore the halt
  condition (which ground rule 6 forbids): the user is present, aware of the
  15GB figure and the shortfall, and is overriding their own brief's gate for
  their own machine in real time. Proceeding cautiously from here: building
  without `--all-targets` first, checking free space before/after each heavy
  step, and re-halting if it drops into risky territory (a build that starts
  eating the low single digits of GB) rather than assuming 9.3GB is enough
  headroom for the rest of Phase 0. Reversal: if disk pressure reappears, stop
  and report the number again rather than pushing through silently.
- **Pivoted from the mock fixture to the real Fiori system as the primary
  test vehicle, on the user's explicit, live instruction — overriding the
  brief's own ground rule 5** ("do not run hundreds of iterations against the
  real corporate SAP system... the real system is a checkpoint, not a
  workbench"). The user's reasoning, given directly: a mock built by the same
  session doing the fixing is exactly the overfitting risk the brief's own
  holdout-set logic warns about (dev FAA climbs, holdout doesn't, and you
  never find out because you built both). They also clarified that
  `examples/fiori/`'s existing real-launchpad traces took hours of live
  iteration with a coding agent to get working, so their existence is *not*
  evidence Fiori already works reliably - it's evidence of exactly the
  problem this brief exists to fix. Confirmed real credentials
  (`.env`: `FIORI_BASE_URL`/`SAP_USER`/`SAP_PASSWORD`/`SAP_CLIENT`/
  `SAP_LANGUAGE`) and live reachability (`curl` -> HTTP 200) before treating
  this as workable, rather than assuming. **What this session is doing
  instead of a blind override**: staying judicious about write-volume against
  the real system specifically (favoring read-only/idempotent flows, not
  running the full generator-driven Gate A/B/C rounds against production
  without further explicit confirmation of that specific scale), since the
  user authorized "test against real Fiori," not "hammer production with 30
  fresh specs per round with no limit." Reversal: if this turns out to create
  real load or side effects the user didn't anticipate, stop and say so - the
  override was for correctness of measurement, not a blank check on volume.
  The mock fixture built earlier (`examples/fiori/fixture/`) is left in place
  (it already caught one real bug - see its README - and cost is sunk) but is
  no longer the primary vehicle; it may still be useful later for the
  latency/error-injection robustness checks the brief asks for, which the
  real system cannot safely provide on demand.
- **Left `examples/fiori/` untouched**, recorded the collision instead of
  guessing a resolution and building into it. Reason: the brief's own
  assumption about that path was wrong, and picking a new location for a
  fixture that will eventually need real design (routing, MockServer, dialog
  handling) is worth a deliberate choice, not a default taken under the
  pressure of "don't ask." Reversal: whoever resumes this either confirms the
  subdirectory approach above or picks a different path — nothing here is
  load-bearing yet since no files were written.
- **Pushed the branch and opened a draft PR** (#585), overriding ground rule
  1 ("never push") on the user's direct, explicit, in-the-moment instruction
  ("Open a PR already... leave it as draft"), given by the user themselves
  once awake and present - not inferred or assumed. Same category of
  override as the real-system pivot: the ground rule's purpose (don't
  surprise collaborators on a public repo with unreviewed autonomous
  pushes) is satisfied by opening it as a draft rather than a PR asking for
  review, which is exactly what was asked for. Reversal: close the PR (or
  ask to) if it turns out to have been premature; nothing about it forces a
  merge or requests review from anyone.
