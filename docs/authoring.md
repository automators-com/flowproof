# The authoring grammar — every accepted form

`record` resolves each spec step with deterministic rules first; anything
the rules cannot parse falls back to the LLM author (when a model backend
is configured). This page is the **complete rules grammar**. Nothing here
requires a model call, and everything here is covered by a test that
parses the exact examples shown (`documented_grammar_examples_all_resolve`
in `crates/flowproof-agent/src/rules.rs` — if the doc and the code drift,
CI fails).

Conventions: forms are case-insensitive in their keywords. `<text>` is
literal text (may carry `${VAR}` secret references). A quoted `"<label>"`
is a **text anchor** — matched against visible text, accessible label
(`aria-label`), placeholder, or an associated `<label>` (both
`<label>Name: <input/></label>` wrapping and `<label for>`/`id` pairing).
Matching is exact first, then prefix (`"Name"` finds the field labelled
`Name:`), then ASCII case-insensitive as a last resort (`"Close Account"`
still finds the button reading `Close account`) — a case-sensitive match
always wins. `page shows` reads visible text **plus** the accessible names
of visible elements, so icon-only buttons that exist purely as an
`aria-label` count. Assertion TEXT matches the same way selectors do:
exact first, then case-insensitive (`page shows Close Account` passes
against a page reading `Close account`), and the negative forms mirror
it — if `shows X` would pass, `does not show X` fails. Two escape
hatches work inside any
quoted label: `"css:<selector>"` (web) and `"id:<native id>"` (DOM id,
UIA AutomationId, SAP scripting id). `[2nd ]` marks an optional 1-based
ordinal (`2nd`, `3rd`, `10th`) for when several elements match.

## Actions (web, sap, vision — the generic grammar)

| Step | Notes |
|---|---|
| `Type <text> into the [2nd ]"<label>" field` | text anchor / `css:` / `id:` |
| `Type <text> into the <id> field` | bare native id |
| `Type <text>` | types into the FOCUSED element |
| `Replace the [2nd ]"<label>" field with <text>` | clear + type, one step |
| `Replace the <id> field with <text>` | |
| `Clear the [2nd ]"<label>" field` / `Clear the <id> field` | fill-with-empty semantics |
| `Remember the [2nd ]"<target>" as <name>` | read the target's text into a flow-scoped name (`[a-z][a-z0-9_]*`) for a later assertion to compare against. The VALUE is read at execution time on record and on every replay, so it never enters the trace - the same indirection `${VAR}` secrets use. Re-using a name overwrites it |
| `Check the [2nd ]"<label>" checkbox` / `Uncheck the …` | drives a checkbox, radio, or `role=switch` to a STATE, not a toggle: `Check` on an already-checked box is a no-op, so the step means the same thing however the environment arrives. Resolves the control inside a wrapper too (the common pattern of a visually hidden `input` inside a styled label), performs a real click so the app's own handlers fire, then verifies the state took |
| `Select <option> from the [2nd ]"<label>" field` | native `<select>`: committed via the value setter, fires `input`+`change` (React-safe). `in the` and `… dropdown` also accepted |
| `Press the [2nd ]"<label>" button` / `Press the <id> button` | |
| `Right-click [the [2nd ]]"<text>"` | opens the element's context menu; `Right click` also accepted |
| `Upload <path> into the [2nd ]"<label>" field` | sets a file on a file-chooser input (may be hidden behind a styled button); relative paths resolve against the working directory at execution |
| `Upload <path> into the <id> field` | |
| `Click [the [2nd ]]"<text>"` | tabs, links, menu options, rows |
| `Press <Key>` / `Press <Mod>+<Key>` | `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Space`, arrows, `Home`/`End`, `PageUp`/`PageDown`; chords `Control+V`, `Alt+Shift+Backspace`. `Mod` (aliases `CtrlOrMeta`, `ControlOrMeta`) is the **portable** primary modifier: stored neutrally in the trace and resolved at execution — Meta on macOS, Ctrl elsewhere — so `Press Mod+K` recorded on a Mac replays on Linux CI |
| `Go to <path-or-URL>` / `Navigate to <path-or-URL>` | relative paths resolve against the flow URL's origin; on SAP this is transaction navigation (`Go to /nVA01`) |
| `Reload the page` | web |
| `Wait until page shows <text> [within <N>s]` | long-bound auto-waiting assert (default 60s) |

There is deliberately **no `Blur` step**. Blur is not something a user does;
it is a DOM event that a user action causes. `Press Tab` is that action, it
already works, and it additionally tests what the user really experiences -
that focus lands somewhere sensible. Blur-triggered form validation is
exercised with `Press Tab`.

## Assertions (every app — the shared grammar)

All assertion forms **auto-wait** (default 10s, recorded into the trace);
append `within <N>s` to any form to change the bound.

| Assert | Meaning |
|---|---|
| `page shows <text>` | the whole surface (page text / window subtree / SAP session / OCR frame) contains `<text>` — `the page shows <text>` also accepted |
| `page shows <text> <N> times` | exact occurrence count of the TEXT |
| `page does not show <text>` | waits for it to be GONE |
| `page url is <expected>` | the surface's URL. A `<expected>` starting with `/` compares the PATHNAME exactly, including the query only when `<expected>` carries a `?` and the fragment only when it carries a `#` (so `/orders` ignores `?page=2`); one containing `://` compares the whole URL exactly. Web flows only: a window or an OCR frame has no URL, and the error says so |
| `page url contains <text>` | substring of the whole URL |
| `the [2nd ]"<label>" field contains <text>` | input VALUE, by label |
| `the <id> field contains <text>` | input VALUE, by native id |
| `the [2nd ]"<target>" shows <text>` | element-scoped substring |
| `the [2nd ]"<target>" shows ${captured.<name>}` | compare against a remembered value: text, with the same matching ladder as any `shows` |
| `the [2nd ]"<target>" shows ${captured.<name>} + <number>` / `- <number>` | compare NUMERICALLY against the remembered number offset by a literal, e.g. `the "Balance" shows ${captured.balance} - 100`. Currency symbols and thousands separators are ignored on both sides |
| `the [2nd ]"<target>" is visible` / `is not visible` | target resolves / does not resolve |
| `the "<target>" appears <N> times` | how many ELEMENTS match the anchor. Exact, not a minimum. No ordinal: `the 2nd "Row"` is one element by construction, so counting it has no answer |
| `the [2nd ]"<target>" is enabled` / `is disabled` | platform enabled state (`disabled`/`aria-disabled` on web, UIA IsEnabled on desktop) |
| `the [2nd ]"<target>" checkbox is checked` / `is not checked` | checkbox state, read from the `checked` property or `aria-checked`. A target that is not a checkbox fails as exactly that, not as "wrong state" |
| `the "<target>" is empty` / `is not empty` | the target's trimmed visible text (or input value) is empty. A first-class predicate: `shows ""` cannot express it |
| `the "<column>" column of the row containing "<anchor>" <predicate>` | a table cell, by IDENTITY. See below |

Two different questions share the word "times", and picking the wrong one
is a quiet way to write a test that cannot fail:

```yaml
- assert: page shows Pending 3 times      # the TEXT appears 3 times anywhere
- assert: the "css:.order-row" appears 3 times   # 3 ELEMENTS match
```

A list assertion almost always wants the second. Three rows whose labels
happen to repeat a word are still three rows, and a row that renders its
status twice would satisfy the first without any row existing at all.

Counting rides on the same ordinal as `the 2nd "Row"`, so it means on each
adapter exactly what an ordinal means there: DOM order on web, UIA tree
order on the desktop, reading order under vision. A passing count costs
`N + 1` questions to the app; only a FAILING one counts further, so the
error can say `found 5` rather than just "not 3".

The URL forms map `cy.location("pathname").should("equal", "/signin")` and
`cy.url().should("include", "checkout")`, and they auto-wait like every other
assertion, because an SPA redirect lands asynchronously:

```yaml
- assert: page url is /signin
- assert: page url contains checkout
- assert: page url is /orders?page=2 within 15s
```

Checkboxes map `cy.check()` / `should("be.checked")`:

```yaml
- Check the "Remember me" checkbox
- assert: the "Remember me" checkbox is checked
- Uncheck the "Remember me" checkbox
- assert: the "Remember me" checkbox is not checked
```

### Table cells, by identity

A cell in a grid is addressed by the column's header text and an anchor that
identifies the row — never by position:

```yaml
- assert: the "Status" column of the row containing "Grace Hopper" shows Suspended
- assert: the "Balance" column of the row containing "Grace Hopper" is empty
- Click the "Actions" column of the row containing "Grace Hopper"
```

The same cell target composes with every predicate (`shows`, `is empty`,
`is [not] visible`, `is enabled`, `checkbox is [not] checked`) and every
action (`Click`, `Type … into`, `Clear`, `Check`). `in the row containing`
also works — the of/in coin flip is one you should not have to remember.

Why identity, not `the 2nd ".column-status"`: an ordinal encodes position,
so inserting a row or reordering a column silently makes the assertion hit
the wrong record. Identity survives both — the trace records the header
text and the row anchor, and replay finds them wherever they moved. For
that reason an ordinal cannot address a cell (`the 2nd "Status" column …`
is a parse error).

Resolution is generic over any `<table>` or ARIA grid (`role=grid`/`table`/
`treegrid`), so react-admin, MUI DataGrid and AG Grid all work with no
framework-specific selector. Two things are hard errors rather than a
silent wrong guess, and both point at the `css:` escape hatch: a row anchor
that matches more than one row (`use a more specific anchor`), and a
duplicate column header. **Known boundary:** a virtualized grid that keeps
off-screen rows out of the DOM (AG Grid's row virtualization) can only be
addressed for rows that are rendered; scroll the anchor row into view
first, or use `css:` against the grid's own row API.

Computed assertions answer "did this change by the right amount?", which a
literal cannot express because the starting value is only known at run time:

```yaml
- Remember the "Account Balance" as balance
- Press the "Pay" button
- assert: the "Account Balance" shows ${captured.balance} - 100
```

The expression grammar is deliberately tiny and does not compose: one
capture reference, optionally one `+` or `-`, and one plain number. There is
no second capture, no nesting, no `*` or `/`. A capture may only be
referenced in an ASSERTION - using one in an action is a parse error,
because that would let the app under test steer execution.

## Driving an arbitrary Windows app (`app:` mapping, `window:` config)

`app:` is normally a registry id (`web`, `calc`, `notepad`, `sap`, `vision`,
`api`). It also accepts a mapping, which drives any Windows program through
UI Automation:

```yaml
app:
  command: '"C:\Program Files\My App\app.exe" --profile=test'
  window_title: ${APP_WINDOW}
window:
  width: 1280
  height: 800
```

`command` is a command LINE, not a program name: the program may be quoted
so a path with spaces survives, and everything after it reaches the app
verbatim. Both fields take `${VAR}` references, resolved at launch and
stored RAW in the trace. `command` is executed code, the same trust surface
as a suite's `env_from`: a spec is code.

`window:` pins the window's shape, which is a determinism precondition for
visual assertions rather than something a user does - so it is config,
applied once before the first step and identical at record and replay, not a
step. `width` and `height` go together; `x` and `y` are optional but go
together and need a size. Geometry values are literal integers, never
`${VAR}`: a precondition that varies by environment is not one. The trace
records what was APPLIED, so a spec that gives only a size still pins the
position the window landed on.

A vision flow names the window it attaches to in the same block, and may
pin geometry too - which is where it matters most, because OCR baselines
depend on it:

```yaml
app: vision
window:
  title: Citrix Receiver
  width: 1280
  height: 720
```

Each app kind has exactly ONE spelling for naming a window:
`app.window_title` for a Windows program flowproof launches, `window.title`
for a window vision attaches to but never launched. Using the wrong one is a
parse error that names the right one. A web flow sizes its page with
`browser: viewport`, and an api flow has no window at all.

### UWP and packaged apps

A UWP app (Calculator, Settings, anything from the Store) is not an exe you
launch by path. Launch one through the shell, naming the package by its
Application User Model ID:

```yaml
app:
  command: explorer.exe shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App
  window_title: Calculator
window:
  width: 640
  height: 900
```

`explorer.exe` returns immediately, before the app has a window, which is
exactly why `window_title` exists: flowproof waits for a window with that
title rather than for the process it spawned. List the ids on the machine
with `Get-StartApps` in PowerShell.

The window matters for geometry. A UWP app draws into a
`Windows.UI.Core.CoreWindow` hosted inside an `ApplicationFrameWindow` that
belongs to `ApplicationFrameHost.exe`, and the CoreWindow does not own its
own size - resizing it does nothing visible. flowproof detects the
CoreWindow class and applies `window:` to the hosting frame instead, so a
UWP flow pins its shape like any other. Nothing to configure; worth knowing
only when a resize appears to be ignored.

For running a UWP app on a CI runner that does not ship one, see
[Deploying a UWP app on a CI runner](getting-started.md#deploying-a-uwp-app-on-a-ci-runner):
a Windows Server image has no Store apps, but it can build and side-load
the one a suite needs.

## Out-of-band assertions (any app; structured steps, not prose)

```yaml
- assert_sql:
    connection: reporting        # resolved from FLOWPROOF_SQL_REPORTING
    query: SELECT count(*) FROM orders WHERE ref = '4711'
    equals: "1"
- assert_api:
    request: GET ${API}/orders/4711
    status: 200
    body_contains: "confirmed"
- assert_api:                    # authenticated JSON POST
    request: POST ${API}/connections/test
    headers:
      Authorization: Bearer ${SESSION_TOKEN}
    body:
      provider: postgres
      connectionString: ${TEST_CONN_STRING}
    status: 200
    body_contains: "Database not yet supported!"
```

`headers` values and `body` string values may carry `${VAR}` refs — the
trace stores only the raw reference; tokens and connection strings resolve
when the probe fires (record and every replay). `body` is any YAML
(mapping, list, or string), sent as JSON with `content-type:
application/json` unless you set your own `content-type` header — yours
wins. A `body` on GET/HEAD/DELETE is rejected at parse time.

## Visual assertions (structured step)

```yaml
- assert_screenshot:
    name: dashboard              # baseline PNG name (no path, no extension)
    mask: ["css:.clock", "Sync"] # optional: selectors blanked before compare
    threshold: 0.001             # optional: fraction of pixels allowed to differ (default 0)
```

`record` captures the surface, blanks each mask's element rect, and mints
`<spec-stem>.baselines/<name>.png` next to the trace — re-recording (or
`record --reuse`) is how baselines refresh. Replay captures with the
**same masks** and compares pixel-exact (up to `threshold`); on failure
the run bundle gains `visual/<name>.actual.png` and `visual/<name>.diff.png`
(differing pixels in red) and the message names the diff percentage.
Masks take the same forms as quoted labels (text anchor, `css:`, `id:`)
and every mask must resolve — a silently-unmasked volatile region would
mint a flaky baseline. Pin the viewport with `browser:` so capture
dimensions stay stable across machines.

## Network mocks (web flows; spec-level, not steps)

```yaml
mock:
  - url_contains: /api/rates          # substring match on the request URL
    method: GET                       # optional; any method when absent
    status: 200                       # optional; default 200
    body:                             # any YAML: string served verbatim
      rate: 1.23                      #   (text/plain), anything else as
      source: mocked                  #   JSON; content_type: overrides
```

Requests matching a rule are answered inside the browser — the real host
is never contacted (it need not even exist). The rules travel in the
trace header and apply **identically at record and replay**: what was
mocked once is mocked always, which is what keeps the two executions
equivalent. Mocked responses carry permissive CORS headers and answer
preflights, so cross-origin `fetch()` calls just work. The tool for
third-party calls (payments, analytics) and hard-to-provoke server
states; for asserting on real APIs, use `assert_api` instead.

## Browser config (web flows; spec-level, not steps)

```yaml
browser:
  viewport:                   # device emulation, applied before navigation
    width: 390
    height: 844
    device_scale_factor: 3    # optional; default 1
    mobile: true              # optional; mobile layout + meta-viewport
    touch: true               # optional; emulate a touch screen
  user_agent: my-agent        # optional; navigator.userAgent override
  args: ["--lang=en-US"]      # optional; extra Chrome flags
```

The config travels in the trace header and applies **identically at
record and replay** — a flow recorded on an emulated phone never replays
on a desktop viewport. This is how `*.mobile` test variants and
deterministic-seeding user agents (previously an env-var wrapper around
Chrome) become first-class. `args` forces a private (non-shared) browser
for the flow, since flags only apply at process start — expect its cold
start. A suite's `suite.yaml` may carry the same `browser:` block as a
default for every flow; a flow's own block wins outright.

## App sugar

Sugar is an alias layer, not a cage: on every UIA-driven app (`calc`,
`notepad`, and the `app:` mapping form) the full shared action grammar
applies too — `Press the "<label>" button`, `Click "<text>"`, `Type <text>
into the "<label>" field`, `Press Ctrl+S`, `id:` targets and ordinals all
act on any control the app shows, menus and dialogs included. Sugar wins
where it matches; everything else falls through to the shared forms.

- **calc**: `Type <digits>` (one press per digit), `Press
  plus|minus|times|divided by|equals`, `assert: display shows <number>`.
  Keys the sugar never named are shared-grammar presses: `Press the
  "Square root" button`, `Click "History"`.
- **notepad**: `Type <text>` types into the *document*; the targeted form
  `Type <text> into the "<label>" field` addresses a dialog's field (Find,
  Replace, Save As) instead. `assert: document contains <text>` (plus the
  shared grammar).

## When a step doesn't parse

The error names the accepted forms for that app. Anything freeform (e.g.
`Smash the shiny button`) is handled by the LLM author when a model
backend is configured (`FLOWPROOF_AI_PROVIDER` / `FLOWPROOF_AI_API_KEY`)
— the model grounds the step against the live scene and can never invent
a selector; replay stays zero-model either way. See
[getting-started](getting-started.md#authoring-with-a-model-arbitrary-steps).

When a step is too *ambiguous* to author at all ("make required field
changes" — which fields?), recording fails with a structured
**clarification payload**: the stuck step plus the live screen's field
inventory, via `record --json`, the MCP record tool, or Python's
`ClarificationNeeded`. The driving agent rewrites the step into concrete
grammar and re-records — see [self-help.md](self-help.md) for the loop.
