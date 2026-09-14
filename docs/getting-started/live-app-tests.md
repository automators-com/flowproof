---
title: "Running the live-app tests"
description: "Running flowproof's own live-app test suite, including deploying a UWP app on a CI runner."
---

Two live-app tests drive real applications, both Windows-only and gated on
`FLOWPROOF_E2E=1` (the gate variable's name is a stable interface and
predates the current naming):

```powershell
$env:FLOWPROOF_E2E = "1"
cargo test -p flowproof-cli --test calc_e2e -- --nocapture     # needs a desktop VM
cargo test -p flowproof-cli --test notepad_e2e -- --nocapture  # also runs in CI
```

The Notepad one (`examples/notepad.flow.yaml`: type text, assert the
document contains it) runs automatically in CI on `windows-latest`, so the
record→replay spine is proven on every push. Calculator stays a manual VM
walkthrough because GitHub's Windows Server runners don't ship the
Calculator app.

### Deploying a UWP app on a CI runner

A Windows Server runner can still run a UWP app the suite needs: you
build and side-load it in the workflow. The sequence below is the one
that works for Microsoft's open-source Calculator (each step's obvious
alternative fails in a non-obvious way):

1. **Build the solution target, not the csproj**:
   `msbuild Calculator.slnx -t:Calculator`. Building the project file
   directly fails on project references that only resolve through the
   solution.
2. **Install the signing certificate into `TrustedPeople`**: the build
   signs the package with an ephemeral `SignTestApp` certificate;
   side-loading rejects it until that certificate is trusted,
   specifically in the **TrustedPeople** store, not Root or My.
3. **Side-load with the generated script**:
   `.\Add-AppDevPackage.ps1 -Force` (next to the built `.appx`/`.msix`)
   installs the package for the runner's user.
4. **Launch through the alias**: the System32 `calc.exe` stub now
   resolves to the dev build, so `app: calc` (or an `app:` mapping with
   `command: calc.exe`) drives it with no further wiring.

Two UWP-specific traps for specs and window handling: the visible window
belongs to **ApplicationFrameHost**, not the app's own process: target
windows by *title*, never by process; and the frame window is the one
`window:` geometry applies to.

SAP has three tiers: `sap_pipeline` (in-memory fake engine, every
platform, plain `cargo test`), `sap_sim_e2e` (the REAL COM engine against
a simulated scripting API: `tests/support/sap_simulator.py` registers
`SAPGUI` in the ROT as an item moniker, the way real SAP GUI does, and
serves SAP's object shapes; needs `pip install pywin32`, runs in windows
CI), and `sap_e2e` (a real SAP GUI session;
maintainer-run, `FLOWPROOF_E2E_SAP=1`).
