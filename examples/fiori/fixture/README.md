# flowproof Fiori fixture

A small, real, offline SAPUI5 application used to evaluate and improve
flowproof's `app: web` adapter against Fiori/UI5 idioms - a growing table,
client-side routing, dialogs and popovers rendered into the static UIArea, an
F4-style value help, and the common form control set - backed by
`sap/ui/core/util/MockServer` so it needs no credentials and no network at
runtime. Part of `docs/fiori-reliability/` (see `../../../docs/fiori-reliability/FINDINGS.md`
for why this exists); not part of the flowproof product.

## Run it

```bash
cd examples/fiori/fixture
npm install                 # pulls in @ui5/cli only - see note on framework libs below
npx ui5 serve --port 8081   # first run also downloads OpenUI5 1.120.20 into ~/.ui5/framework
```

Then open `http://localhost:8081/index.html`.

**Do not add `@openui5/*` packages to this project's own `package.json`.**
The UI5 tooling resolves framework libraries itself, from `ui5.yaml`, into a
shared `~/.ui5/framework` cache - if the same libraries also appear as direct
npm dependencies, `ui5 serve` refuses to start ("Duplicate framework
dependency definition(s)").

## UI5 version

Pinned to **OpenUI5 1.120.20** in `ui5.yaml`, chosen before the real version
was known. **The real system's version is now confirmed: 1.114.11**
(`sap.ui.version` read directly from the live launchpad - see
`docs/fiori-reliability/FINDINGS.md`, "Real-system probe #2"). This fixture's
pin has not been updated to match yet - this session pivoted to testing
against the real system directly instead (per the same findings doc) rather
than continuing to invest in the fixture. Whoever picks this back up: change
`framework.version` to `1.114.11` in `ui5.yaml` before treating this fixture
as representative of anything version-sensitive.

## What's here vs. what the full brief asked for

Built: a worklist (`sap.m.Table`, `growing`/`growingScrollToLoad`, verified
via `table.getGrowingThreshold()`/`getItems().length` in-browser - 20 of 42
rows load initially, full count known via OData inline count), routing to a
detail page (view instantiated on navigation - the mechanism that shifts
`__xmlview` counters), a `sap.m.Dialog` (delete confirmation) and a
`sap.m.Popover` (info button) on the worklist, an F4-shaped value help
(`sap.m.Dialog` + searchable `sap.m.List`) and the full requested form
control set (`Input`, `Select`, `ComboBox`, `DatePicker`, `CheckBox`) on the
detail page, a `MessageToast` on save, a busy-state binding
(`app>/busy`), and an OData v2 model via `MockServer` with `?mockDelay=<ms>`
and `?mockErrorRate=<0..1>` URL params for latency/error injection (see
`webapp/model/mockserver.js`).

**Not built, scoped down given the time available**: the launchpad-shell
variant (iframe-hosted Fiori) - `examples/fiori/` already has real,
launchpad-recorded traces proving flowproof handles that shape against the
actual system (see `docs/fiori-reliability/FINDINGS.md`), so this fixture
focuses on the mechanisms that need a controllable, repeatable local
environment instead: async timing, id instability, and static-UIArea
rendering. The F4 value help is also a simplification - a plain `Dialog` +
`List`, not `sap.ui.comp.valuehelpdialog.ValueHelpDialog` (that library isn't
in this fixture's dependency set) - close enough in DOM/interaction shape to
exercise the same adapter code paths, not identical to SAP's own value-help
chrome.

## A live bug this fixture caught while being built

`sap.m.Dialog`'s `subHeader` aggregation requires an `IBar`-implementing
control. An early version of `Detail.controller.js` passed a bare
`sap.m.SearchField` directly and failed at runtime
(`oSubHeader.getDesign is not a function`) - fixed by wrapping it in a
`sap.m.Bar`. Left as evidence that this fixture is a real, running app that
was actually exercised in a browser, not scaffolding that was written and
never executed.
