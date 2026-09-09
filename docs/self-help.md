---
title: "The outside-in loop"
description: "What happens when authoring gets stuck: recording hands the ambiguity to the driving agent to resolve and re-record."
---

A spec step like **"make required field changes"** cannot be authored: it
names no fields, and no amount of grounding fixes that. flowproof's answer
is deliberately *not* an in-loop conversation: recording stops and hands
the **driving agent** (you, or any MCP-capable agent: DataMaker's agent,
Claude, a CI bot) everything it needs to resolve the ambiguity itself,
rewrite the step into concrete grammar, and record again. flowproof stays
deterministic; the intelligence stays outside.

The loop has two legs: a **clarification payload** (what was ambiguous,
what the live screen offered) and **values files** (how non-secret
business inputs reach the spec without being baked into flow steps).
Together they let an agent author tests against systems it cannot see
into, like SAP, by asking tools that can.

This loop is for UI authoring. An `app: agent` flow (see
[agent-testing.md](agent-testing/index.md)) never enters it: its steps
(`prompt:`, `assert_tool_call:`, `assert_no_tool_call:`) are structured and
either parse or raise a plain error, so there is nothing to clarify.

## The clarification payload

When neither the deterministic rules nor the grounded model (one attempt +
one self-correcting retry) can author a step, recording fails with data:

```json
{
  "needs_clarification": {
    "step": "make required field changes to the info-record",
    "step_index": 14,
    "stage": "model",
    "reason": "target 'css:#made-up' is not one of the listed elements",
    "rules_error": "no rule matches this step for app 'web'",
    "completed_steps": ["Type ${MATERIAL} into the \"Material\" field", "…"],
    "scene": [
      {"target": "css:#netPrice", "tag": "input", "type": "text", "label": "Net Price"},
      {"target": "css:#minQty", "tag": "input", "type": "text", "label": "Minimum Order Quantity"},
      {"target": "text:Save", "tag": "button", "text": "Save"}
    ],
    "hint": "Rewrite the step using the grammar in docs/authoring.md, …"
  }
}
```

- `scene` is the live screen's interactable inventory **at the moment of
  failure**: the app is in the state after `completed_steps`, so these
  are the real fields the vague step was about. Each `target` token is
  usable verbatim in a rewritten step.
- `stage` is `no_model` (rules failed, no model backend configured) or
  `model` (the model was consulted and could not ground the step).
- Every text field keeps `${VAR}` references raw: the payload never
  holds a resolved secret.
- `--author rules` keeps failing with a plain rules error (no payload):
  rules-only mode is a request for determinism, not a conversation.
- `heal` currently surfaces plain errors only: payload support there is
  a known follow-up.

### Where it surfaces

| Surface | Shape |
| --- | --- |
| `flowproof record --json` | the JSON above on stdout, exit code 2 |
| MCP `flowproof_record` | the same dict as a normal (non-error) tool result |
| Python `Flow.record()` / `flowproof.record()` | raises `ClarificationNeeded`, payload on `.clarification` |

## The worked example: "make required field changes"

The Fiori test case in [examples/fiori/](../examples/fiori/) came from a
manual script whose step 4 read *"Select the info-record and make required
field changes"*. The loop that turned it into grammar:

1. **Record.** Steps 1–13 (login, navigate, search, open, Edit) author
   fine; step 14 fails with the payload above.
2. **Read the scene.** The payload lists what the edit screen actually
   offers: `Net Price`, `Minimum Order Quantity`, a `Save` button.
3. **Ask the system of record what "required" means.** The scene knows
   what is *on screen*; it cannot know which fields the test *should*
   change or to what values. That is a domain question, so the driving
   agent asks the tool that can see into SAP (here, the DataMaker CLI):
   "which fields on info record `${MATERIAL}`/`${SUPPLIER}`/`${PLANT}`
   are editable, and give me a valid new net price."
4. **Rewrite.** The vague step becomes concrete grammar:
   ```yaml
   - Replace the "Net Price" field with ${NET_PRICE}
   - Replace the "Minimum Order Quantity" field with 10
   ```
5. **Re-record.** Every step now authors deterministically; the recorded
   trace replays forever with zero model calls.

One rule worth internalizing while rewriting: **quoted targets never
resolve `${VAR}`** (selectors travel raw in traces). You cannot
`Click "${MATERIAL}"`; address the row structurally (`Click the 1st
"css:.sapMListItems .sapMLIB"`) and assert on the data instead
(`assert: page shows ${MATERIAL}`).

## The data leg: values files

The rewritten steps reference `${MATERIAL}`, `${NET_PRICE}`, …, values
that must exist in the connected SAP system. Those are business inputs,
not credentials, so the Fiori examples keep them visible in a checked-in
file:

```yaml
# examples/fiori/values.yaml
MATERIAL: "TG10"
SUPPLIER: "10300001"
PLANT: "1010"
NET_PRICE: "12.35"
```

Run the shared Fiori values explicitly:

```console
$ flowproof run examples/fiori/display-info-record-by-supplier.flow.yaml --vars examples/fiori/values.yaml
$ flowproof run examples/fiori/purchasing-info-record-api.flow.yaml --vars examples/fiori/values.yaml
$ flowproof run examples/fiori/ --vars examples/fiori/values.yaml --missing skip
```

A single standalone flow can also use the sibling convention, where the
values file sits beside the flow and is discovered automatically:

```text
display-info-record-by-supplier.flow.yaml
display-info-record-by-supplier.values.yaml
```

Credentials still stay outside the example values file: use
`flowproof config`, CI secrets, or the caller's environment for login
names, passwords, and API auth tokens. Values reach traces only as raw
`${VAR}` references, never resolved.

Related: because traces store only the raw `${VAR}` refs, `app: api`
flow traces can be minted **offline** against a local contract responder
and replayed against the real stack, see
[getting-started](getting-started/api-flows.md#minting-traces-offline-against-a-contract-responder).

## Why no in-loop tool use

The authoring model could, in principle, call the DataMaker CLI itself
mid-recording. We chose not to: recording stays a bounded, predictable
computer-use loop (ground against the scene, act, record), and every
judgment that needs external truth happens *outside*, where it is
reviewable and where the driving agent already has richer context. This
mirrors the selector-ladder philosophy: deterministic first, model only
where genuinely ambiguous, and *conversational* never, inside the
engine.
