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
- **Left `examples/fiori/` untouched**, recorded the collision instead of
  guessing a resolution and building into it. Reason: the brief's own
  assumption about that path was wrong, and picking a new location for a
  fixture that will eventually need real design (routing, MockServer, dialog
  handling) is worth a deliberate choice, not a default taken under the
  pressure of "don't ask." Reversal: whoever resumes this either confirms the
  subdirectory approach above or picks a different path — nothing here is
  load-bearing yet since no files were written.
