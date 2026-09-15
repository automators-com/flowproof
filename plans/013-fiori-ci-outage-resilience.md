---
status: done
---
# Plan 13 — Fiori CI reliability: shared-credential contention and outage resilience

On the night of 2026-09-14 into 2026-09-15, the real corporate Fiori
reference system (configured via the `FIORI_BASE_URL` secret, hosted by a
third-party vendor rather than this project's own infrastructure) went down
with a sustained `503 Service Unavailable` for 8+ hours, hit identically by
manual testing, both scheduled Fiori/SAP GUI CI jobs, and other concurrent
testing happening at the same time. This plan is not about that specific
outage — nothing here can fix a third party's system — it is about the
things inside this repo that make an outage like it worse, harder to
diagnose, or more likely to recur, found while investigating that incident.

## The incident, as evidence for this plan

- `.github/workflows/fiori-e2e.yml` (cron `0 4 * * *`) and
  `.github/workflows/sap-e2e.yml` (cron `0 6 * * *`) both run on the same
  self-hosted `[self-hosted, sap]` runner, staggered two hours apart by a
  deliberate earlier fix (#583).
- Both passed cleanly on 2026-09-14 (04:18 and 06:30 UTC). A real fix (the
  shared-browser process leak — see below) merged to `main` at 20:39:44 UTC
  that evening via PR #585.
- Both failed identically the next morning — `fiori-e2e` at 04:17 UTC,
  `sap-e2e` at 06:24 UTC — on the very first step (typing into the Fiori
  login page), which never appeared: `precondition failed: element did not
  appear within 5000ms`. The outage was already in progress before either
  scheduled run started, and was still ongoing many hours later.
- A clean 15-minute window of zero requests from this session (poller
  stopped entirely, one manual check afterward) still returned 503,
  ruling out this session's own health-checking as a contributing cause.
- `fiori-e2e.yml` aliases its own credential internally
  (`SAP_USER: ${{ secrets.FIORI_USER }}`), and this session's own `.env`
  during manual testing used the same `SAP_USER`/`SAP_PASSWORD` names —
  strong circumstantial evidence that CI and interactive development share
  one SAP user account, not separate ones.
- Separately, this session found and fixed a real, universal bug
  (`crates/flowproof-adapters/src/web.rs`: `shutdown_shared_browser`,
  `SharedBrowserGuard`) where every `flowproof` invocation — including
  every CI run before this fix existed — leaked its browser process and,
  with it, an authenticated Fiori session that was never cleanly closed.
  The CI runner is a persistent, self-hosted machine, not an ephemeral
  one, so any such leak accumulates across days rather than resetting.

None of this proves what actually took the system down on the night in
question — that answer lives with whoever hosts and operates it, not in
this repository. What it does show is a real, fixable gap: this project
has no way to tell the difference between "the target system is down" and
"our own test suite is broken," and multiple automated + manual consumers
share one credential with no coordination between them.

## Problem

1. **CI has no fast-fail path for an unreachable target.** Both workflows
   run their full suite and let individual step timeouts (5s precondition
   waits, 2 retries) accumulate before the job fails — burning the full
   ~30-minute budget and adding load to an already-struggling system on
   every day it happens to be down, rather than detecting "the whole
   launchpad is unreachable" in one cheap request and skipping cleanly.
2. **CI and interactive/manual testing appear to share one SAP user
   account.** SAP systems commonly restrict or flag concurrent logons per
   user ID. Two scheduled CI jobs, a developer's own manual `flowproof
   record`/`run` sessions, and (per this incident) other concurrent testing,
   all under one identity, is exactly the shape that trips those limits or
   exhausts a shared work-process budget — even before considering load
   from outside this team entirely (this is very likely a shared
   reference/demo tenant, not dedicated infrastructure).
3. **A long-lived, self-hosted CI runner has no session hygiene audit.**
   The browser-process leak fixed this session (see Landed work,
   `docs/fiori-reliability/FINDINGS.md`) means the runner may be carrying
   forward stale authenticated sessions from every affected run before the
   fix existed. Merging the fix stops new leaks; it does not clean up ones
   that already happened.

## Decision

This plan lands the repo-side outage gate now and records the human
operational actions without blocking the branch on them:

1. **Add a cheap reachability pre-check as the first step of both
   `fiori-e2e.yml` and `sap-e2e.yml`.** One HTTP request to the launchpad
   URL from `FIORI_BASE_URL` with a short timeout. `sap-e2e.yml` uses the
   same probe because the SAP GUI and Fiori workflows point at the same SAP
   reference system. Healthy HTTP 2xx/3xx responses, plus unauthenticated
   auth-boundary responses (`401`/`403`), continue into the suite. Retryable
   outage signals (`408`, `429`, `5xx`, and transport failures/timeouts)
   post a clear, named skip ("target unreachable, http_code=503") and exit
   successfully rather than burning the full suite and its retries into a
   system that is already down. Unexpected client errors still fail loudly
   instead of masking a bad URL as an outage. This is the only change in this
   plan that would have measurably reduced the cost of the actual incident.
   The skip is not a retry/backoff feature: if the target is unreachable, a
   human can rerun the workflow once the target is healthy again.
2. **One-time hygiene pass on the self-hosted runner**: after the
   process-leak fix lands, someone with access to that machine checks for
   and clears any stray browser processes / stale authenticated sessions
   left over from before the fix existed, rather than assuming the fix
   alone retroactively cleans up prior damage. This is a human runner-access
   task, not a repository change.

## Deferred follow-ups

- **Provision a separate SAP user account for CI** is deliberately kept out
  of this plan. It still appears warranted, but it needs administrative
  access to the hosted SAP/Fiori system and should be filed/tracked as its
  own operational issue with an approved issue body.
- **Merge `fiori-e2e` and `sap-e2e` into one shared concurrency group** is
  also deliberately kept out of this branch. It is worth testing in a
  separate PR once the SAP/Fiori instance is healthy, because a mistake there
  could block this outage-gate PR from merging.

## Scope

Changes to `.github/workflows/fiori-e2e.yml` and `.github/workflows/sap-e2e.yml`
for the reachability pre-check. The one-time runner hygiene pass is recorded
as an operational task because this repository cannot inspect or clean the
self-hosted runner directly.

## Out of scope

- Reducing CI frequency (daily → every-other-day, or alternating which
  workflow runs on which day). Discussed as a secondary lever during the
  incident; not adopted here because it lowers average load without
  addressing the actual mechanism (shared credential, no fast-fail, no
  runner hygiene), and daily coverage has its own separate value this plan
  does not weigh against the outage risk.
- Anything on the hosting vendor's own infrastructure — the Web
  Dispatcher / application-server work-process capacity that most plausibly
  actually failed. Outside this repository's control; the action there is
  a direct incident report to that vendor, not a code change.
- Re-litigating the two workflows' existing staggered schedule (#583) in
  this branch. Shared concurrency is a separate PR so it can be tested
  independently.
- Building any kind of general-purpose "is the target system up" health
  dashboard or alerting beyond the one pre-check step each workflow needs.
- Workflow-level automatic retry/backoff. If `FIORI_BASE_URL` is unreachable,
  the workflow skips cleanly; the retry is a later manual dispatch after the
  target comes back.

## Tests

- The reachability pre-check step should be exercised against both a
  healthy response and a `503`/timeout by dispatching the workflows against
  the real runner/secret configuration. There is no existing local
  `act`/workflow test harness in this repo; the local check is YAML parsing
  plus whitespace validation.
- No new Rust/Python test surface: this plan is entirely GitHub Actions
  YAML and a documented operational task, not application code.

## Resolved questions

- `sap-e2e.yml` should use `FIORI_BASE_URL` for its pre-check because it
  targets the same SAP system as the Fiori workflow.
- No vendor status-page/contact/SLA workflow is needed at this stage.
- CI credential separation is real follow-up work, but not important to
  settle inside this plan.

## Landed

- `.github/workflows/fiori-e2e.yml` now starts with a PowerShell
  `FIORI_BASE_URL` reachability check. Healthy HTTP 2xx/3xx responses allow
  checkout, build, replay, and diagnostics upload to continue, as do
  unauthenticated `401`/`403` auth-boundary responses. Retryable outage
  responses, including `503` and timeouts, write a GitHub notice and step
  summary, set `reachable=false`, and skip the rest of the live Fiori job
  successfully. Unexpected client errors fail the job.
- `.github/workflows/sap-e2e.yml` now runs the same `FIORI_BASE_URL`
  reachability check before checkout, build, SAP GUI bootstrap, tests,
  replay, diagnostics upload, and reset. When the shared target is down, the
  SAP GUI job exits cleanly without opening or touching the SAP session.
