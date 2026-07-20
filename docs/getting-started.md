# Getting started: your first flow (Windows Calculator)

flowproof records a flow once from a natural-language YAML spec, then replays
it deterministically — zero LLM calls at replay time. This walkthrough drives
Windows Calculator to compute **5 + 3 = 8**.

Requirements: **Windows 10/11** with the Calculator app, Python ≥ 3.9.

## Install

From PyPI (once 0.1.0 is published) or from a built wheel:

```powershell
pip install flowproof
# or: pip install .\flowproof-0.1.0-cp39-abi3-win_amd64.whl
```

Building from source instead? You need Rust and maturin: `pip install .`
from `sdk/python` compiles the engine automatically.

## 1. Write a spec

`calc.flow.yaml` (also in [`examples/calc.flow.yaml`](../examples/calc.flow.yaml)):

```yaml
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
```

## 2. Record

```powershell
flowproof record calc.flow.yaml
```

flowproof launches Calculator, resolves every step to a real UI Automation
element, **actually performs the flow** (you'll see the buttons pressed),
verifies the assertion against the live display, and writes
`calc.trace.jsonl` — one JSON step per line, human-diffable:

```text
Recorded 'Add two numbers': 5 steps -> calc.trace.jsonl
```

## 3. Replay

```powershell
flowproof run calc.flow.yaml
```

Replay is deterministic: it re-resolves the recorded selectors, presses the
same buttons, and evaluates the assertion by reading the display.

```text
  [PASS] s0001 Type 5
  [PASS] s0002 Press plus
  [PASS] s0003 Type 3
  [PASS] s0004 Press equals
  [PASS] s0005 display shows 8
PASS: Add two numbers (2154 ms) -> .flowproof\runs\20260718T120000.000Z\result.json
```

Exit codes: `0` pass, `1` test failure, `2` error. Each run writes a
self-contained bundle under `.flowproof/runs/<timestamp>/`: `result.json`
(the machine surface, including the step→time mapping), `report.html`
(with a step-synchronized frame viewer — click any step to see exactly
what happened), `junit.xml` (one testcase per step, for Jenkins / GitLab /
Azure DevOps / any CI that ingests JUnit — point your test-report collector
at `.flowproof/runs/*/junit.xml`), and `recording/` with the captured
keyframes plus `recording.gif` — the whole run as one animation, paced
like the real execution, embedded at the top of the report. Sensitive
regions are masked before frames are written: declare `redact:` rules in
the spec, and password fields are always masked automatically
(see [docs/recording.md](recording.md)).

### Running a whole suite

Point `run` at a **directory** and every `*.flow.yaml` under it (recursive,
sorted, `.flowproof` artifact dirs skipped) replays as one suite — a failing
flow doesn't stop the rest, each flow keeps its own run bundle, and a merged
`<dir>/.flowproof/suite-junit.xml` (one `<testsuite>` per flow) is what CI
ingests. Exit code is non-zero if ANY flow failed:

```bash
flowproof run specs/
```

Programmatic callers invoking the CLI should pass `--json`: the full
structured report prints to stdout instead of the human-readable lines —
never parse the prose output.

```powershell
flowproof run calc.flow.yaml --json
```

## Python API (the primary surface)

flowproof is built to be driven by programs — usually AI agents — with the
CLI as a thin wrapper over the same library. Every call returns structured
data:

```python
from flowproof import Flow

flow = Flow("calc.flow.yaml")

rec = flow.record()          # RecordResult(trace_path=..., steps=5)

result = flow.run()          # RunResult — truthy iff the flow passed
result.passed                # True
result.steps[4].status      # "passed"
result.steps[4].intent      # "display shows 8"
result.report_path           # Path to the result.json artifact

trace = flow.get_trace()     # {"header": {...}, "steps": [...]} for inspection
```

A failing test is a `RunResult` with `passed=False` (with per-step status
and failure detail) — not an exception. `RuntimeError` is reserved for runs
that could not execute at all.

## Running the end-to-end tests

Two E2E tests drive real apps, both Windows-only and gated on
`FLOWPROOF_E2E=1`:

```powershell
$env:FLOWPROOF_E2E = "1"
cargo test -p flowproof-cli --test calc_e2e -- --nocapture     # needs a desktop VM
cargo test -p flowproof-cli --test notepad_e2e -- --nocapture  # also runs in CI
```

The Notepad one (`examples/notepad.flow.yaml` — type text, assert the
document contains it) runs automatically in CI on `windows-latest`, so the
record→replay spine is proven on every push. Calculator stays a manual VM
walkthrough because GitHub's Windows Server runners don't ship the
Calculator app.

SAP has three tiers: `sap_pipeline` (in-memory fake engine, every
platform, plain `cargo test`), `sap_sim_e2e` (the REAL COM engine against
a simulated scripting API — `tests/support/sap_simulator.py` registers
the `SAPGUI` ProgID and serves SAP's object shapes; needs `pip install
pywin32`, runs in windows CI), and `sap_e2e` (a real SAP GUI session;
maintainer-run, `FLOWPROOF_E2E_SAP=1`).

## Web flows (any OS)

The same spine drives browsers through the `web` adapter — this works on
Linux and macOS too, since it runs on headless Chromium rather than Windows
UIA. Specs add a `url:` and use the web vocabulary
([`examples/web.flow.yaml`](../examples/web.flow.yaml)):

```yaml
name: Greet the user
app: web
url: examples/web/greeter.html   # or any http(s):// URL
steps:
  - Type Ada into the name field
  - Press the greet button
  - assert: page shows Hello, Ada
```

```bash
flowproof record web.flow.yaml && flowproof run web.flow.yaml
```

Set `CHROME=/path/to/chrome` if the browser isn't auto-detected. The web E2E
(`cargo test -p flowproof-cli --test web_e2e`, `FLOWPROOF_E2E=1`) runs in CI
on ubuntu.

### The web action vocabulary

Steps address elements the way a user sees them; the engine records a
selector for exactly what it resolved:

```yaml
steps:
  - Type Ada into the name field              # <id> form -> #name
  - Type Ada into the "Full name" field       # placeholder / accessible label
  - Type email into the 2nd "Field Name" field   # ordinal when labels repeat
  - Clear the "Search" field                  # replace semantics (fill)
  - Type Berlin                               # types into the FOCUSED element
  - Press the "Save" button                   # button by visible label
  - Click "Templates"                         # tabs, links, menu options, rows
  - Click "css:[data-test='expand']"          # css: prefix = CSS selector,
                                              #   for text-less icon buttons
  - Press Enter                               # named keys: Enter, Escape, Tab, …
  - Press Control+V                           # chords: Ctrl/Alt/Shift/Meta + key
  - Press Alt+Shift+Backspace
```

Text anchors match exactly first, then by prefix — `Click "Database"`
finds the card whose label *starts with* "Database" when no element matches
it exactly, mirroring how Playwright's accessible-name matching is used in
real suites.

### The assertion vocabulary (shared across every app profile)

Assertions describe **what** to check; **how** each target resolves is the
adapter's job, so the same forms work for web, desktop (UIA), SAP GUI —
and vision/OCR when that adapter lands. All forms auto-wait
(bounded, recorded timeout; `within <N>s` overrides) — including waiting
for the *target itself* to appear, so asserting on a toast works:

```yaml
steps:
  - assert: page shows Welcome                       # the SURFACE: page text on
                                                     #   web, window subtree on
                                                     #   UIA, OCR frame later
  - assert: page shows templates found 2 times       # occurrences of the TEXT
                                                     #   (not an element count)
  - assert: page does not show TestConnection        # waits for it to be GONE
  - assert: the templateName field contains Draft    # input VALUE, by NATIVE id
                                                     #   (DOM id / AutomationId)
  - assert: the "Field Name" field contains Street   # input VALUE, by label
  - assert: the "css:#live_preview" shows Street     # element-scoped substring
                                                     #   (css: is web-specific)
  - assert: the "css:#modal" is visible              # "visible" = the target
  - assert: the "css:#modal" is not visible within 15s   # RESOLVES (tree/DOM
                                                     #   presence, not pixels)
```

The Playwright equivalents quoted in the PR history (`toHaveCount`,
`toHaveValue`, `toBeVisible`, …) are the **web mapping** of these forms —
one provenance among four (uia, sap-com, vision/OCR, out-of-band), not
their definition. `calc` and `notepad` layer their sugar (`display shows`,
`document contains`) on top of the same shared grammar.

### Out-of-band assertions: the posted record, not the pixel

Enterprise correctness often lives in the database or behind an API, not
on screen. Structured steps probe it directly — app-independent,
auto-waiting like every other assertion, and replayed with zero model
calls:

```yaml
steps:
  - Press the "Save" button
  - assert_sql:
      connection: reporting            # env FLOWPROOF_SQL_REPORTING holds the
      query: >                         #   postgres connection string — the
        SELECT count(*) FROM templates #   trace only ever stores the NAME
        WHERE name = 'Customers'
      equals: "1"                      # first column of first row, as text
  - assert_api:
      request: GET ${DM_API}/templates # METHOD url; ${VAR} refs resolve at
      status: 200                      #   run time, never stored
      body_contains: Customers
      timeout_seconds: 30              # optional bound override (default 10s)
```

An unconfigured connection fails closed immediately with an error naming
the `FLOWPROOF_SQL_<NAME>` variable — never a silent pass.

(YAML note: an `assert:` value cannot *start* with a `"` — that's why
quoted targets always follow `the `.)

## Authenticated flows: sessions and navigation

Real app suites don't walk the login UI in every test — they inject a
session and start on the page under test (Playwright's storageState
pattern). Declare it in the spec; it's applied **before the page loads**
(cookies via CDP, localStorage before any page script runs), travels in
the trace with `${VAR}` references intact, and is re-applied identically
at every replay:

```yaml
name: Templates workspace
app: web
url: ${DM_BASE_URL}/templates          # env refs resolve at launch
session:
  cookies:
    - name: automators.session
      value: ${DM_SESSION_COOKIE}      # resolved at apply time, never stored
  local_storage:
    projectId: ${DM_PROJECT_ID}
steps:
  - Wait until page shows templates found within 30s
  - Go to /settings                    # same-origin navigation mid-flow
  - Reload the page
```

`Go to` takes a path (resolved against the flow URL's origin) or a full
URL.

## SAP GUI flows (Windows)

`app: sap` drives SAP GUI for Windows through **SAP GUI Scripting** — the
COM automation surface SAP ships — never through pixels or synthetic
keystrokes. Requirements: SAP Logon running and logged in, scripting
enabled (`sapgui/user_scripting = TRUE` in RZ11), and flowproof on the
same Windows machine.

```yaml
name: Create standard order
app: sap
connection: ${SAP_CONNECTION}   # SAP Logon entry to open if no session is
                                # running yet; omit to attach to the current one
steps:
  - Go to /nVA01                                    # navigate by transaction code
  - Type ZOR into the "Order Type" field            # anchors match the tooltip,
                                                    #   visible text, or technical
                                                    #   name (VBAK-AUART)
  - Type 4711 into the "id:wnd[0]/usr/txtVBAK-KUNNR" field   # scripting id, direct
  - Press Enter                                     # SAP virtual keys: Enter,
                                                    #   F1–F12, Shift/Ctrl+F1–F12
  - assert: page shows Create Standard Order        # the whole session surface
```

The scripting id (`wnd[0]/usr/ctxtVBAK-AUART`) is this provenance's
**native selector rung** — recorded with `provenance: sap-com`, replayed
deterministically, and offered to the LLM author as `id:` target tokens
like any other scene. Labelled press targets also record the label as a
text-anchor fallback rung, so those steps survive id drift (degraded,
reported, healable). See `examples/sap/create-order.flow.yaml`.

## Vision flows: pixels only (Citrix, RDP, anything)

`app: vision` drives a window with **no accessibility API at all** —
perception is OCR over captured frames, action is real mouse/keyboard
injection. This is the mode for Citrix/RDP sessions where the remote app
is just pixels on your screen (Windows-only today: capture + SendInput).

```yaml
name: Post order
app: vision
window: Citrix Receiver        # title (substring) of the window to drive
steps:
  - Type ZOR into the "Order Type" field    # OCR finds the LABEL; the click
                                            #   lands right of it, in the field
  - Press the "Submit" button               # clicks the text itself
  - assert: page shows Order saved          # asserts on the OCR'd frame
```

Text anchors match OCR lines exactly first, then by prefix; `the 2nd
"Amount" field` disambiguates repeats in reading order. The recorded
trace carries `provenance: vision` text anchors with their spatial
`relation` (`inside` for clicks, `right_of` for fields), and freeform
steps work through the LLM author — the OCR lines are the scene. OCR
models (pure-Rust [ocrs](https://github.com/robertknight/ocrs), ~12 MB)
download on first use to `~/.cache/flowproof/ocrs`. Deliberately not in
this slice yet: visual-template matching and OCR-region sync conditions.

## API-only flows (no browser, any OS)

Not every test drives a UI. `app: api` runs a flow of **out-of-band
assertions only** — HTTP status/body and SQL row checks — with no browser
and no window launched, on any platform:

```yaml
name: Provisioning API
app: api
steps:
  - assert_api:
      request: GET ${API}/health
      status: 200
      body_contains: '"status":"ok"'
  - assert_api:
      request: POST ${API}/teams/${TEAM}/members    # cross-team write must 403
      status: 403
  - assert_sql:
      connection: reporting
      query: SELECT count(*) FROM members WHERE team_id = '${TEAM}'
      equals: "1"
```

These are the tests that assert on HTTP status codes and response bodies
with no UI to drive — they run through the same deterministic record/replay
spine (zero model calls), and the connection names and `${VAR}` hosts never
enter the trace. See `examples/api/health.flow.yaml`.

## Authoring with a model (arbitrary steps)

The rules only know the demo vocabularies. With a model backend configured,
`record` falls back to the **LLM author** for any step the rules can't
parse, on web and Windows desktop apps alike. The driver describes its live
scene — each interactable element tagged with a provenance-neutral *target
token* (`css:#name` on the web, `id:15` / `text:Close` under UI
Automation) — and the model must copy one of those tokens verbatim; it can
never invent a selector. Assertions may also target the literal `surface`
token: everything readable on the current screen, whatever the driver:

```bash
export FLOWPROOF_AI_PROVIDER=anthropic        # or openai-compatible
export FLOWPROOF_AI_API_KEY=sk-...            # falls back to ANTHROPIC_API_KEY
flowproof record shop.flow.yaml               # steps in your own words
flowproof run shop.flow.yaml                  # replays with ZERO model calls
```

`--author rules|llm|auto` controls the mode (default auto). The trace header
records `agent: {backend, model}` whenever a model authored steps — reviewers
always know. For a local model: `FLOWPROOF_AI_PROVIDER=openai-compatible`
plus `FLOWPROOF_AI_BASE_URL=http://localhost:8000/v1` (vLLM).

## When the app drifts: fallback selectors and `degraded`

Replay walks each step's recorded selector ladder in order: the native id
first, then structural (control type + accessible name), then a text
anchor. If the primary selector is dead but a fallback rung still finds the
element, the step runs and the flow stays green — but the step and the run
are marked `degraded` in `result.json` (with the matched tier in
`selector_tier`), the CLI prints a `DEGRADED:` line pointing at `heal`, and
`RunResult.degraded` is set in Python. Degraded-but-passing is the signal
to heal the trace *before* the remaining rungs die too:

```text
  [PASS] s0002 Press plus (matched via structural fallback)
PASS: Add two numbers (2154 ms) -> .flowproof\runs\...\result.json
DEGRADED: fallback selectors were needed — the app drifted; run `flowproof heal calc.flow.yaml`
```

## Waiting on slow operations (no sleeps, still deterministic)

Assertions **auto-wait**: the engine polls until the expectation holds or a
bounded timeout elapses (default 10s), during recording and at every
replay. The bound is recorded into the trace, so replay waits exactly as
long as authoring allowed — deterministic, no sleeps in specs. For slow
backend operations, use an explicit wait step (default bound 60s) or a
`within` qualifier on either form:

```yaml
steps:
  - Press the generate button
  - Wait until page shows Generation complete within 120s
  - assert: page shows 100 rows within 5s
```

## Secrets: values never enter the trace

Traces are reviewable, diffable artifacts — so sensitive values must never
be written into them. Write `${VAR}` references in the spec instead:

```yaml
steps:
  - Type ${LOGIN_PASSWORD} into the password field
  - Press the login button
  - assert: page shows Welcome, ${LOGIN_USER}
```

The engine resolves references from the environment **at the moment of
use** — during recording and again on every replay. The trace (and every
artifact rendered from it) stores only the literal `${LOGIN_PASSWORD}`
reference. A reference to an unset variable fails closed: recording is
refused, and a replay step fails with an error naming the variable —
flowproof never types the literal reference into the app. Failure messages
mask live text whenever the expectation contained a reference.

This covers the *trace text*; the *pixels* of secret fields are covered by
the recording layer (`redact:` rules and always-on password-field masking,
see [docs/recording.md](recording.md)).

## Healing a stale trace

When the app changes and replay fails, `heal` re-authors the flow from the
spec against the live app and proposes a reviewable diff — it never touches
the trace on its own:

```text
$ flowproof heal calc.flow.yaml
  [CHANGED] s0002 Press plus (selectors)
REVIEW: calc.heal.html (before/after with frames)
PROPOSED: review calc.proposed.jsonl then re-run with --apply
$ flowproof heal calc.flow.yaml --apply   # explicit opt-in
```

Alongside the machine-readable proposal, heal writes `<name>.heal.html` — a
self-contained review page with a before/after pair per changed step: the
frames each execution's recording captured for that step (recorded run vs.
re-authored run) plus the step JSON, rendered entirely from the structured
report. Open it to see *what the app looked like* when each version of the
step ran, then decide on `--apply`.

Exit codes: `0` healthy (or applied), `1` changes proposed for review,
`2` error. `--json` emits the structured report (including `diff_html`); the
Python API returns a `HealResult` (with `diff_html: Path | None`) and the
MCP tool mirrors it.

## What's deliberately missing (this is the first slice)

- Only `calc`, `notepad`, `web`, and `sap` resolve, each with a small
  vocabulary — the rule-based resolver covers the common forms and the AI
  authoring agent handles everything else through the same seam (healing
  re-uses it too).
