---
status: draft
---
# Plan 7 — Assisted production authoring loop

Flowproof already has the raw pieces of an assisted authoring workflow:

- `author-from-doc` drafts a `.flow.yaml` from an exported test case document.
- `record` grounds steps against the live app and writes a deterministic trace.
- `record --json` can return structured clarification payloads with the live
  screen inventory when authoring gets stuck.
- `heal` can re-record and propose reviewable diffs when an existing trace
  drifts.
- Replay stays deterministic and does not require an API key.

What is missing is the production wrapper around those pieces. Today, when a
SAP/Fiori flow fails in a non-obvious way, a skilled operator still has to do
what we have been doing manually: inspect the failure, identify whether the
issue is data, selector grounding, page readiness, occlusion, split-button
behavior, iframe behavior, or an unsupported control, patch the flow or engine
with the smallest defensible change, rerun, and only then promote the result.

This plan turns that manual support loop into a first-class product workflow.

## Problem

For ordinary HTML pages, a document draft plus one live `record` pass can be
enough. SAP GUI, SAP WebGUI, and SAP Fiori/UI5 are harder:

- visible text can contain layout-only characters such as soft hyphens;
- field labels can point at generated UI5 controls rather than native inputs;
- page-level text can omit iframe/application body text at the wrong moment;
- shell navigation tabs can sit behind overflow menus;
- toolbar controls can be icon-only;
- split buttons can have overlapping text and arrow hit targets;
- SAP field changes may visually accept a value and later restore the old one;
- a test can pass weakly by clicking or pressing a key without proving the
  business outcome happened.

The current product surface exposes the lower-level failure, but it does not
yet consistently provide the next step a client user needs. In production, that
creates the wrong experience: the user sees Flowproof fail and needs a Flowproof
engineer to interpret the failure.

## Decision

Add an assisted authoring loop that behaves like a cautious support engineer
inside the product:

```text
document/test case -> draft -> live record -> diagnose -> propose patch ->
rerun -> verify -> production-ready trace
```

The loop can use the configured authoring model API key, but only for
authoring/diagnosis/patched-spec proposals. It must never make replay
non-deterministic, and it must never silently mutate a production trace.

The core contract:

```text
A Flowproof test is production-ready only after it has a successful live
recording trace and has passed replay/verification gates.
```

`author-from-doc` output remains a draft. A model-authored patch remains a
proposal. A failed or unrecorded flow is not production-ready.

## Proposed capability

Introduce assisted recording as an extension of the existing record command,
plus equivalent MCP/API workflow:

```console
$ flowproof author-from-doc <document.pdf> --app web --out flow.flow.yaml
$ flowproof record flow.flow.yaml --assist --headed
$ flowproof record flow.flow.yaml --assist --apply <proposal-id>
$ flowproof record flow.flow.yaml --assist --promote
```

`--assist` is deliberately attached to `record`: it is not a second execution
engine, it is the support layer around live recording. The product states should
not change even if the final sub-flags are adjusted during implementation:

| State              | Meaning                                                       |
| ------------------ | ------------------------------------------------------------- |
| `draft`            | A spec exists, but has not been proven live.                  |
| `blocked`          | Recording stopped with a diagnosis and suggested next action. |
| `grounded`         | Live recording produced a trace.                              |
| `verified`         | The trace replayed successfully, or `record --verify` passed. |
| `production-ready` | A human accepted the verified flow/trace for the suite.       |

The command should maintain a small sidecar readiness report next to the flow,
for example:

```text
flow.flow.yaml
flow.trace.jsonl
flow.assist.json
flow.assist.html
```

The sidecar is the production-readiness file: it records the current state,
what evidence supports that state, and why the flow is or is not ready for the
client suite. A per-flow sidecar is required. A suite-level report can aggregate
many sidecars later, but the flow should not depend on a suite to know whether it
is ready.

The HTML report should show:

- current state;
- last failed step;
- completed steps;
- screenshot/frame evidence where available;
- diagnosis category;
- proposed YAML diff;
- whether an engine issue was detected instead of a flow issue;
- what was rerun and whether it passed.

## Diagnosis categories

The assistant should classify failures into a small, actionable taxonomy:

- `ambiguous-step`: the document/manual step does not name a concrete action.
- `missing-target`: the requested field/button/text is not present.
- `selector-drift`: an old trace selector no longer resolves.
- `page-not-ready`: the next target exists later than the current wait allows.
- `wrong-wait-signal`: the flow waits on a transient or proxy signal.
- `occluded-target`: another element would receive the click.
- `split-button`: the named control has separate text/arrow hit targets.
- `icon-only-control`: the control has no user-visible text label.
- `iframe-boundary`: page-level text or selectors are looking outside the real
  application frame.
- `field-commit-rejected`: a typed SAP/Fiori value was restored or rejected.
- `data-mismatch`: the values file/business data does not match live tenant
  data.
- `engine-gap`: the flow is reasonable, but Flowproof lacks a primitive or
  adapter behavior.

Every category should carry a recommended next action. Examples:

- `wrong-wait-signal`: replace `Wait until page shows Home` with the concrete
  next target or result title.
- `split-button`: propose an offset click or a more precise selector.
- `iframe-boundary`: propose an iframe-scoped target or assertion.
- `field-commit-rejected`: fail at the field and suggest verified replace/commit
  behavior.
- `engine-gap`: stop flow patching and open an implementation task.

## Model/API-key usage

The API key should help with the same work a human support engineer performs:

- summarize the failing run;
- read a structured clarification payload;
- choose between live inventory candidates;
- rewrite a vague document step into Flowproof grammar;
- propose a minimal YAML diff;
- explain why a selector is safer than a broad text click;
- identify that a failure is likely an engine gap instead of a bad test.

The model should not:

- run during deterministic replay;
- silently apply patches;
- receive resolved secrets;
- receive screenshots or purchasing/customer data by default without an
  explicit operator policy;
- guess around a missing business requirement.

For SAP/Fiori production environments, default to structured, redacted inputs.
This means the model sees a safe summary of what Flowproof saw, not the full
business screen by default:

- step text with `${VAR}` references preserved;
- target inventory with labels/roles/CSS selectors;
- failure category and driver error;
- optional screenshot attachment only when the operator enables it.

Screenshots and raw table/customer/purchasing data are more powerful for
diagnosis, but they may expose client data. The production default should
therefore be structured inventory only. A client can opt into screenshots with a
clear policy setting when their data-handling rules allow it.

## Flow patching rules

The assistant can propose flow changes when the evidence supports them. The
normal production path is local: propose a patch to the user's `.flow.yaml`,
show the diff, and apply it only after explicit approval. It should not open a
codebase PR by default.

- replace a flaky proxy wait with the concrete next target;
- add a readiness wait before a field or iframe action;
- replace `Press Enter` with a named submit button when the UI exposes one;
- replace broad `Click "Purchasing"` with a route through `More` when the
  shell puts navigation behind overflow;
- use a CSS suffix selector for stable UI5 control ids when the prefix is
  generated but the semantic suffix is stable;
- use an offset click for a split-button text half when the center is occluded.

The assistant must not paper over an unproven business outcome. It should add
or preserve outcome assertions such as:

- result screen title reached;
- expected table title present;
- download captured;
- spreadsheet assertion passed;
- SAP GUI field value accepted after commit.

If the failure points to a Flowproof engine gap rather than a bad local flow, the
assistant should stop local patching and create a separate engineering
recommendation. In an internal development workflow that recommendation may
become a GitHub issue or PR. In a client production workflow it should be a
blocked status with a clear explanation and workaround, not an automatic code
change.

## Engine-gap escalation

If the flow is semantically correct but Flowproof is missing runtime support,
the assistant should stop patching the test and create an implementation
recommendation.

Examples from recent SAP/Fiori work:

- soft hyphens should be normalized in visible web text matching and text-anchor
  lookup;
- SAP WebGUI framed inputs should commit and read back typed values;
- framed presence assertions should work for frame-only selectors instead of
  producing an empty selector;
- UI5 split buttons and icon-only toolbar controls should have better built-in
  diagnostics and candidate selectors.

This distinction is important. Production trust comes from fixing root causes,
not from making one YAML file pass by force.

## Promotion gates

A flow cannot be marked production-ready unless all of these hold:

- the spec parses;
- all TODO/ambiguous steps are resolved or explicitly accepted as manual;
- `record` completed against the live target;
- a trace exists and records the grounded selectors/actions;
- `record --verify` or an immediate replay passed where safe;
- every critical business outcome has an assertion;
- no resolved secret appears in the trace, report, or proposed patch;
- any degraded selector warning is either healed or explicitly accepted.

For destructive SAP workflows, `record --verify` may be unsafe because it repeats
the action. In that case the gate should allow a documented exception and use
non-mutating assertions or a sandbox tenant.

## UI/API shape

The assisted loop should be available through:

- CLI, for local power users;
- MCP/API, so Codex, Claude Code, CI bots, or a client-facing UI can drive it;
- HTML report, so non-engineers can review the proposed fix;
- JSON report, so automation can enforce promotion gates.

The MCP/API should expose operations roughly equivalent to:

- `assist_start_from_doc`;
- `assist_record`;
- `assist_diagnose_failure`;
- `assist_propose_patch`;
- `assist_apply_patch`;
- `assist_rerun`;
- `assist_status`;
- `assist_promote`.

These can wrap existing `record`, `heal`, and `author-from-doc` internals rather
than inventing a second execution engine.

## Implementation steps

1. Define the persisted `assist` report schema: state, attempts, diagnostics,
   artifacts, proposed patches, verification results, and promotion metadata.
2. Add a diagnosis layer that maps existing record/replay/driver errors into
   the taxonomy above.
3. Extend `record --json`/MCP output so non-clarification failures can also
   include useful structured context where safe: failed selector, occlusion,
   last screenshot path, completed steps, and surface name.
4. Add model-assisted patch proposal on top of the structured context, with
   strict redaction and an operator policy for screenshots/live data.
5. Add a reviewable YAML diff format and an explicit apply command.
6. Add rerun orchestration with bounded attempts. Default to at most three
   assist reruns for one failure sequence, and stop earlier if the same category
   repeats without progress.
7. Add promotion gates and make their result visible in CLI/JSON/HTML.
8. Wire the same flow through MCP so external coding agents can run the loop
   without manually parsing terminal output.
9. Add SAP/Fiori-focused local fixtures for common categories: soft hyphen,
   iframe wait, field commit, overflow navigation, icon-only export, split
   button occlusion, and download capture.
10. Document the client deployment workflow: draft, assisted record, review,
    verify, promote.

## Tests

- Unit tests for each diagnosis category.
- Golden JSON/HTML report tests for blocked, patched, verified, and promoted
  flows.
- Local browser E2E fixtures for Fiori-like controls:
  - soft-hyphen tile text;
  - shell overflow navigation;
  - split-button export control;
  - icon-only toolbar button;
  - iframe application body;
  - SAP-like field commit/readback.
- CLI tests proving `assist` does not apply patches without explicit approval.
- Replay tests proving promoted traces still run without an API key.
- Redaction tests proving resolved secrets do not enter model prompts, assist
  reports, traces, or patches.
- Multi-surface tests proving diagnosis names the failing surface and preserves
  captures across reruns.

## Docs

- Add a production authoring guide:
  - `author-from-doc` creates a draft;
  - assisted `record` grounds it;
  - diagnosis proposes patches;
  - verification proves it;
  - promotion accepts it.
- Update `docs/self-help.md` from an outside-in concept into the documented
  workflow a client team can follow.
- Update `docs/adopting.md` with the production readiness bar.
- Add an example report walkthrough using the SAP/Fiori export case.

## Out of scope

- Autonomous production trace mutation.
- Replay-time model calls.
- Sending screenshots or full purchasing/customer data to a model by default.
- Replacing human business decisions when a requirement document is ambiguous.
- Guaranteeing arbitrary SAP/Fiori controls work without live recording.
- Changing Flowproof's deterministic trace format unless the assist report
  needs references to existing trace artifacts.

## Resolved decisions

- CLI spelling: implement this as `flowproof record --assist`, because assisted
  authoring is an extension of live recording rather than a separate execution
  engine.
- Default model inputs: send structured, redacted failure context only. Do not
  send screenshots or full purchasing/customer/table data unless the operator or
  client policy explicitly enables that.
- Patch behavior: propose and apply local `.flow.yaml` changes only after
  explicit approval. Do not open codebase PRs automatically in the client
  production path.
- Rerun budget: default to three assisted reruns for one failure sequence, and
  stop earlier when the same diagnosis repeats without progress.
- Production readiness storage: write a required per-flow sidecar report, such
  as `flow.assist.json`, and optionally aggregate those reports at suite level
  later.

## Open questions

- What exact approval UX should `--assist` use before modifying a local
  `.flow.yaml`: interactive prompt, `--apply <proposal-id>`, or both?
- Should screenshots be controlled by a global client policy, a per-command flag,
  or both?
- When the assistant detects an engine gap in a client environment, where should
  that recommendation be routed: local HTML report only, support ticket, GitHub
  issue, or an integration hook?
- What suite-level readiness view does the client need for rollout: counts only,
  detailed per-flow evidence, or release sign-off metadata?
