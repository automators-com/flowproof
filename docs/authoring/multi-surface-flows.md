---
title: "Multi-surface flows"
description: "Flows that cross apps: and in: blocks, one surface active at a time."
---

One test case, several technologies, one flow file, one trace. Declare the
surfaces under `apps:` and put steps in `in:` blocks; exactly one surface
is active at a time, and captures share one namespace across blocks:

```yaml
name: Order across GUI and portal
apps:
  gui: {app: sap, connection: "${SAP_CONNECTION}"}
  portal: {app: web, url: "${PORTAL_URL}/orders"}
steps:
  - in: gui
    steps:
      - Go to /nVA01
      # ... create and save the order ...
      - Go to /nVA02
      - Remember the "id:wnd[0]/usr/ctxtVBAK-VBELN" as order
  - in: portal
    steps:
      - Type ${captured.order} into the "Search" field
      - assert: page shows ${captured.order}
```

What holds, and why:

- **One surface active at a time.** SAP GUI scripting, UIA and vision all
  inject real input into the foreground window; sequential blocks are
  correctness, not a limitation. A block boundary launches its surface on
  the first visit and re-foregrounds it on returns — a later `in: gui`
  resumes the same session, same login, same screen.
- **Captures cross blocks.** The order number read off SAP's status bar
  types into the portal as `${captured.order}` — and the trace stores the
  NAME, never the value, exactly as in a single-surface flow. On replay
  the capture is re-read live from this run's SAP and typed into this
  run's portal.
- **Replay needs the trace and nothing else.** The header carries the
  surface map with config stored as written, so a `${VAR}` connection or
  url resolves fresh at every replay — and replay makes zero LLM calls,
  as always.
- **Steps author against their surface's own grammar** (what SAP performs
  differs from what a browser does), and out-of-band asserts
  (`assert_api`, `assert_sql`, `assert_spreadsheet`) run fine inside any
  block.
- **A web surface carries its own `browser:`** — viewport/device
  emulation, user-agent, pinned clock, seeded random, exactly the
  single-surface block, one level deeper:

  ```yaml
  apps:
    portal:
      app: web
      url: "${PORTAL_URL}/orders"
      browser:
        viewport: {width: 390, height: 844, mobile: true, touch: true}
  ```

  It travels on the surface's header entry, so record and every replay
  launch that surface the same shape. On a non-web surface it is a parse
  error naming whose config it is. `browser:` also takes `downloads_dir`
  (where downloaded files land — absent means the surface creates its own
  per-launch temp directory), the field `Wait until the download completes
  as <name>` reads back.
- **A surface launched with a Windows `command`/`window_title` may
  reference `${captured.x}`** — a value an EARLIER block captured, resolved
  at THIS surface's actual activation rather than before any step has run
  (a value that does not exist yet cannot resolve). This is the seam that
  lets one block download a file and a later block, in a different
  application, open it:

  ```yaml
  apps:
    fiori: {app: web, url: "${FIORI_BASE_URL}/ui#Shell-home"}
    excel: {app: {command: "EXCEL.EXE ${captured.pir_export}", window_title: "Excel"}}
  steps:
    - in: fiori
      steps:
        - Press the "Export" button
        - Wait until the download completes as pir_export
    - in: excel
      steps:
        - assert: page shows Net Price
  ```

  An unresolved capture at activation time (the minting block never ran, or
  ran on the wrong surface) fails the run closed, naming what was missing —
  never a launch against the literal `${captured.x}` text.
- **A desktop surface carries its own `window:`** — for `vision`, the
  `title:` names the window pixels mode attaches to (required there, and
  what finally makes a Citrix/RDP-published app a surface); for `sap` and
  windows-mapping surfaces, `width`/`height` (optionally `x`/`y`) pin the
  shape at the surface's FIRST activation, recording what was applied so
  replay reproduces it exactly:

  ```yaml
  apps:
    citrix:
      app: vision
      window: {title: "Citrix Receiver", width: 1280, height: 720}
  ```

  On a web surface `window:` is a parse error — a page is sized with
  `browser: viewport`.
- **`assert_screenshot` works in any block, and its baseline names its
  surface**: the stored identity is `<name>@<surface>.png`, so two blocks
  may reuse one spec name and a `gui` baseline can never be compared
  against a `portal` frame. (`@` in a spec-chosen name is refused for
  exactly that reason.)
- Surface kinds are UI kinds: `agent` (chain an `app: agent` flow in the
  suite instead) and `api` (nothing to drive) are refused at parse, each
  with its reason — as are flow-level `session`/`mock`/`redact` (and
  flow-level `browser:`/`window:`, which moved into the surface entries).
- **`flowproof heal` works on multi-surface flows** — healing is
  re-record-plus-diff, so it runs on the same surface registry recording
  uses, and a step that moved between surfaces is flagged as a `surface`
  change: the same action against another app is not the same step.

When the case is "do in system A, prove in system B" with no ping-pong, a
suite of single-surface flows chained with
[`exports:`](#handing-a-value-to-the-next-flow-exports) is still the
simpler spelling — one driver per flow, per-flow verdicts, and the same
value handoff.
