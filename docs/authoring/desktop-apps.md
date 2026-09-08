---
title: "Driving desktop apps"
description: "Mapping an arbitrary Windows app with app: and window: config, including UWP and packaged apps."
---

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
[Deploying a UWP app on a CI runner](../getting-started/live-app-tests.md#deploying-a-uwp-app-on-a-ci-runner):
a Windows Server image has no Store apps, but it can build and side-load
the one a suite needs.
