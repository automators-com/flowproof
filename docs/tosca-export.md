---
title: "Exporting to Tosca"
description: "flowproof export turns a recorded SAP GUI or web trace into the test-case JSON the Automators Tosca migrator imports."
---

Record a flow once in flowproof, then hand it to Tosca:

```bash
flowproof export examples/sap/create-order.flow.yaml --format tosca-json --out create-order.json
```

The path is a trace or a flow spec (its trace is used). Without `--out` the
JSON goes to stdout. `--format` defaults to `tosca-json`. The file is one
test case in the migrator's `file_format: JSON` shape.

| Recorded | Exported |
|---|---|
| `Go to /nVA01` (SAP) | keyword `StartTransaction` `VA01` |
| flow `url:` and `Go to` (web) | keyword `NavigateBrowser` |
| type, click, check | `Input` (`Click`, `true`/`false`) |
| `Press Enter`, Escape, Tab, F1 to F12 | keyword `SendKey` |
| field and checkbox assertions | `Verify`, presence as `Exist` |
| remember a value | `Buffer` |
| `${captured.x}` / `${VAR}` | `{B[x]}` / `{CP[VAR]}` |

SAP elements use steering strategy `SAPGUI_CBTA` with `RelativeId` and
`Name` from the scripting id; modules are named after the transaction. Web
elements use `Html_NWBC` with `html id`; modules are named after the site and
match any page title (`*`), since the title is not recorded yet. One authored
step stays one Tosca step.

Steps Tosca cannot express are printed on stderr as `warning: <step>: not
exported, <reason>`: page-wide checks (`page shows ...`), web targets without
a plain DOM id, visual, SQL and API assertions, key chords, and hover, drag,
scroll or upload. `repeat:` and `when:` blocks export as the one recorded
path. Windows desktop, vision, API and agent traces are refused.
