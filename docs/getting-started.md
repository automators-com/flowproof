# Getting started

flowproof records a flow once from a natural-language YAML spec, then replays
it deterministically - **zero LLM calls at replay time**.

Start with the agent quickstart below. The UI walkthrough after it is the
same idea applied to a desktop app, and needs Windows.

## Install

Either package ships the same engine as a native binary. Pick whichever
matches the project you are testing:

```bash
npx flowproof --version      # no install, no Python
```

```bash
npm install --save-dev flowproof
```

```bash
pip install flowproof
```

The npm package resolves a platform binary for linux-x64, darwin-x64,
darwin-arm64 and win32-x64. On any other platform install from PyPI instead;
`npx flowproof` will say so rather than fail obscurely.

Building from source instead? You need Rust and maturin: `pip install .`
from `sdk/python` compiles the engine automatically.

## Quickstart: test an agent

The thing flowproof is for: an agent calls tools, and you want a test that
fails when it calls the wrong one - without paying a model on every CI run.

[`examples/agent-demo/`](../examples/agent-demo/) has a real agent built on
the official OpenAI SDK, in two languages. Use the Node one if you installed
from npm; nothing here needs Python.

```yaml
# examples/agent-demo/weather-node.flow.yaml
name: Weather assistant answers with the forecast (Node)
app: agent
agent:
  command: node examples/agent-demo/weather_agent.mjs
tools:
  - name: get_weather
    result: { city: Nairobi, sky: sunny, temp_c: 26 }
steps:
  - prompt: What is the weather in Nairobi right now? Use your tools.
  - assert_tool_call: get_weather where city contains Nairobi
  - assert: reply contains sunny
```

**Record once.** This is the only step that calls a real model, so it is the
only step that needs a key:

```bash
npm install openai
export ANTHROPIC_API_KEY=...        # or OPENAI_API_KEY
npx flowproof record examples/agent-demo/weather-node.flow.yaml
```

**Replay for ever.** No key, no model, no network to the provider:

```bash
npx flowproof run examples/agent-demo/weather-node.flow.yaml
```

```text
  [PASS] s0001 prompt
  [PASS] s0002 get_weather where city contains Nairobi
  [PASS] s0003 reply contains sunny
PASS: Weather assistant answers with the forecast (Node)
```

What just happened, and why it is worth having:

- The agent ran **for real** both times - same client, same tool loop.
- At record, flowproof sat at the model boundary and captured the exchange.
  At replay it served that recording back, so the trajectory is fixed and
  **no model was called**. A CI run costs nothing and cannot flake on
  sampling.
- `assert_tool_call: get_weather where city contains Nairobi` is the part
  that fails when the agent regresses: wrong tool, wrong argument, or a
  tool called out of order.
- `get_weather` returns a live timestamp. Replay is deterministic anyway,
  because the spec's `result:` is substituted at the model boundary.

Two limits worth knowing before you build on this, rather than discovering
them later:

- **A flow is one turn, not a conversation.** Every `prompt:` step is joined
  into a single task delivered up front; there is no follow-up user turn.
  See [agent-testing.md](agent-testing.md).
- **The model boundary is not the tool boundary.** A `tools:` mock changes
  what the model is TOLD a tool returned; the agent still ran its own tool.
  Only the `mcp:` boundary stops a tool executing. flowproof warns at
  runtime when a flow relies on this.

Python instead of Node? Same flow, same assertions:
[`weather.flow.yaml`](../examples/agent-demo/weather.flow.yaml) runs
`python3 examples/agent-demo/weather_agent.py` (`pip install openai`).

**Adding flowproof to an existing agent?** [adopting.md](adopting.md) is
written to be handed to a coding agent: the audit to run first, the three
questions that decide everything, and the order to do it in.

Next: [agent-testing.md](agent-testing.md) for the full assertion grammar,
the MCP tool boundary, and egress containment.

## Walkthrough: a UI flow (Windows Calculator)

The same record-once/replay-deterministically idea, applied to a desktop
app. This drives Windows Calculator to compute **5 + 3 = 8**.

Requirements: **Windows 10/11** with the Calculator app.

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

That filename is a convention, not a lookup: `record` derives
`<name>.trace.jsonl` from the spec, and `run` and `heal` derive the same one
when you do not say otherwise. Override it when a trace should not sit next
to its spec — `--out` chooses where `record` writes, `--trace` tells `run`
and `heal` where to read:

```powershell
flowproof record calc.flow.yaml --out traces/calc.trace.jsonl
flowproof run   calc.flow.yaml --trace traces/calc.trace.jsonl
flowproof heal  calc.flow.yaml --trace traces/calc.trace.jsonl
```

Keep the pair together unless you have a reason not to. A suite run resolves
every spec's trace by the convention and **ignores `--trace`**, so a
relocated trace reads to `run <dir>` as a flow that was never recorded —
skipped by default, or a hard error under `--strict`.

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
PASS: Add two numbers (2154 ms) -> .flowproof\runs\20260718T120000.000Z\report.html
```

The path is the one worth opening: `report.html` is the human rendering, and
on a headless adapter it is the only way to *see* what the run did. Exit
codes: `0` pass, `1` test failure, `2` error. Each run writes a
self-contained bundle under `.flowproof/runs/<timestamp>/`: `result.json`
(the machine surface, including the step→time mapping), `report.html`
(with a step-synchronized frame viewer — click any step to see exactly
what happened), `junit.xml` (one testcase per step, for Jenkins / GitLab /
Azure DevOps / any CI that ingests JUnit — point your test-report collector
at `.flowproof/runs/*/junit.xml`), and `recording/` with the captured
keyframes. Pass `--video` to additionally create `recording.gif` — the whole
run as one animation, paced like the real execution, embedded at the top of
the report. Sensitive
regions are masked before frames are written: declare `redact:` rules in
the spec, and password fields are always masked automatically
(see [docs/recording.md](recording.md)).

For faster runs, GIF/video assembly is off by default while screenshots remain
available. Use `--video` to opt in. `--recording-detail low` captures only the
initial state, every fifth step, and the final state; `--recording-detail off`
disables screenshots and video entirely. The flags work on both `record` and
`run`, and affect artifacts rather than execution or verdicts. Add
`--highlight-cursor` when reviewers should see a synthetic cursor and bright
click halo at each pointer action; drag steps highlight both their source and
destination.

**When a step fails**, the bundle additionally answers the first two
questions a human asks. `debug/dom.html` is the full DOM at the moment of
failure and `debug/console.log` the page's recent console/exception tail
(web flows; captured best-effort). And when an anchored element wasn't
found, the failure detail suggests the nearest visible text anchors:
`element … not found — did you mean 'Save changes'?` — usually the whole
diagnosis for drifted labels, with `flowproof heal` as the fix.

**Verify the recording reproduces itself.** A recording is a claim that
the flow can be performed again from the trace alone, and nothing checks
that claim: authoring succeeds when the live app happens to cooperate, so
a target that was merely reachable at that moment — a button under a
rotating carousel, a field beneath a datepicker that hadn't opened yet —
gets written down as though it always would be. The first person to learn
otherwise is whoever runs the suite.

```bash
flowproof record shop.flow.yaml --verify
```

`--verify` replays the new trace once, immediately, and refuses the
recording if it cannot reproduce itself: the trace is kept as evidence for
`flowproof heal`, and the command exits non-zero saying so. It is opt-in
rather than the default because it **performs the flow a second time**
against the live application, repeating whatever that flow does — orders,
e-mails, payments. Turn it on for a flow whose steps are safe to repeat,
and leave it off for one that isn't.

**Incremental re-record.** When the app changes, don't re-record the
flow — re-record the step: `flowproof record calc.flow.yaml --reuse`
walks the spec against the existing trace and reuses every old step
whose intent still matches and whose target still resolves on the live
app, verbatim (same selectors, zero rules/model work). Only drifted or
new steps are authored fresh — for model-authored steps that means the
model is consulted **only** for the drift. The summary reports the
split: `Recorded 'Flow': 12 steps (11 reused)`.

**Actionability.** Element actions don't fire on an element that merely
exists: replay gates every click/type on **enabled** (not
`disabled`/`aria-disabled`), **stable** (bounding box settled — no
mid-animation clicks), and **receives events** (a click at its center
actually reaches it, not a toast or modal backdrop), polling within the
step's auto-wait bound. A gate that never clears fails with its name —
`element exists but is disabled after 5000ms` — so a flake is a
diagnosis, not a mystery.

### Running a whole suite

Point `run` at a **directory** and every `*.flow.yaml` under it (recursive,
sorted, `.flowproof` artifact dirs skipped) replays as one suite — a failing
flow doesn't stop the rest, each flow keeps its own run bundle, and a merged
`<dir>/.flowproof/suite-junit.xml` (one `<testsuite>` per flow) is what CI
ingests. Exit code is non-zero if ANY flow failed:

```bash
flowproof run specs/
flowproof run specs/ --retries 2      # re-run a flow that fails, up to twice
```

**Dev servers with file watchers.** flowproof writes each run bundle to
`.flowproof/runs/…` inside the project, next to the spec it came from. A dev
server watching that tree (vite, webpack-dev-server, nodemon) sees those
files appear and reloads the app **mid-run**, which can fail a flow for
reasons that have nothing to do with the app. Exclude the artifacts from the
watcher: in vite that is `server.watch.ignored: ["**/.flowproof/**"]`, plus
any directory your app writes to during a test (a JSON-file database, an
upload folder).

Deterministic replay is stable, but the infrastructure under it (a dropped
CDP frame, a momentarily slow backend) is not — `--retries N` re-runs a
failed flow up to N more times with a fresh driver before calling it
failed. The web adapter reuses **one headless browser** across the whole suite
(an isolated context per flow), so the cold start is paid once, not per flow;
set `FLOWPROOF_NO_SHARED_BROWSER=1` to force a browser per flow. A headed run
is private automatically: its visible browser is maximized and closes with the
flow instead of leaving the shared keep-alive window on the desktop.

**Suite manifest.** A suite usually needs sequencing a bespoke harness
would otherwise provide — shared env, seed before each flow, cleanup after.
Declare it in an optional `suite.yaml` next to the specs instead:

```yaml
# specs/suite.yaml
env:
  DM_BASE_URL: http://localhost:3000
  DM_SESSION_COOKIE: ${DM_SESSION_COOKIE}   # re-map / compose ambient vars
before_each: pnpm --filter app exec tsx seed.ts   # $FLOWPROOF_SPEC = the spec path
after_each: pnpm --filter app exec tsx cleanup.ts
order:                                       # optional; unlisted specs run after, sorted
  - smoke/login.flow.yaml
```

`env` is exported to every flow and hook; `before_each`/`after_each` run
via `sh -c` with the current spec path in `$FLOWPROOF_SPEC`. A hook that
exits non-zero aborts the suite — silent seed/cleanup failure is exactly
the fragility to avoid.

**Minted test data: `env_from`.** Hooks are for *effects*; their stdout
is not captured. When flows need values an external CLI mints (DataMaker
picking a valid Material/Supplier/Plant out of SAP), declare a data
command instead:

```yaml
env_from: datamaker sap info-record pick --plant 1010 --format env
```

It runs once before any flow (via `sh -c`, from the suite directory); its
stdout must be `KEY=VALUE` lines (`#` comments and blank lines allowed)
which become env vars for every flow and hook — reachable from specs as
`${VAR}`. It fails closed: a non-zero exit or a malformed line aborts the
run, and the command's stderr is echoed either way, so a mint script that
explains itself is heard.

The command **runs with the suite's `env:` visible** — minting test data
almost always needs the suite's own base URL and credentials. Each `env:`
entry is resolved against the process environment for this purpose; an
entry that cannot resolve yet is simply not passed (it may reference this
command's own output, and it gets its turn afterwards). Two orderings are
easy to conflate and only the first changed: what the *command sees* now
includes `env:`, while `${VAR}` precedence *in flows* is unchanged —
process env, then `env_from` output, then `env:`.

Suite context follows single flows too: `record` and single-spec `run`
discover the nearest `suite.yaml` walking up from the spec (nearest wins;
the chosen manifest is named on stderr), so a flow behaves the same alone
as inside its suite — including at record time, when `${VAR}`s must
already resolve. Note the trust model: running a spec executes the
`env_from`/hooks of the suite it belongs to, same as running the suite.
See [self-help.md](self-help.md) for the authoring loop this enables.

More suite machinery, all from the first external adoption:

- **`min_version: "X.Y.Z"`** in `suite.yaml`: the engine refuses to run
  when older than the suite demands, naming both versions. Set it when
  specs use vocabulary an older flowproof would have mishandled (before
  0.2.2, unknown spec fields were silently ignored; now they are parse
  errors).
- **Missing traces skip, not abort**: a committed spec whose trace was
  never recorded reports as junit `skipped` with the reason, instead of
  hard-failing everyone's suite run. `--record-missing` records it in
  place first; `--strict` restores the hard error for CI that must not
  let coverage silently shrink.
- **Suite `env:` is lazy per entry**: an unresolvable value warns and is
  skipped instead of blocking flows that never reference it. A flow that
  DOES reference it still fails at the moment of use, naming the
  variable. (`env_from` stays fail-closed — a data command failing is
  never ignorable.)
- **`skip_unless_env: [FLAG]`** on a spec: first-class env-flag gating,
  reported as junit `skipped` with the reason instead of an invisible
  bash guard. Checked after suite env applies, so `suite.yaml` can
  satisfy the gate; a gated flow skips even under `--strict`.

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

rec = flow.record()          # includes per-step llm/rules/fallback routing
rec = flow.record(author="rules")  # deterministic grammar for all plain steps

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

## Running the live-app tests

Two live-app tests drive real applications, both Windows-only and gated on
`FLOWPROOF_E2E=1` (the gate variable's name is a stable interface and
predates the current naming):

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

### Deploying a UWP app on a CI runner

A Windows Server runner can still run a UWP app the suite needs — you
build and side-load it in the workflow. The sequence below is the one
that works for Microsoft's open-source Calculator (each step's obvious
alternative fails in a non-obvious way):

1. **Build the solution target, not the csproj**:
   `msbuild Calculator.slnx -t:Calculator`. Building the project file
   directly fails on project references that only resolve through the
   solution.
2. **Install the signing certificate into `TrustedPeople`**: the build
   signs the package with an ephemeral `SignTestApp` certificate;
   side-loading rejects it until that certificate is trusted —
   specifically in the **TrustedPeople** store, not Root or My.
3. **Side-load with the generated script**:
   `.\Add-AppDevPackage.ps1 -Force` (next to the built `.appx`/`.msix`)
   installs the package for the runner's user.
4. **Launch through the alias**: the System32 `calc.exe` stub now
   resolves to the dev build, so `app: calc` (or an `app:` mapping with
   `command: calc.exe`) drives it with no further wiring.

Two UWP-specific traps for specs and window handling: the visible window
belongs to **ApplicationFrameHost**, not the app's own process — target
windows by *title*, never by process; and the frame window is the one
`window:` geometry applies to.

SAP has three tiers: `sap_pipeline` (in-memory fake engine, every
platform, plain `cargo test`), `sap_sim_e2e` (the REAL COM engine against
a simulated scripting API — `tests/support/sap_simulator.py` registers
`SAPGUI` in the ROT as an item moniker, the way real SAP GUI does, and
serves SAP's object shapes; needs `pip install pywin32`, runs in windows
CI), and `sap_e2e` (a real SAP GUI session;
maintainer-run, `FLOWPROOF_E2E_SAP=1`).

## Web flows (any OS)

The same spine drives browsers through the `web` adapter — this works on
Linux and macOS too, since it runs on Chromium (headless by default; see
[`FLOWPROOF_HEADED`](#watching-the-browser-flowproof_headed)) rather than
Windows UIA. Specs add a `url:` and use the web vocabulary
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

Set `CHROME=/path/to/chrome` if the browser isn't auto-detected. The web
live-app suite (`cargo test -p flowproof-cli --test web_e2e`,
`FLOWPROOF_E2E=1`) runs in CI on ubuntu.

### Watching the browser: `FLOWPROOF_HEADED`

Web flows run headless by default, which is right for CI and unhelpful the
first time a recording does not do what you expected. Set `FLOWPROOF_HEADED`
to see the window:

```bash
FLOWPROOF_HEADED=1 flowproof record web.flow.yaml
```

```powershell
$env:FLOWPROOF_HEADED = "1"; flowproof record web.flow.yaml
```

It is presence-based, like `FLOWPROOF_NO_SHARED_BROWSER` — `FLOWPROOF_HEADED=0`
still shows the window, because a variable you bothered to set is one you meant.
Unset it to go back to headless.

Deliberately an environment variable and not a spec field: watching is a
property of the run you are supervising, not of the flow. A committed
`headed: true` would follow the flow into CI, where nobody is watching and
there may be no display at all.

The visible browser belongs to that flow alone. Flowproof maximizes it and
brings it to the foreground before navigation starts, then closes the process
when the flow finishes; headed runs do not use the headless suite's shared
keep-alive browser or its isolated second window.

To inspect the final page after a single flow completes, use `--keep-open`:

```bash
flowproof record web.flow.yaml --keep-open
flowproof run web.flow.yaml --keep-open
```

It implies a visible browser and waits after execution. Close that Chromium
window to let Flowproof exit; the normal default remains to close it
automatically. The option is intentionally unavailable for directory suites,
retries, and `--json` callers, where waiting for a person would make automation
hang.

**One caveat if the flow has visual assertions.** Headed Chromium sizes its
window from the desktop; headless uses a fixed default. Record a screenshot
baseline headed and replay it headless and the sizes differ, which replay
reports as:

```
screenshot is 1280x720 but baseline 'checkout' is 800x600 — viewport changed? re-record to refresh the baseline
```

Pin the size in the spec and both modes produce the same frames:

```yaml
app: web
url: https://example.test
browser:
  viewport:
    width: 1280
    height: 720
```

Flows without visual assertions are unaffected — the DOM does not change shape
because a window is visible.

**It needs a real desktop session.** Over SSH, in a container, or on a CI
runner there is no window to show, so Chromium exits during startup and the
launcher reports a port error several minutes later:

```
launching browser: There are no available ports between 8000 and 9000 for debugging
note: FLOWPROOF_HEADED is set, so Chromium was asked for a VISIBLE window. ...
```

The first line is the launcher's own; the note is flowproof naming the cause,
because nothing in "no available ports" suggests a missing display. Unset
`FLOWPROOF_HEADED` and it goes back to working.

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

## Test-context seeding: sessions, fixtures, and navigation

Real app suites don't rebuild their starting state through the UI in
every test: they inject it and start on the page under test. That
covers two idioms, and the `session:` block handles both:

- **an authenticated session** (Playwright's storageState pattern), so
  a flow skips the login UI;
- **an app-state fixture** (a pre-filled cart, a chosen project, a
  dismissed banner), so a flow skips the setup clicks that are not what
  it is testing.

Declare either in the spec; it's applied **before the page loads**
(cookies via CDP, localStorage before any page script runs), travels in
the trace with `${VAR}` references intact, and is re-applied identically
at every replay. Seeding runs **once, on the flow's first document**:
state the flow mutates afterwards (an item added to the seeded cart)
survives mid-flow navigation and reload instead of being reset to the
fixture, and that holds across a navigation that changes ORIGIN (a login
host to an app host) as well as within one:

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

Fixture values that are not secrets can be plain literals. A checkout
flow that needs an item already in the cart seeds it directly instead of
clicking through the catalog first:

```yaml
name: Checkout with a seeded cart
app: web
url: http://localhost:3000/cart.html
session:
  cookies:
    - name: session-username
      value: standard_user             # a plain literal is fine here
  local_storage:
    cart-contents: "[4]"               # the fixture the flow starts from
steps:
  - assert: the "css:.cart_item" appears 1 time
  - Press the "Checkout" button
```

Two notes on values. Real credentials and tokens always go through
`${VAR}` references: they resolve when the session is applied and the
trace stores only the reference, never the value. And seeded `${VAR}`s
are not automatically part of an `assert_no_secret_leak` scan; a flow
that seeds `${SESSION_TOKEN}` and wants leak coverage for it must list
it in the assertion explicitly.

`Go to` takes a path (resolved against the flow URL's origin) or a full
URL.

### Shared identities: declare once, reference by name

An access-control suite runs the same flows as several identities (a viewer,
an admin), so repeating the `session:` mapping in every flow is noise. Declare
each identity ONCE in the suite manifest under `identities:`, and reference it
from a flow by name. Each entry is exactly the inline `session:` shape
(cookies plus `local_storage`, values `${VAR}` refs resolved at apply time,
never stored):

```yaml
# suite.yaml
identities:
  viewer:
    cookies:
      - name: app.session
        value: ${VIEWER_SESSION_COOKIE}     # resolved at apply time, never stored
  admin:
    cookies:
      - name: app.session
        value: ${ADMIN_SESSION_COOKIE}
    local_storage:
      role: admin
```

The flow's `session:` field is an untagged string-or-mapping, distinguished
by YAML type the same way `app:` and `window:` are: a bare STRING names a
suite identity, a MAPPING is the inline setup you already write. So nothing
shipped changes meaning; existing specs keep their inline mapping.

```yaml
session: viewer          # a string: resolved against the suite's identities
```

**Dereference is a load-time copy, not a runtime lookup.** When the flow is
LOADED, the named identity's `${VAR}`-bearing setup is copied into the trace
header EXACTLY as an inline `session:` mapping is copied today, so the trace
stays self-contained: it carries the identity's setup, not a pointer to the
suite. A later edit to the suite's identity definition is therefore a
re-record or heal event on the flows that use it, never a silent change to
existing traces (the same rule as editing an inline `session:`). A bare
`session: viewer` in a flow with no governing `suite.yaml` is a load-time
error naming the missing suite; an unknown name is an error listing the
identities the suite declares.

**An identity carries the browser session only.** Cookies and `local_storage`,
nothing else. An access-control flow that also probes an API with
`assert_api` needs a bearer token (`${VIEWER_TOKEN}`), and that token is NOT
part of the browser session, so it does not live in the identity block. API
credentials stay plain suite `env` by convention: `${VIEWER_TOKEN}` is a suite
variable resolved at apply time like any other. The identity block is a
faithful mirror of the shipped `session:` shape, not a general credential
container.

For the control-authoring forms these identities feed (the `control:` block,
the denial pattern, `assert_no_secret_leak`, and `flowproof audit`), see
[authoring.md](authoring.md#security-controls).

## SAP GUI flows (Windows)

`app: sap` drives SAP GUI for Windows through **SAP GUI Scripting** — the
COM automation surface SAP ships — never through pixels or synthetic
keystrokes. Requirements: SAP GUI for Windows installed, scripting enabled on
the client and server (`sapgui/user_scripting = TRUE` in RZ11), and flowproof
on the same Windows machine. With `connection:` present, Flowproof starts SAP
Logon when needed, selects or opens that connection, and can complete the
standard SAP login screen from environment variables. Without `connection:`,
the flow remains attach-only and needs an existing logged-in session.

On the client, enable **SAP Logon Options → Accessibility & Scripting →
Scripting → Enable scripting**. The server setting alone is not sufficient.

```powershell
$env:SAP_CONNECTION = "S/4HANA Development" # SAP Logon entry description
$env:SAP_USER = "training-user"
$env:SAP_PASSWORD = "..."                    # never written to the trace
$env:SAP_CLIENT = "100"                      # optional
$env:SAP_LANGUAGE = "EN"                     # optional
```

For a non-standard installation, set `SAP_LOGON_EXE` to the full path of
`saplogon.exe`. Named connections wait up to 60 seconds by default; override
that for slow SAProuter landscapes with `FLOWPROOF_SAP_CONNECT_TIMEOUT_MS`.

### `login:` — the flow names its own user

Those variables are *process-global*, which is fine until a test case needs
two identities: a clerk creates the order, an approver releases it. One
process has one `SAP_USER`, so the second user could not be expressed at all.
A flow's own `login:` block can, and it needs no environment whatsoever:

```yaml
name: Clerk creates the order
app: sap
connection: TS3
login:
  user: obeva
  password: ${TS3_PASSWORD}   # a literal works too — see below
  client: "100"               # optional
  language: EN                # optional
steps:
  - Go to /nVA01
```

The two-user test case is then two flows in a suite, one `login:` each,
chained with [`exports:`](authoring.md#handing-a-value-to-the-next-flow-exports).
`login:` requires `connection:`: without one the flow would attach to
whatever session is already open, which may be a different user than the one
named — so that combination is a parse error rather than a surprise at run
time. When a flow has no `login:` block, nothing changes: the environment
pair still answers, exactly as before.

What holds:

- **The password never enters the trace.** It is not a header field, so
  there is nothing to redact and nothing to leak into a committed artifact.
  Only `login_user` travels, because a recording that cannot say which
  identity produced it is not reviewable.
- **Values resolve at the moment of use**, on record and on every replay —
  so `${TS3_PASSWORD}` picks up a rotated password rather than the one that
  was true when the trace was cut, and a literal password needs no
  environment at all. A literal stays in the spec file, which is then the
  only place it appears; prefer a `${VAR}` for anything you commit.
- **A session belonging to another user is never taken over.** Naming a user
  and silently driving somebody else's session would pass while proving
  nothing, so flowproof opens its own connection and logs in beside them.

```yaml
name: Create standard order
app: sap
connection: ${SAP_CONNECTION}   # SAP Logon entry to select/open; SAP Logon is
                                # started if needed. Omit for attach-only.
steps:
  - Go to VA01                                      # plain transaction code; Flowproof
                                                    #   records deterministic /nVA01
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

After an Enter or scripted control action, Flowproof reads SAP's status bar.
An SAP error or abort (for example, `This function is not possible`) now fails
that exact step with SAP's message instead of allowing recording to continue.

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

A repeated block with one value changing collapses into a `foreach`
values matrix — scalars use `${each}`, mappings use `${each.<key>}`
(whole-string tokens keep their YAML type, so `status: ${each.status}`
stays a number). Expansion happens at parse time: each iteration is an
ordinary recorded step.

```yaml
steps:
  - foreach:
      values: [mysql, mssql, oracle]
      steps:
        - assert_api:
            request: POST ${API}/connections/test
            body: { type: "${each}" }
            status: 500
            body_contains: "Database not yet supported!"
```

### Minting traces offline against a contract responder

Traces store only raw `${VAR}` references — verified end to end: no
resolved host, token, or connection string ever lands in the file. That
gives `app: api` flows a genuinely useful property: **recording against a
faithful local responder produces the same trace a live-stack recording
would** (only `trace_id`/timestamps differ). The official pattern for
minting api-flow traces without infrastructure:

1. Stand up a tiny local server speaking the endpoint's contract (the
   right paths, status codes, and body shapes — not the real logic).
2. Point the spec's `${VAR}`s at it and `flowproof record`.
3. Commit the trace. At replay, the same `${VAR}`s point at the real
   stack — the trace neither knows nor cares where it was recorded.

The in-repo proof is `crates/flowproof-cli/tests/api_pipeline.rs`: every
api-flow trace there is minted against a throwaway `tiny_http` responder
and replayed cleanly, with leak assertions on the secrets.

## Agent flows: test an AI agent (any OS)

`app: agent` tests an AI agent at the **model boundary** instead of a UI:
record its tool-call trajectory once against a real model, then replay it
deterministically with zero model calls. The spec drives the agent process,
mocks the tools at the boundary, and asserts the calls it makes:

```yaml
name: Weather assistant answers with the forecast
app: agent
agent:
  command: python3 examples/agent-demo/weather_agent.py
tools:
  - name: get_weather
    result: { city: Nairobi, sky: sunny, temp_c: 26 }
steps:
  - prompt: What is the weather in Nairobi right now? Use your tools.
  - assert_tool_call: get_weather where city contains Nairobi
  - assert: reply contains sunny
```

Recording needs a real model to record against; replay needs none. Point
flowproof at the upstream and give it a key, then record and replay:

```bash
export FLOWPROOF_AGENT_UPSTREAM=https://api.openai.com/v1   # or your endpoint
export FLOWPROOF_AGENT_KEY=sk-...                           # never enters the trace
flowproof record examples/agent-demo/weather.flow.yaml
flowproof run    examples/agent-demo/weather.flow.yaml      # zero model calls
```

flowproof spawns the agent, injects the proxy URL (`OPENAI_BASE_URL` and
friends) and the prompt (`FLOWPROOF_PROMPT`) into its environment, and
captures the trajectory into a cassette. The key rides only the outbound
`Authorization` header and is never written to disk. The full grammar and
runtime contract are in [agent-testing.md](agent-testing.md); the runnable
example is `examples/agent-demo/`.

## Authoring with a model (arbitrary steps)

In the default `--author auto` mode, a plain scalar UI step is
**natural-language model intent**:

```yaml
steps:
  - Enter 24 Market Street in the shipping address field
  - Press the Save button
  - assert: page shows Address updated
```

With an authoring model configured, `record` grounds the plain step against
the live scene on web and Windows desktop apps alike. The driver lists each
actionable or readable element with a provenance-neutral *target token*
(`css:#name` on the web, `id:15` / `text:Close` under UI Automation), and
the model must copy one of those listed tokens verbatim; it cannot invent a
selector. Assertions may also target the literal `surface` token:
everything readable on the current screen, whatever the driver.

The grounded actions and selector ladder are written to the trace. The
model is an author at recording time, not an executor at replay time:
`flowproof run` reads those persisted deterministic actions and makes zero
authoring-model calls.

Write what a person would do; selector and rule syntax are not required. This
includes dragging between visible regions, clicking a particular part of a
control, remembering a value or row count, choosing one or several options,
scrolling an embedded surface, typing in a same-origin frame, and moving focus
with a key. The live inventory includes rendered controls below the fold as
well as stable table, frame, and relational targets. The model may only choose
from that inventory, and Flowproof compiles the choice into the same
deterministic trace used by an explicitly rule-authored flow.

A step is a unit of **intent**, not a single click. One step may cover a whole
form:

```yaml
steps:
  - Fill out all the vehicle data and click next
  - Fill out all the insurant data and click next
  - assert: page shows Select Price Option
```

Each such step is still one model call: the model answers with the sequence of
grounded actions that carries it out, and the recorder performs and verifies
each one exactly as if it had been written out by hand. The trace that results
lists every action individually, so replay is no less deterministic than a flow
whose steps were spelled out field by field. The scene the model works from
carries what a form-filler needs — what each field currently holds, which ones
the page marks required, which boxes are ticked, and a dropdown's exact
options — so a chosen option is one the control actually offers. A password's
value is never included.

```bash
export FLOWPROOF_AI_PROVIDER=anthropic        # or openai-compatible
export FLOWPROOF_AI_API_KEY=sk-...            # falls back to ANTHROPIC_API_KEY
flowproof record shop.flow.yaml               # steps in your own words
flowproof run shop.flow.yaml                  # replays with ZERO model calls
```

Use `rules: <text>` when one step should bypass model authoring and use the
deterministic grammar explicitly:

```yaml
steps:
  - Enter the customer's new address
  - rules: Press the "Save" button
  - assert: page shows Address updated
```

`--author rules|llm|auto` controls the whole recording. `--author rules`
is the global deterministic opt-in for a flow already written in the
[rules grammar](authoring.md); `--author llm` forces model authoring for
plain UI steps. Structured steps such as `assert:` keep their own meaning.

If auto mode has no configured model, recording warns visibly and then
tries the deterministic rules for plain steps. It does not silently change
the route. Human output identifies each step as `rules`, `llm`, `reused`, or `fallback`,
and structured/JSON output exposes the same routing information without
requiring callers to parse terminal prose. The trace also records the
authoring backend and model whenever one participated.

Natural remembered values work across model-authored steps:

```yaml
steps:
  - Remember the order number
  - Enter it in the "Confirmation" field
```

`it` is accepted only when one remembered value is the clear candidate. If
the flow has remembered, for example, both an order number and a customer
number, an ambiguous `Enter it ...` stops with a structured clarification
listing the candidates instead of guessing. Give the value a natural name
to disambiguate it (`Remember the order number as the order ID`, then
`Enter the order ID ...`). For exact rule-authored flows, the explicit
`${captured.name}` syntax remains available:

```yaml
steps:
  - rules: Remember the "id:oid" as oid
  - rules: Type ${captured.oid} into the "Confirmation" field
```

For a local model, set `FLOWPROOF_AI_PROVIDER=openai-compatible` plus
`FLOWPROOF_AI_BASE_URL=http://localhost:8000/v1` (vLLM).

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
PASS: Add two numbers (2154 ms) -> .flowproof\runs\...\report.html
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
