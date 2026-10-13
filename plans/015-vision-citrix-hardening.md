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

## Live result after the fixes

With #697, #698, #699 and #702 built together (12 October), on the same
kitchensink page: **two flows record AND replay green with zero model calls
on replay** - clicking a button and choosing a dropdown option, both far below
the fold. The final replay frames show the popover open and `apples` selected,
so the passes are not just the assertions matching the page's code samples.
The morning's run had 0 of 6. The type flow now stops honestly at finding 17
(fixed since) instead of passing falsely.

Second live run (12 October, evening, all five PRs built together): the
double-click visibly works (finding 15), scroll-to-bottom records and replays,
and the type flow's green replay is a FALSE pass (finding 18) - so the honest
count of proven flows is still two plus scroll-to-bottom.

Third live run (13 October, all six PRs): **double-click records and replays,
proven by its frame** (`Zebra42Double click to edit` in the revealed input,
a word nowhere else on the page). #706's warning fired on both of the type
flow's checks at record time, and its green replay was again false (finding
21). Proven flows: popover click, dropdown select, double-click, and
scroll-to-bottom.

## Findings

| # | Area | Symptom | Cause | Status |
|---|------|---------|-------|--------|
| 1 | npm launcher | `npx flowproof` refuses to start: "no prebuilt binary for win32-arm64" | `sdk/js/bin/flowproof.js` maps platforms to binaries with no `win32-arm64` entry, though Windows 11 on ARM runs the `win32-x64` binary under emulation | confirmed, not yet fixed |
| 2 | keyboard | (expected under Citrix/RDP) typed text and key presses do not arrive in the remote session | `backend.rs` sends text as `KEYEVENTF_UNICODE` packets and keys as virtual-key codes with `wScan: 0`. Citrix and RDP clients forward keyboard input by scan code, so both are candidates for being dropped. Locally, typing works | suspected from code; Phase 2 |
| 3 | multi-monitor | (expected) a client window on a second monitor is captured and clicked wrongly | `gdi::capture_screen` captures the primary screen only; `frame()` clamps negative window origins to 0 while `click()` does not, so capture and click disagree; mouse normalisation covers the primary screen only | suspected from code |
| 4 | display scaling | (expected) at 125%/150% scaling, capture, window rect and click coordinates disagree | no DPI awareness is declared anywhere in the driver or the binary | suspected from code |
| 5 | authoring | every model-authored `rule_step` on `app: vision` (clear, check, select, hover) is rejected: "target `Text("text:…")` is not one of the listed scene targets", although that exact token is listed | the prompt tells the model to copy scene tokens into quotes; a vision scene lists only `text:` tokens; `rules::target_from_label` unwraps `css:` and `id:` but not `text:` | **fixed** — this branch, test `rule_step_grounds_a_quoted_text_token_from_a_vision_scene` |
| 6 | assertions | `page shows Hello, Ada` fails against a page showing `Hello, Ada!` | `text_matches` is exact substring plus a case fallback; OCR drops and misreads punctuation (`,` dropped, `!` → `l`), and a Citrix stream's compression will make it worse | **fixed** — branch `vision-hardening/ocr-text`: `AppDriver::reads_text_by_ocr` plus a widening-only rung ignoring punctuation at word edges, never inside a token |
| 7 | scrolling | any target below the fold is "not present on the current screen"; 5 of 6 kitchensink flows fail on it | `VisionAppDriver::resolve` OCRs one frame of the visible window and never scrolls; the input layer has no wheel event at all | **fixed** — branch `vision-hardening/scroll`: a wheel `InputEvent`; actions search down then up a page at a time; existence checks never scroll |
| 8 | repair | (a) a stale browser notification read as "the browser crashed"; (b) repair **rewrote the flow's assertion** from `Hello, Ada` (correct) to `Hello Ada!`, from OCR noise | the repair agent sees only OCR text, and is permitted to edit an assertion's expected value | confirmed. (b) is a product decision — whether repair may ever change what an assertion expects — not changed here |
| 9 | error text | "the previous step left a problem behind" on the FIRST step of a flow | the message assumes a prior step | confirmed, cosmetic |
| 10 | recording | with the scroll fix in place, every below-the-fold flow still stopped at its first step: "previous step left a problem — X is not on the current screen" | the recorder asks the model BEFORE the driver acts, and the model is shown only the visible frame; with no way to say "below the fold" it reports the previous step broken | **fixed** — #698: on `app: vision` the step's first off-screen quoted label is scrolled into view before the model is asked |
| 11 | unattended runs | a re-run reported only browser chrome on screen, page body blank, for every flow | the VM's display stopped repainting once the host was left (taskbar clock frozen 12 minutes behind); capture returned stale frames and the browser painted nothing | confirmed. Environment, not engine — but flowproof read a frozen screen as "the field is not there". A vision run should detect a frame that never changes across actions and say the display is not updating. Not yet fixed |
| 12 | replay | the first three flows to record all failed to replay: "element did not appear within 5000ms"; one target recorded as `Fake@email.com` read back as `fake@email.com` | replay's existence precondition is a pure read of the visible frame, so an off-screen target timed out before the action's scroll search; vision anchors matched case-sensitively | **fixed** — #702 (stacked on #698): `AppDriver::reveal` before the wait and before giving up; case-insensitive fallback |
| 13 | false pass | `Type slow.typing@email.com into the "Email address" field` recorded as a single Escape press, and the flow passed | the model dismissed Edge's autofill popup, then judged the step done because the value appears in the page's code sample; the next `page shows` matched the same sample | **warned** — #706: a vision `page shows` whose text was on screen before the actions it follows carries a recording warning. (A type step that types nothing is not yet guarded.) |
| 14 | layout | `Type ... into the "Describe:" field` authored no action | vision types `right_of` a label by default (the SAP GUI layout); web forms put the label ABOVE the box | confirmed — **needs a decision** (see Open questions) |
| 15 | actions | double-click and checkbox steps fail: "double-click is not supported by this driver"; "no actionable checkbox" | `VisionAppDriver` implements neither `double_click` nor `set_checked`, and vision scene entries carry no checkbox role | double-click and right-click **fixed** — #705. Checkboxes **need a decision** (see Open questions) |
| 16 | grounding | `Press the "Submit" button` authored no action | not yet diagnosed — needs the `record --json` inventory from a live run | open |
| 17 | clearing | after a recorded Clear, the field still held its old value and the next step stopped on it | a clear is recorded as `type_text("")` with replace; vision select-all'd and typed nothing | **fixed** — #702: typing nothing deletes the selection |
| 18 | false pass (again) | the type flow REPLAYED green, but its final frame shows the email field empty under Edge's autofill popup | the clear worked; the second value never landed (typed while the autofill popup held focus); `page shows slow.typing@email.com` matched the page's code sample | **warned** — #706 (same guard as 13); the autofill-popup focus loss itself is not fixed |
| 19 | deterministic steps sent to the model | after a double-click that visibly worked (input revealed and focused), `Type Zebra42` was refused ("previous step left a problem") or looped ("asked for another screen" 3 times) | under `--author auto` with a model configured, every plain step is model-authored, even one the grammar resolves exactly (bare `Type X` is `TypeFocused`); on vision the model sees only OCR text and second-guesses it | **fixed** — #706: on `app: vision`, a plain step resolving to actions that name no element skips the model |
| 20 | nested scrolling | `Scroll "I'm Here" into view` — the target is in a small scrolling box inside the page | the wheel is turned over the window centre, which scrolls the page, never an inner container | confirmed, not fixed; inner scroll areas are common in Citrix grids |
| 21 | silent type failure | with every fix built, the type flow replayed green again; its final frame shows the field empty and the WHOLE PAGE selected. #706's warning flagged both checks at record time | the click meant for the field lands `right_of` its label, which sits ABOVE the box, so the field never gets focus; select-all then selects the page and the typing goes nowhere | confirmed — this is finding 14 failing silently. Fixing 14 fixes it; a read-back guard (OCR near the field after typing: did the value appear?) would catch every silent type failure |
| 22 | right-click in a browser | the right-click visibly revealed and focused the input, but `Type Quokka77` landed in Edge's own context menu | real input opens the browser's native menu, which takes focus - as it would for a person. Cypress's synthetic `rightclick()` never shows it | test content, not engine: the flow presses Escape after the right-click |

## Notes for merging

- #698 and #699 both add a method at the same point in `VisionAppDriver`'s
  `AppDriver` impl, and both add a CHANGELOG entry and a docs paragraph at the
  same place. Whichever merges second has a keep-both conflict in all three.
- Live vision runs need a display that is on and repainting: on this VM,
  leaving the host stops it (finding 11).

## Blockers

- **The development machine cannot build the workspace.** C: is 73 GB with
  under 1 GB free, and 4 GB of RAM grows the page file into whatever space
  appears; a guarded test build of one crate fell to 247 MB free within a
  minute. Builds need a larger disk (30 GB added is comfortable) or another
  machine. Clearing the Visual Studio installer's package cache freed 6.2 GB,
  enough for single-crate test builds with debug info off; full builds are
  left to CI.

## Open questions

- **Checkboxes on `app: vision` (finding 15).** `set_checked` is SET-state and
  must verify, so a reseeded environment cannot silently invert a step. Pixels
  give no checkbox state, so a click only toggles. Either vision keeps refusing
  checkboxes with a clearer message, or it reads the state from the box's
  pixels (compare the box region before and after the click) - heuristic, and
  it should be proven on real Citrix screens before it is trusted.
- **Labels above fields (finding 14).** A vision trace stamps WHERE an action
  lands relative to its label, hard-coded to `right_of` for typing
  (`recorder::stamp_vision_relation`) - right for SAP GUI, wrong for most web
  forms. Either the grammar lets an author say so (`into the field below
  "Describe:"`, a grammar change), or the recorder infers the layout from the
  pixels around the label.
- **Is a web page the right proxy for Citrix?** Kitchensink found real engine
  bugs, but it is a documentation page whose code samples make `page shows`
  vacuous, and Citrix targets are business forms and grids. Candidates closer
  to the client case: `examples/tricentis-insurance.flow.yaml`, the Notepad and
  Calculator desktop flows, and a SAP GUI flow, all as `app: vision`.

- Whether the record-time agent's default model (`claude-sonnet-5`) is a
  factor in vision recordings, or only the engine. Re-running a failing
  recording on a newer model separates the two; it is not a reason to change
  the default.
