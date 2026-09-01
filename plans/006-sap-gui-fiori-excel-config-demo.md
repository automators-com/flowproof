---
status: draft
---
# Plan 6 — SAP GUI to Fiori to Excel, with `flowproof config` as the setup story

[Issue #536](https://github.com/automators-com/flowproof/issues/536):
"SAP GUI flow continuing into Fiori, with flowproof config." The requested
artifact is a several-minute walkthrough that starts in SAP GUI, continues into
SAP Fiori as the same test case, demonstrates `flowproof config` replacing
checkout-local `.env` setup, and ends in Excel by opening or asserting against
an exported spreadsheet.

This plan treats the issue as a release-demo example first, not as a new engine
feature unless live recording proves a real gap. The important thing to show is
that the features already shipped in plans 1, 4, and 5 compose in a single,
credible SAP workflow.

## What already exists

The core machinery is already present:

- `flowproof config sap` and `flowproof config fiori` seed SAP GUI and Fiori
  credential/profile variables from a per-machine config file, fill-gaps-only,
  before `${VAR}` resolution. No repo-local `.env` is required.
- Business data values can live beside a flow in `<flow-stem>.values.yaml`,
  keeping credentials out of the example package while still making the flow
  portable.
- Multi-surface flows are shipped: one `apps:` map, ordered `in:` blocks, one
  trace, one shared capture namespace, and lazy surface activation.
- Fiori downloads are shipped: `Wait until the download completes as <name>`
  captures the resolved downloaded file path.
- Excel does not need a bespoke adapter. A later Windows surface can launch
  `EXCEL.EXE ${captured.export_path}`, and `assert_spreadsheet` reads the file
  directly through `calamine` instead of relying on Excel grid UI Automation.
- `examples/fiori/purchase-info-records-report.flow.yaml` already proves the
  Fiori-to-Excel shape. What it does not prove is a preceding SAP GUI block in
  the same flow and the release-notes setup story around `flowproof config`.

The one known limitation that matters here: multi-surface `login:` blocks are
still refused because surface-specific credentials are not staged yet. This
demo should not use surface `login:` blocks. SAP GUI and Fiori credentials come
from `flowproof config` / environment seeding instead, and the SAP GUI surface
uses `connection: ${SAP_CONNECTION}`.

## Proposed artifact

Add a new example package:

```text
examples/release-notes/
  sap-gui-fiori-excel.flow.yaml
  sap-gui-fiori-excel.values.yaml.example
  README.md
```

After live recording, the package should also contain:

```text
examples/release-notes/
  sap-gui-fiori-excel.trace.jsonl
```

The example should be a single multi-surface flow:

```yaml
name: SAP GUI to Fiori to Excel release walkthrough
apps:
  gui:
    app: sap
    connection: ${SAP_CONNECTION}
  fiori:
    app: web
    url: ${FIORI_BASE_URL}?sap-client=${FIORI_CLIENT}&sap-language=${FIORI_LANGUAGE}#Shell-home
  excel:
    app:
      command: EXCEL.EXE ${captured.fiori_export}
      window_title: Excel
steps:
  - in: gui
    steps:
      # Start from a native SAP GUI transaction using the same business data
      # that the Fiori report will later search/export.
      - Go to /nME1L
      - Type ${SUPPLIER} into the Supplier field
      - Type ${MATERIAL} into the Material field
      - Type ${PLANT} into the Plant field
      - Press Enter
      - Remember the accepted material as gui_material

  - in: fiori
    steps:
      - Type ${FIORI_USER} into the "User" field
      - Type ${FIORI_PASSWORD} into the "Password" field
      - Press the "Log On" button
      - Wait until page shows Home within 60s
      - Wait until page shows ${REPORT_TILE} within 20s
      - Click ${REPORT_TILE}
      - Wait until page shows Info Records per Supplier within 20s
      - Type ${captured.gui_material} into the "Material" field inside iframe "Application"
      - Type ${SUPPLIER} into the "Supplier" field inside iframe "Application"
      - Type ${PLANT} into the "Plant" field inside iframe "Application"
      - Press the "Go" button
      - Press the "Export" button
      - Wait until the download completes as fiori_export within 60s

  - in: excel
    steps:
      - assert: page shows Net Price
      - assert_spreadsheet:
          path: ${captured.fiori_export}
          column: Net Price
          row_contains: ${captured.gui_material}
```

The concrete SAP GUI field identifiers and final export control must still be
captured against the live target system at record time. Use direct SAP scripting
ids where SAP GUI labels are ambiguous, and use direct WebGUI selectors in the
Fiori iframe if the generic labels do not replay reliably.

## Resolved workflow choices

- SAP GUI should start with `/nME1L`, the read-only "Purchasing Info Records by
  Supplier" report. It is the native SAP GUI counterpart to the Fiori
  "Display Purchasing Info Record by Supplier" route, uses the same Supplier /
  Material / Plant business data, and avoids mutating records in a release demo.
- Business data should come from
  `MM_PUR_INFO_RECORDS_MANAGE_SRV/C_PurInfoRecordWithOrg`. The live OData probe
  returned a plant-scoped row with the fields the demo needs:

  ```text
  MATERIAL: TG10
  SUPPLIER: "10300001"
  PLANT: "1010"
  NET_PRICE: "12.35"
  REPORT_TILE: Display Purchasing Info Record by Supplier
  ```

  The values are non-secret SAP business identifiers and can be committed in the
  example values file. If the reference tenant changes, refresh them with the
  same OData shape used by `examples/fiori/mint-test-data.sh`, keeping
  credentials in `flowproof config` or the operator's environment, not in the
  example package.
- Fiori should use the already-recorded launchpad tile
  `Display Purchasing Info Record by Supplier`, which opens the WebGUI screen
  titled `Info Records per Supplier`. Existing live artifacts show stable iframe
  field selectors for the search form if label targeting is not enough:
  Supplier `css:#M0\:46\:\:\:0\:34`, Material `css:#M0\:46\:\:\:1\:34`,
  and Plant `css:#M0\:46\:\:\:8\:34` inside iframe `Application`.
- The Excel handoff should export from the Fiori/WebGUI report result, then
  assert the downloaded spreadsheet by checking that the `Net Price` column is
  present and the exported rows contain `${captured.gui_material}` / `${MATERIAL}`.
  The exact toolbar control for export is a recording-time selector decision
  because the WebGUI ALV toolbar may expose it as a button, menu item, or
  spreadsheet action depending on theme and layout.
- Release-note copy belongs in `examples/release-notes/README.md`, with the
  normal `CHANGELOG.md` entry if this plan is implemented. No separate external
  release-notes file is required for this plan.

## Setup story: no `.env`

The README should show the release-demo setup as explicit commands:

```console
$ flowproof config sap
$ flowproof config fiori
$ flowproof config show
$ flowproof doctor --sap
$ flowproof doctor --fiori
```

Then copy the example values file:

```text
SUPPLIER: "10300001"
MATERIAL: TG10
PLANT: "1010"
NET_PRICE: "12.35"
REPORT_TILE: Display Purchasing Info Record by Supplier
```

The README should state the contract plainly:

- credentials and connection defaults live in `flowproof config`;
- business data lives in `sap-gui-fiori-excel.values.yaml`;
- the example must run from a clean shell with no repo-local `.env`;
- an explicit shell export or `--var` override still wins when supplied.

This is not just documentation polish. The issue asks to demonstrate config
replacing `.env` setup, so the verification must include a clean-env run where
`SAP_*` and `FIORI_*` are not exported by the shell and are supplied only by the
global flowproof config file.

## Why Fiori should own the export

The issue allows the Excel handoff to come from SAP Fiori or SAP GUI. Use Fiori
for the first version because Flowproof already has a browser download primitive
and an existing Fiori-to-Excel example. SAP GUI export-to-file through native
dialogs is a separate Windows/SAP-driver problem and should not be smuggled into
this release walkthrough unless live recording proves Fiori export is unusable.

The demo can still start in SAP GUI with meaningful work: use the GUI block to
open/display the same record, capture an accepted value from SAP GUI, and carry
that capture into Fiori. That is enough to prove one continuous test case across
SAP GUI, browser Fiori, and Excel.

## Implementation steps

1. Create `examples/release-notes/sap-gui-fiori-excel.flow.yaml` as the drafted
   multi-surface flow: SAP GUI `/nME1L`, then Fiori
   `Display Purchasing Info Record by Supplier`, then Excel/spreadsheet
   assertion.
2. Add `sap-gui-fiori-excel.values.yaml.example` with non-secret business data
   keys only. Do not put credentials, base URLs, or passwords in this file.
3. Add `examples/release-notes/README.md` with the `flowproof config`,
   `doctor`, record, and replay commands.
4. Add a parser-focused test beside the existing example parsing checks so the
   multi-surface shape stays valid on Linux/macOS without requiring SAP GUI,
   Fiori, or Excel.
5. On a Windows machine with SAP GUI, Fiori, and Excel installed, record the
   live trace. Add the new trace as a new cassette only after confirming
   `secret_scan` passes and no committed existing trace was modified.
6. Replay from a clean shell where `SAP_*` and `FIORI_*` are not exported by the
   shell. `env -u` is only a negative-control guard here: it proves the child
   process is not inheriting shell credentials. The actual credential source
   should be `flowproof config`; business data should come from the sibling
   values file.
7. Update `CHANGELOG.md` in its existing voice, explaining why the demo matters:
   a single recorded proof can cross SAP GUI, Fiori, and Excel without a local
   `.env` file.

## Verification

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `flowproof config show` masks passwords.
- `flowproof doctor --sap` attaches to SAP GUI scripting and names the selected
  `SAP_CONNECTION`.
- `flowproof doctor --fiori` reaches the launchpad and, when configured, passes
  the login check.
- `env -u SAP_USER -u SAP_PASSWORD -u SAP_CLIENT -u SAP_LANGUAGE -u SAP_CONNECTION -u FIORI_USER -u FIORI_PASSWORD -u FIORI_CLIENT -u FIORI_LANGUAGE -u FIORI_BASE_URL flowproof record examples/release-notes/sap-gui-fiori-excel.flow.yaml --headed`
- The recorded flow spends real time in the product screens; do not add sleeps
  merely to inflate duration.
- `env -u SAP_USER -u SAP_PASSWORD -u SAP_CLIENT -u SAP_LANGUAGE -u SAP_CONNECTION -u FIORI_USER -u FIORI_PASSWORD -u FIORI_CLIENT -u FIORI_LANGUAGE -u FIORI_BASE_URL flowproof run examples/release-notes/sap-gui-fiori-excel.flow.yaml --headed`
  replays without model calls and reaches the Excel/spreadsheet assertion.
- The new trace contains placeholders/captures, not credential values.

## Out of scope

- Implementing surface-specific `login:` staging for multi-surface flows.
- Building a dedicated Excel adapter.
- Building SAP GUI export/download automation if Fiori export works.
- Adding live SAP/Fiori/Excel tests to default CI.
- Moving business data into `flowproof config`.
- Modifying any existing committed `*.trace.jsonl`.

## Open questions

None for the planning PR. The remaining unknowns are recording details that
must be resolved on the live Windows SAP GUI/Fiori/Excel machine: exact SAP GUI
field ids and the final WebGUI export toolbar selector.
