# Fiori reliability — overnight run report

**The goal was not reached. Say that plainly, per the brief's own honesty
clause: this run halted in Phase 0, before a baseline FAA existed, before any
production code was touched, and before Gate A was even attempted.**

## 1. Gate status

No gate attempted. No rounds run. There is no round history table because no
round exists.

## 2. The numbers

None. No baseline FAA was established (Phase 0 didn't complete), so there is
nothing to report for dev, holdout, default latency, injected latency, or
determinism variance. Reporting a number here would be fabrication.

## 3. Control results

Not applicable — no harness was built, so there were no negative controls, no
mutation checks, no corpus hashes, and no generator to freeze.

## 4. Hypothesis verdicts

Unresolved, all of them (H1–H5). Phase 1 (the 2-hour investigation reading
`docs/trace-format.md`, `docs/authoring.md`, `docs/design.md`, and the
adapter/trace/agent source) was never reached.

## 5. What changed

Nothing in production code. Two files added under `docs/fiori-reliability/`
(this report and `FINDINGS.md`) on branch `fiori-reliability/2026-09-14`.
Nothing pushed.

## 6. What is still broken

Unknown — not investigated. The taxonomy this section is supposed to report
does not exist yet.

## 7. What you should not trust

Nothing was built, so there's no speculative result to distrust. The one
thing worth flagging: this session ran the ground-rule-2 overlap check (open
issues/PRs) and it came back clean, but it did **not** do a deep read of
merged history beyond the last ~20 commits — there could be closed PRs or
recently-merged work more relevant than what a shallow `git log` surfaced.
Treat the "no conflict" finding in `FINDINGS.md` as a quick check, not an
exhaustive one.

## 8. Reproduce it / resume it

There's nothing to regenerate — no fixture, no seed, no round. To resume:

1. Get free disk space on this machine above 15 GB (or explicitly waive that
   gate), per `FINDINGS.md` → "Disk space".
2. Re-read the brief and `FINDINGS.md` (the collision with the existing
   `examples/fiori/` real-launchpad examples, and the charter-milestone
   mismatch, both need a decision before Phase 0b starts).
3. Start at Phase 0a (build check) exactly as the brief specifies.

## Why this halted rather than pushing through

The brief lists an explicit halt condition: *"free disk space stays below
15 GB after the cleanup allowlist is exhausted."* That's the state of this
machine right now (6.3 GB free after running every allowlisted cleanup step),
and the cause is disk usage at the OS/container level (other APFS containers
that are very likely mounted disk images, explicitly protected; OS update
snapshots, not on the allowlist) — not anything reclaimable from user-space
caches. The brief is also explicit that ground rule 6's autonomy is about
*decisions*, not *permissions*: it authorizes deciding without asking, it does
not authorize deciding *past* a halt condition on the theory that the goal
matters more. A partial, disk-conscious version of Phase 0b would still start
with `cargo build --workspace`, which is the actual risk at 6.3 GB free, so
trimming this session's own output wouldn't have fixed the underlying problem.

Full detail, including the disk-space evidence and every decision made before
halting, is in `docs/fiori-reliability/FINDINGS.md` on this branch.
