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
- **Left `examples/fiori/` untouched**, recorded the collision instead of
  guessing a resolution and building into it. Reason: the brief's own
  assumption about that path was wrong, and picking a new location for a
  fixture that will eventually need real design (routing, MockServer, dialog
  handling) is worth a deliberate choice, not a default taken under the
  pressure of "don't ask." Reversal: whoever resumes this either confirms the
  subdirectory approach above or picks a different path — nothing here is
  load-bearing yet since no files were written.
