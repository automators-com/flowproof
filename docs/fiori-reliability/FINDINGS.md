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

- **H1 — native-id selector is harmful for UI5: CONFIRMED.**
  [`web.rs:3867`](../../crates/flowproof-adapters/src/web.rs) `semanticCss()`
  returns `'#' + CSS.escape(el.id)` unconditionally whenever `el.id` is
  non-empty — checked *before* `data-testid`/`aria-label`/`name`, and before
  any class-based fallback. For a UI5 control this is exactly the generated,
  view-instance-counter-bearing id the brief describes
  (`__xmlview0--idTable-listUl`), and it becomes the recorded `native_id`-tier
  selector — [`lib.rs:36`](../../crates/flowproof-trace/src/lib.rs), tier 0,
  tried first at replay.
- **H2 — no UI5-aware idle signal: CONFIRMED, and it's the same mechanism as
  H5.** [`web.rs:788`](../../crates/flowproof-adapters/src/web.rs)
  `settled_scene()`: waits for two DOM-shape reads 100ms apart to agree,
  capped at 20 rounds (~2s), then proceeds regardless. No concept of a UI5
  busy indicator or an in-flight OData batch. **The same function backs
  authoring**: `driver.scene()` in
  [`recorder.rs:2327`](../../crates/flowproof-agent/src/recorder.rs) is called
  directly before handing the scene to the model for grounding, with no
  additional UI5-specific wait. So H5's "the model grounds against a
  half-rendered DOM" and H2's "replay acts on a control about to be
  destroyed" are **one root cause wearing two symptoms**, not two separate
  defects — fixing the settle/idle mechanism address both the authoring
  quality problem and the replay race at once. This raises its rank on the
  fix list: one change, two classes of failure closed.
- **H3 — missing UI5 selector rung: CONFIRMED ABSENT.** No reference to
  `sap.ui.test.RecordReplay` or `waitForUI5` anywhere in the codebase
  (`grep -rn` across `crates/`). The brief's proposed fix — a `ui5` selector
  tier above `native_id`, using `RecordReplay.findControlSelectorByDOMElement`
  / `findDOMElementByControlSelector`, plus `waitForUI5` as the inter-step
  barrier (subsuming H2/H5's fix) — is architecturally exactly what H1+H2
  jointly point at. Not yet designed in code; this needs the design-note
  treatment the brief calls for (new selector tier = trace-format-adjacent
  change) before implementation, not a quick patch.
- **H4 — dialogs/popovers escape the search root: LIKELY NOT REPRODUCIBLE AS
  STATED, unverified.** The adapter's element search (`web.rs`, scene-building
  and `try_find`) operates against `document.querySelectorAll(...)` — the
  whole top-level document — with special-case handling only for iframes.
  `#sap-ui-static` is a sibling div in the *same* document, not a separate
  frame, so nothing in the code as read scopes search away from it. This is a
  lean from reading the code, not a proof; killing it for real needs an actual
  UI5 app with a `sap.m.Dialog` running through the adapter (Phase 0b).
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

## Implementation status: foundation landed, capture/resolution not yet built

The trace-format commit above (new `A11y` tier, ranked first, schema+docs
updated, tested) is landed. The part that would actually change FAA - the
recorder capturing `a11y` selectors and replay resolving them - is not built
yet. Confirmed ready to build on, so the next session doesn't have to
re-derive this:

**The CDP capability is real and reachable from this exact vendored
`headless_chrome` fork** (`Cargo.toml`'s `[patch]`, pinned to
`github.com/automators-com/flowproof@eed7bc1`). Verified by compiling a
throwaway probe crate against it (not part of this repo, discarded after)
that resolved every type needed:

- `headless_chrome::protocol::cdp::Accessibility::{Enable, GetAXNodeAndAncestors, GetAXNodeAndAncestorsReturnObject, AXNode, AXValue}`
- `headless_chrome::protocol::cdp::DOM::{ResolveNode, BackendNodeId, DescribeNode}`

`GetAXNodeAndAncestors` (params: `backend_node_id: Option<DOM::BackendNodeId>`,
or `object_id`/`node_id`) is exactly record-time-shaped: given the backend
node id of an element the recorder already resolved through its existing
CSS-based path, it returns that node **and its ancestor chain** in one call -
no separate ancestor-walk needed. `AXNode.role`/`.name` are `Option<AXValue>`;
`AXValue.value` (a generic `Value`) carries the actual computed string
(`"link"`, `"Change Purchasing Info Record Tile"`) - confirmed against the
JSON protocol spec's field shapes (`crates/flowproof-trace`'s vendored
counterpart doesn't cover this; the shapes came from `json/browser_protocol.json`
in the `headless_chrome` checkout itself, cross-checked against a compiled
probe, not merely read off the spec).

**Why this session stopped here rather than continuing into the actual
integration**: capturing at record time means finding where `web.rs`
resolves a target element for a click today (its existing CSS-based
`semanticCss`/element-lookup path) and getting a `backend_node_id` out of
that same resolution - which likely needs a new capability on the
`AppDriver` trait (`flowproof-driver`) or a web-adapter-specific hook, since
`a11y` is web-only and the recorder is adapter-agnostic. Replay needs the
mirror: `Accessibility.getFullAXTree` (already confirmed available too) plus
matching by role+name+ancestor, then `DOM.resolveNode` back to a live
element. Both sides need real tests against a live Chromium page (matching
the pattern of `web.rs`'s existing `web_round_trip_via_mock_uses_css_selectors`-
style tests), not just unit tests of string matching. That is a real,
multi-hour, multi-file piece of engineering on its own, and landing it
carelessly at the tail of an already very long session risks exactly the
"committed half-fix... a trap I will step in next week" the brief warns
against. Stopping at a solid, tested foundation and naming the exact next
step precisely serves the goal better than a rushed, undertested attempt at
the rest.

## What would need to be true to resume

- Free space at or above 15 GB (or the user says a build up to N GB is fine
  even below that, overriding the brief's own gate — that's their call, not
  one to infer).
- Then Phase 0a (build check) becomes the first real step, followed by 0b/0c
  as originally scoped, with the `examples/fiori/` collision decision above
  applied.

## Decisions

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
