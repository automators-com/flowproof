---
title: "SAP GUI flows"
description: "Running flows against SAP GUI on Windows, including the login: block for the flow's own user."
---

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

Typing those into every new shell gets old fast — `flowproof config sap`
writes them to a global, per-machine config file once, and every `record`/
`run` picks them up automatically (see
[below](#flowproof-config-credentials-without-hand-exporting-env-vars)).
That includes a single `.flow.yaml` run directly with no neighboring
`suite.yaml`.

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
