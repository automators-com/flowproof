---
title: "Web flows"
description: "Headed vs. headless replay, the web action and assertion vocabulary, and out-of-band assertions for web flows."
---

The same spine drives browsers through the `web` adapter; this works on
Linux and macOS too, since it runs on Chromium rather than Windows UIA.
`record` shows the browser by default so you can watch it work; `run`
(replay) stays headless, which is right for CI. See
[watching the browser](#watching-the-browser-headed-and-headless) to change
either. Specs add a `url:` and use the web vocabulary
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

### Watching the browser: headed and headless

`flowproof record` shows the browser by default: recording is the one
human-in-the-loop step, and watching a misread selector fail live is far
faster than debugging it blind against a `debug/dom.html` dump. `flowproof
run` (replay) stays headless by default, because it is meant to run
unattended in CI, often on a runner with no display at all.

Override either with a flag, for one invocation:

```bash
flowproof record web.flow.yaml --headless   # e.g. a scripted/CI recording pass
flowproof run web.flow.yaml --headed        # watch a replay while debugging it
```

`--headed` and `--headless` are mutually exclusive and take priority over
everything else, including the environment variable below.

**If you script `flowproof record`** (in CI, a container, or any pipeline
that generates cassettes without a person watching), pass `--headless`
explicitly. Recording now opens a window by default, and a host with no
display cannot give it one (see the port-timeout note further down).

The same choice is also available as `FLOWPROOF_HEADED`, which both commands
still honor when no flag is passed:

```bash
FLOWPROOF_HEADED=1 flowproof run web.flow.yaml
```

```powershell
$env:FLOWPROOF_HEADED = "1"; flowproof run web.flow.yaml
```

It is presence-based, like `FLOWPROOF_NO_SHARED_BROWSER`: `FLOWPROOF_HEADED=0`
still shows the window, because a variable you bothered to set is one you meant.
Unset it, or pass `--headless`, to go back to headless.

Deliberately an environment variable (and now flags) rather than a spec
field: watching is a property of the run you are supervising, not of the
flow. A committed `headed: true` would follow the flow into CI, where nobody
is watching and there may be no display at all.

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
screenshot is 1280x720 but baseline 'checkout' is 800x600, viewport changed? re-record to refresh the baseline
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

Flows without visual assertions are unaffected: the DOM does not change shape
because a window is visible.

**It needs a real desktop session.** Over SSH, in a container, or on a CI
runner there is no window to show, so Chromium exits during startup and the
launcher reports a port error several minutes later:

```
launching browser: There are no available ports between 8000 and 9000 for debugging
note: FLOWPROOF_HEADED is set, so Chromium was asked for a VISIBLE window. ...
```

The first line is the launcher's own; the note is flowproof naming the cause,
because nothing in "no available ports" suggests a missing display. This
happens with `FLOWPROOF_HEADED`, with `--headed`, and with a plain `flowproof
record` on a display-less host: recording is headed by default. Pass
`--headless`, or unset `FLOWPROOF_HEADED`, and it goes back to working.

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

Text anchors match exactly first, then by prefix: `Click "Database"`
finds the card whose label *starts with* "Database" when no element matches
it exactly, mirroring how Playwright's accessible-name matching is used in
real suites.

### The assertion vocabulary (shared across every app profile)

Assertions describe **what** to check; **how** each target resolves is the
adapter's job, so the same forms work for web, desktop (UIA), SAP GUI, and
vision/OCR. All forms auto-wait
(bounded, recorded timeout; `within <N>s` overrides), including waiting
for the *target itself* to appear, so asserting on a toast works:

```yaml
steps:
  - assert: page shows Welcome                       # the SURFACE: page text on
                                                     #   web, window subtree on
                                                     #   UIA, OCR frame on vision
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
`toHaveValue`, `toBeVisible`, …) are the **web mapping** of these forms,
one provenance among four (uia, sap-com, vision/OCR, out-of-band), not
their definition. `calc` and `notepad` layer their sugar (`display shows`,
`document contains`) on top of the same shared grammar.

### Out-of-band assertions: the posted record, not the pixel

Enterprise correctness often lives in the database or behind an API, not
on screen. Structured steps probe it directly, app-independent,
auto-waiting like every other assertion, and replayed with zero model
calls:

```yaml
steps:
  - Press the "Save" button
  - assert_sql:
      connection: reporting            # env FLOWPROOF_SQL_REPORTING holds the
      query: >                         #   postgres connection string, the
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
the `FLOWPROOF_SQL_<NAME>` variable, never a silent pass.

(YAML note: an `assert:` value cannot *start* with a `"`, that's why
quoted targets always follow `the `.)
