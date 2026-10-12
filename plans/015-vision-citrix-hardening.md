---
status: in-progress
---
# Plan 15 — `app: vision` on Citrix: making the pixels-only adapter survive a remote display

`app: vision` exists for one situation: an application published through
Citrix or RDP, where flowproof sees nothing but the client window's pixels and
can act only by injecting the input a user's own mouse and keyboard would
produce. A team tried it against a Citrix VM and it did not hold up. This plan
is the working record of finding out why and fixing it in the engine — not in
individual flows.

## How the work is run

- **Phase 1 — this machine.** Flows run as `app: vision` against an ordinary
  local window. Anything that breaks here is an engine defect (OCR, anchor
  resolution, input, timing) that would break under Citrix too.
- **Phase 2 — a VM, flowproof outside it.** The same flows pointed at a VM
  viewer window, which is the client topology. Only this reproduces what is
  specific to a remote display: scan-code-only keyboard forwarding, scaling
  between client and session, compressed pixels, repaint latency.

A clean Phase 1 says the engine is sound; it says nothing yet about Citrix.

Every fix ships with the test that proves it, per `CLAUDE.md`. Fixes are kept
under the 400-line review limit, one cause per change.

## The test machine

Recorded because several findings depend on it. The development machine is
itself virtualised (CPU reports `virt-10.0`), with one 1920x1080 monitor at
96 DPI (100%), and Node reports it as `win32-arm64`. Defects that need a
second monitor or display scaling cannot be reproduced on it directly and are
proven with unit tests against the coordinate maths instead.

## Phase 1 runs

Engine: the installed desktop engine, 0.23.10 (the machine cannot build the
workspace — see below). Pages served locally: `examples/web/greeter.html` and
Cypress's own reference app, `cypress-io/cypress-example-kitchensink@d009dd0`
(MIT), fetched at run time into a scratch directory and never vendored. The
kitchensink `actions.cy.js` spec was translated to 11 vision flows; 4 of its
tests assert only CSS state and were declined. Recorded with `--no-repair`, so
each failure is the engine's own.

| Flow | Result | Cause |
|------|--------|-------|
| greeter (with a stale Edge "Restore pages" bubble on screen) | fail | the bubble swallowed the click on Greet; repair then blamed a browser crash that never happened (#8) |
| greeter (clean) | fail | OCR read `Hello, Ada!` as `Hello Adal`; exact-substring assertion (#6) |
| kitchensink type | fail at step 3 | quoted `text:` token never grounds (#5) |
| kitchensink clear, submit, click, dblclick, checkboxes | fail at first step | target below the fold; vision cannot scroll (#7) |

## Findings

| # | Area | Symptom | Cause | Status |
|---|------|---------|-------|--------|
| 1 | npm launcher | `npx flowproof` refuses to start: "no prebuilt binary for win32-arm64" | `sdk/js/bin/flowproof.js` maps platforms to binaries with no `win32-arm64` entry, though Windows 11 on ARM runs the `win32-x64` binary under emulation | confirmed, not yet fixed |
| 2 | keyboard | (expected under Citrix/RDP) typed text and key presses do not arrive in the remote session | `backend.rs` sends text as `KEYEVENTF_UNICODE` packets and keys as virtual-key codes with `wScan: 0`. Citrix and RDP clients forward keyboard input by scan code, so both are candidates for being dropped. Locally, typing works | suspected from code; Phase 2 |
| 3 | multi-monitor | (expected) a client window on a second monitor is captured and clicked wrongly | `gdi::capture_screen` captures the primary screen only; `frame()` clamps negative window origins to 0 while `click()` does not, so capture and click disagree; mouse normalisation covers the primary screen only | suspected from code |
| 4 | display scaling | (expected) at 125%/150% scaling, capture, window rect and click coordinates disagree | no DPI awareness is declared anywhere in the driver or the binary | suspected from code |
| 5 | authoring | every model-authored `rule_step` on `app: vision` (clear, check, select, hover) is rejected: "target `Text("text:…")` is not one of the listed scene targets", although that exact token is listed | the prompt tells the model to copy scene tokens into quotes; a vision scene lists only `text:` tokens; `rules::target_from_label` unwraps `css:` and `id:` but not `text:` | **fixed on branch, test written; not yet compiled** (no disk) |
| 6 | assertions | `page shows Hello, Ada` fails against a page showing `Hello, Ada!` | `text_matches` is exact substring plus a case fallback; OCR drops and misreads punctuation (`,` dropped, `!` → `l`), and a Citrix stream's compression will make it worse | confirmed; fix designed: a vision-only, widening-only rung ignoring punctuation at word edges (never inside a token, so `$10.00` cannot match `$1000`) |
| 7 | scrolling | any target below the fold is "not present on the current screen"; 5 of 6 kitchensink flows fail on it | `VisionAppDriver::resolve` OCRs one frame of the visible window and never scrolls; the input layer has no wheel event at all | confirmed; the largest gap. Needs a wheel `InputEvent`, scroll-and-search in `resolve`, and the same search at replay |
| 8 | repair | (a) a stale browser notification read as "the browser crashed"; (b) repair **rewrote the flow's assertion** from `Hello, Ada` (correct) to `Hello Ada!`, from OCR noise | the repair agent sees only OCR text, and is permitted to edit an assertion's expected value | confirmed. (b) is a product decision — whether repair may ever change what an assertion expects — not changed here |
| 9 | error text | "the previous step left a problem behind" on the FIRST step of a flow | the message assumes a prior step | confirmed, cosmetic |

## Blockers

- **The development machine cannot build the workspace.** C: is 73 GB with
  under 1 GB free, and 4 GB of RAM grows the page file into whatever space
  appears; a guarded test build of one crate fell to 247 MB free within a
  minute. Builds need a larger disk (30 GB added is comfortable) or another
  machine — CI, or a cloud session.

## Open questions

- Whether the record-time agent's default model (`claude-sonnet-5`) is a
  factor in vision recordings, or only the engine. Re-running a failing
  recording on a newer model separates the two; it is not a reason to change
  the default.
