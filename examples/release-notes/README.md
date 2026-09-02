# SAP GUI to Fiori to Excel release walkthrough

A several-minute proof that one recorded flow can cross SAP GUI, SAP Fiori,
and Excel, and that setup for it needs no repo-local `.env` file — answering
[issue #536](https://github.com/automators-com/flowproof/issues/536). See
[`plans/006-sap-gui-fiori-excel-config-demo.md`](../../plans/006-sap-gui-fiori-excel-config-demo.md)
for the design.

The flow, `sap-gui-fiori-excel.flow.yaml`, is a single multi-surface test
case:

1. **SAP GUI** — `/nME1L`, the read-only "Purchasing Info Records by
   Supplier" report. Looks up a purchasing info record by
   Supplier/Material/Plant and remembers the accepted material.
2. **SAP Fiori** — the launchpad tile "Display Purchasing Info Record by
   Supplier", searched with the same business data, then exported.
3. **Excel** — the exported spreadsheet, opened and asserted against
   directly (`assert_spreadsheet`, not Excel's UI grid).

## The contract

- **Credentials and connection defaults live in `flowproof config`.** Never
  in this repo, never in a `.env` file.
- **Business data lives in `sap-gui-fiori-excel.values.yaml`**, copied from
  the committed `.example` template beside it.
- **The example must run from a clean shell**, with no `SAP_*` or `FIORI_*`
  exported — everything comes from `flowproof config` and the values file.
- An explicit shell export or `--var` override still wins when supplied —
  `flowproof config` only fills gaps.

## Setup

```console
$ flowproof config sap
$ flowproof config fiori
$ flowproof config show
$ flowproof doctor --sap
$ flowproof doctor --fiori
```

`flowproof config sap`/`fiori` prompt for the credentials and connection
defaults for this machine once; `flowproof config show` prints them back
with the password masked. `flowproof doctor --sap`/`--fiori` confirm the
seeded config actually reaches something live — see
[`docs/getting-started.md`](../../docs/getting-started.md#flowproof-doctor---sap----fiori-is-any-of-this-reachable)
for what each check does.

Then copy the business data template:

```console
$ cp sap-gui-fiori-excel.values.yaml.example sap-gui-fiori-excel.values.yaml
```

## Record

Against the live target system, with SAP GUI already open and logged in,
and Excel installed:

```console
$ env -u SAP_USER -u SAP_PASSWORD -u SAP_CLIENT -u SAP_LANGUAGE -u SAP_CONNECTION \
      -u FIORI_USER -u FIORI_PASSWORD -u FIORI_CLIENT -u FIORI_LANGUAGE -u FIORI_BASE_URL \
      flowproof record examples/release-notes/sap-gui-fiori-excel.flow.yaml --headed
```

The `env -u` prefix is a negative-control guard, not the credential source:
it proves the child process is not inheriting shell credentials, so a
passing recording is proof the setup story in this README actually works.

## Replay

No model calls, no live SAP/Fiori/Excel connection needed once a trace is
committed:

```console
$ env -u SAP_USER -u SAP_PASSWORD -u SAP_CLIENT -u SAP_LANGUAGE -u SAP_CONNECTION \
      -u FIORI_USER -u FIORI_PASSWORD -u FIORI_CLIENT -u FIORI_LANGUAGE -u FIORI_BASE_URL \
      flowproof run examples/release-notes/sap-gui-fiori-excel.flow.yaml --headed
```

## What is out of scope here

- Surface-specific `login:` staging for multi-surface flows (not shipped
  yet — SAP GUI and Fiori credentials come from `flowproof config` /
  environment seeding instead).
- A dedicated Excel adapter — Excel is a generic Windows-mapping surface
  (`command: EXCEL.EXE ...`), and the assertion reads the spreadsheet file
  directly rather than through Excel's UI grid.
- SAP GUI export/download automation — the Excel handoff comes from the
  Fiori export, which already has a browser download primitive.
