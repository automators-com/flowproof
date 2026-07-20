# flowproof alongside your existing suite

flowproof exists to reach applications that browser automation cannot —
SAP GUI, Oracle Forms, Citrix, and any Windows desktop app — and to let
one deterministic engine replay tests that an AI authored once. This page
is honest about where it fits, when NOT to adopt it, and what a recent
external evaluation got right.

## When flowproof is the right tool

- **You test a desktop or legacy app.** SAP GUI Scripting, Windows UI
  Automation, and pixels-only (Citrix/RDP) targets are first-class. This
  is the case Playwright and Cypress structurally cannot cover.
- **You want AI to author and heal, but a deterministic engine to run.**
  Record once with a model in the loop; replay forever with zero model
  calls. Drift proposes a reviewable diff, not a silent fix.
- **Business-critical flows need out-of-band truth.** `assert_api` /
  `assert_sql` verify the row or response a UI action should have
  produced. `app: api` runs those with no browser at all.
- **Non-engineers need to read the tests.** A product owner can read a
  flow file and tell you whether it tests the right thing.

## When to keep what you have

**If every app you test is a browser app, and your current tool tests it
well, flowproof is not an upgrade for that suite.** An external team
measured exactly this against a Next.js monorepo and recommended staying
on Playwright — a fair call for their situation. Adopting a second
test-automation framework means two runners and two places to look when
something breaks;
that cost is only worth paying for coverage you don't already have.

## What that evaluation found, and where it stands now

The evaluation (flowproof 0.1.0) surfaced real defects. Most are fixed;
the honest ones about fit remain true.

| Finding | Status |
|---|---|
| Native `<select>` change never committed (their blocker) | **Fixed** — `Select … from the "…" field` sets the value through the native setter and fires `input`+`change` (React-safe) |
| Text anchors fused sibling text ("ETE2E Test Runner's Team") | **Fixed** — anchors match an element's own text before its subtree |
| Grammar undocumented, had to be read from source | **Fixed** — [docs/authoring.md](authoring.md) is the complete grammar, kept honest by a test that parses every example |
| `Navigate to`, `the page shows`, `is disabled`, one-step replace rejected | **Fixed** — all accepted now |
| Fresh Chromium per flow (4.3s floor) | **Fixed** — one browser per run, isolated context per flow |
| No retries, no API-only flows | **Fixed** — `run --retries N`; `app: api` for UI-less suites |
| Raw driver faults surfaced ("connection is closed") | **Improved** — web driver retries transient CDP faults once |
| Network mocking (SSE, Stripe interception) | Not yet — on the roadmap |
| Re-record on every UI change | Partly — `heal` proposes diffs; incremental re-record is planned |
| Adds Python + Rust wheel to a JS monorepo | An npm distribution is planned; the wheel stays the primary SDK |
| Needs a harness for seed/cleanup sequencing | A suite manifest with before/after hooks is planned |

## What the evaluation validated (and we kept)

Zero-LLM deterministic replay, session seeding with `${VAR}` indirection,
auto-waiting assertions with the bound recorded in the trace, and
out-of-band API/SQL assertions were all called out as genuinely good —
and none of them changed.

## Two things worth carrying into any suite

The evaluation raised two points that are framework-independent and worth
acting on regardless of tool:

1. **Provisioning should retry.** A single transient 502 from a tired dev
   server killed an entire project run because global setup did not retry.
   That fragility lives in the harness, not the framework.
2. **Out-of-band API assertions are worth adopting** even in a
   browser-only suite — verifying the posted record, not just the pixel.
