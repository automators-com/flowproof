---
name: flowproof-flows
description: >
  Record, replay, repair and audit end-to-end flows through flowproof's MCP
  tools (flowproof_record, flowproof_run, flowproof_heal, flowproof_doctor,
  flowproof_audit). Use when the user wants a `.flow.yaml` recorded or
  replayed, asks why a flow failed, wants a flow fixed after the app changed,
  or wants control-coverage evidence for a suite. Covers which tool to call,
  how to read a failing run, and the two points where the user must decide:
  applying a repair, and letting a flow run its own commands.
---

# Working with flowproof flows

A flow is a `.flow.yaml` of plain steps. Recording performs it once against
the live app and writes a `.trace.jsonl` beside it; replaying runs the trace
with no model calls. These tools run `flowproof` for you. They need the
`flowproof` MCP server; if the tools are missing, tell the user to run
`flowproof mcp install` in the project and restart their agent.

## Which tool

| The user wants | Call |
| --- | --- |
| A new flow, or a flow with no trace yet | `flowproof_record` |
| To know whether a flow (or every flow under a folder) still passes | `flowproof_run` |
| A flow fixed because the app changed and replay now fails | `flowproof_heal` |
| To know whether a model or SAP GUI is set up | `flowproof_doctor` with `check: ai` or `sap` |
| Control-coverage evidence for a suite folder | `flowproof_audit` |

Pass `cwd` when the flow is in a different project from the one the agent
started in; every result echoes the `cwd` it used. Leave `headed` off: runs
are headless unless the user asks to watch.

## Read results; don't retry blindly

- **A failing run is a result, not an error.** `passed: false` comes with a
  `report`: find the step in `report.steps` (for a folder, in each
  `report.flows[].report.steps`) with a failed `status` and read its
  `detail`, `expected` and `found`. Then decide with the user which it is: the app has
  a bug (report it, change nothing), the app changed on purpose (heal), or
  the flow's step is wrong (edit the `.flow.yaml` and record again).
- **`isError: true` means the engine could not do the job**, for example a
  spec that does not parse or a missing trace. Fix the cause the `error`
  names; calling again unchanged gives the same answer.
- A step whose `degraded` is true passed on a weaker selector. Mention it;
  it is an early sign the app is drifting.

## When recording gets stuck

A `flowproof_record` result with `recorded: false` and a
`needs_clarification` object under `result` means one step was too vague to
perform. Its `scene` lists the real fields on screen at that moment, each
with a `target` you can use verbatim. Rewrite that one step into concrete
grammar (for example `Type ${PRICE} into the "Net Price" field`), keep the
steps in `completed_steps` as they are, and record again. If the right value
is unclear, ask the user rather than guess.

## Two decisions that belong to the user

**Applying a repair.** `flowproof_heal` writes a proposal beside the trace
(`.proposed.jsonl`, with a before/after `diff_html`) and leaves the committed
trace alone. Show the user what changed (`steps_changed`, and the diff page if
they want to look). Only when they say to apply it, call `flowproof_heal`
again with `apply_proposal: true` and `reviewed: true`. Never pass `reviewed`
on your own judgement, and never edit a `.trace.jsonl` by hand.

**Letting a flow run its own commands.** A flow whose `suite.yaml` hooks,
`agent.command` or launch command would run a program is refused, and the
result names the command, the file it came from and a `consent` note. Show
the user that command. Call again with `allow_exec: true` only if they agree
to run it.

## What to hand back to the user

- **Credentials and the authoring model.** No tool sets them. If a result
  says no authoring model is configured, or a flow fails on `${SAP_USER}`,
  `${FIORI_PASSWORD}` or a model key, stop and use the `flowproof-config`
  skill to walk the user through `flowproof config`. Never put a secret in a
  tool argument, a `var`, a flow file or a trace.
- **`doctor --fiori` and `doctor --agent`.** `flowproof_doctor` refuses them:
  one logs in to a live system, the other runs a command. The user runs those
  in a terminal.

## Data that leaves the machine

Tool results carry screen content: a clarification's `scene` and a
failure's screen details reach whichever model this agent uses. Recording
with an authoring model also sends screens to the provider set in `flowproof
config ai`. Record against test accounts and test data, and say so if the
user points a flow at production.
