---
status: draft
---
# Plan 7 — Autonomous flow repair loop

## Problem

`flowproof record` grounds a `.flow.yaml` against the live app and writes a
deterministic trace. When a step fails today, the operator sees the failure,
has to read the error themselves, guess which line in the `.flow.yaml` is
wrong, edit it by hand, and run `record` again from the top. For SAP GUI and
Fiori flows this loop is frequent — selector drift, timing, occluded targets,
and split buttons are common — and it currently costs a human iteration cycle
every time.

The goal of this plan is narrower and more mechanical than "assisted
authoring" in general: given one `record` invocation and one failing step,
close the loop automatically. Take the failure, use a model to work out what
about the `.flow.yaml` is wrong, edit the `.flow.yaml`, and rerun — without a
human re-typing `flowproof record`, and without asking the human to approve
each patch. The human's job is to hit `record` once and either get back a
passing flow or a clear explanation of why it couldn't get there.

## Non-goal: this does not touch source code

Flowproof's own source (the Rust workspace, the driver, the adapters) is never
a valid target for this loop. If a flow is failing because Flowproof itself
lacks a capability — an unsupported control, a missing primitive — no amount
of `.flow.yaml` editing fixes that, and the loop must say so and stop rather
than paper over it by mangling the flow until something appears to pass. The
only artifact this loop is allowed to write is the user's `.flow.yaml` (and
its trace/report sidecars). See "Engine-gap stop condition" below.

## Decision

`flowproof record` grows a self-repair loop that runs by default (opt out with
a flag, not opt in):

```text
record -> fail? -> extract error + context -> LLM proposes a .flow.yaml
edit -> apply edit -> rerun from the failed step -> pass? -> done
                                                 -> fail again? -> repeat,
                                                    bounded
```

No approval prompt sits inside this loop. The operator asked for a passing
flow when they ran `record`; the loop's job is to deliver that or explain why
it can't, in one sitting. This is the key difference from a "propose a diff
and wait for a human to apply it" design: the whole point is that the human
does not re-record and does not review each intermediate patch.

What the human does see, once the loop stops (pass, budget exhausted, or
engine-gap):

- the final `.flow.yaml`, already edited in place;
- a diff of everything the loop changed, step by step, so the edits are
  auditable after the fact even though they weren't gated before the fact;
- the trace from the final passing (or last failing) attempt;
- a plain-language summary of what was wrong and what changed.

Editing "after the fact, but visibly" rather than "before the fact, with
approval" is the trade this plan makes deliberately: it optimizes for the
stated workflow (record once, walk away, come back to a passing flow) over
per-edit review. Because the edit is always visible afterward and the flow is
version-controlled, an unwanted change is still recoverable — it's a `git
diff`/`git checkout` away, not silent.

## Loop mechanics

1. `record` runs the flow. On success, the loop does not engage at all — the
   existing `record` behavior is unchanged for a flow that just works.
2. On failure, capture:
   - the failing step (line/index in the `.flow.yaml`);
   - the driver/replay error verbatim;
   - the completed steps before it;
   - the live target inventory at the point of failure (labels, roles,
     selectors) — the same structured, redacted context `record --json`
     already produces for clarification payloads.
3. Send that structured context (not a screenshot, not raw business data — see
   "Model inputs" below) to the configured authoring model and ask for a
   minimal edit to the `.flow.yaml`: which line changes, to what, and why.
4. Apply the proposed edit to the `.flow.yaml` directly.
5. Rerun. Prefer resuming from the failed step rather than the top of the flow
   when the engine can safely do so; fall back to a full rerun when it can't
   (e.g., state from earlier steps can't be assumed still valid).
6. If it passes, stop and report success plus the accumulated diff.
7. If it fails again:
   - same diagnosis category as last time, no progress → stop, this is
     probably an engine gap, not a flow bug (see below);
   - different/progressing failure → loop again, up to a bounded number of
     attempts (default 3, configurable).
8. If the attempt budget is exhausted without a pass, stop and report the
   last failure, the full attempted-edit history, and why the loop gave up.

## Engine-gap stop condition

The loop needs a way to recognize "this isn't a flow-authoring problem" and
stop patching instead of thrashing:

- the same diagnosis category repeats across attempts without the failure
  point advancing;
- the model itself reports that no `.flow.yaml` edit can address the failure
  (e.g., the control class isn't supported at all);
- the proposed edit would remove or weaken a business-outcome assertion
  rather than fix the grounding — the loop must never "fix" a test by making
  it assert less.

When this triggers, the loop stops, leaves the `.flow.yaml` as it was before
that attempt (or at the last edit that made real progress), and reports an
engine-gap recommendation instead of a patch. That recommendation is a
human/engineering follow-up (an issue, in flowproof's own development), never
an automatic source-code change — this loop has no code-editing capability at
all, by construction, not just by policy.

## Model inputs

Same posture as existing model-assisted authoring in this codebase:

- structured, redacted failure context: step text with `${VAR}` references
  preserved, target inventory (labels/roles/selectors), failure category,
  driver error;
- no screenshots and no raw business/customer/table data by default;
- no resolved secrets ever reach the prompt.

The loop must never call the model during deterministic replay — only during
a live `record` run.

## What's different from proposing-and-approving

An earlier draft of this plan (see git history / the discarded PR) modeled
this as: diagnose, propose a diff, require an operator to explicitly approve
and apply it, one proposal at a time. That is a reasonable design for a
support tool used interactively, but it is not what was asked for here. This
plan's premise is that `record` should be able to run unattended: kick it off,
let it iterate against the live app on its own, and come back to either a
production-ready flow or a clear stop reason. Approval-gating every edit is
incompatible with "the user should not have to do anything" — so this plan
removes the per-patch approval step and replaces it with after-the-fact
visibility (diff + report) plus a hard attempt budget as the safety valve.

## Promotion / production-readiness

A flow the loop touches is not implicitly "done" just because it passed once
mid-loop — the existing bar still applies:

- the spec parses and has no unresolved ambiguous steps;
- a live trace exists from a passing run;
- replay/verification passes where safe;
- every critical business outcome still has an assertion (the loop is
  forbidden from removing these, see above);
- no resolved secret appears in the trace, report, or applied edit.

## Implementation steps

1. Add structured failure-context capture to the `record` failure path
   (reuse the same shape `record --json` already emits for clarification).
2. Add a diagnosis/patch-proposal call using the configured authoring model,
   scoped to a single-file `.flow.yaml` edit with a required rationale string.
3. Apply proposed edits directly to the `.flow.yaml` on disk; keep an
   in-memory/temp history of each attempt's before/after for the final diff
   report.
4. Add resume-from-failed-step rerun where safe; fall back to full rerun.
5. Add the bounded-attempt loop with the same-category-no-progress stop rule.
6. Add the engine-gap detection path and its report shape.
7. Add a `--no-repair` (or similarly named) flag to disable the loop and get
   today's single-attempt `record` behavior back.
8. Add a final report (CLI text + JSON) summarizing: attempts made, diffs
   applied, final state, and — on engine-gap — the recommendation text.
9. Wire the same behavior through MCP/API so external callers get the same
   unattended loop.

## Tests

- A fix ships with the test that proves it stays fixed, per this repo's
  convention — every diagnosis category this loop handles needs a fixture
  proving record → fail → patch → pass.
- Bounded-attempt tests proving the loop stops at the configured budget.
- No-progress tests proving repeated identical failures stop the loop instead
  of looping forever.
- Tests proving the loop never removes or weakens an existing outcome
  assertion.
- Tests proving `--no-repair` reproduces prior single-attempt behavior
  exactly.
- Redaction tests proving resolved secrets, screenshots, and raw business
  data never enter the model prompt by default.
- Replay tests proving the loop never engages outside a live `record` run.

## Out of scope

- Any edit to Flowproof's own source code, ever, by this loop.
- Per-patch human approval as a required step (that is a different, simpler
  feature someone could build on top of this later; it is not this plan).
- Replay-time model calls.
- Sending screenshots or full business/customer/table data to the model by
  default.
- Guaranteeing every SAP/Fiori control class can be fixed by a flow edit —
  the engine-gap path exists precisely because some can't be.

## Resolved decisions

- The loop runs as part of `record`, not a separate subcommand, and runs by
  default; `--no-repair` opts out.
- No per-patch approval gate. Visibility is after-the-fact (diff + report),
  not before-the-fact (approval prompt).
- Default attempt budget: 3, matching the bound already used elsewhere in
  this codebase's design discussion for assisted reruns.
- The only file this loop may write is the target `.flow.yaml` plus its
  trace/report sidecars — never source code.

## Open questions

- Exact resume-from-failed-step semantics: which step types are safe to
  resume from versus require a full rerun from the top?
- Where should the "same category, no progress" comparison live — is it
  purely diagnosis-category equality, or does it need to compare the failing
  selector/step too, to avoid stopping too early on a category that covers
  multiple distinct root causes?
- Should the attempt budget be per-flow-run or configurable per flow in the
  `.flow.yaml` itself (some flows may warrant more attempts than others)?
- How does this relate to `heal` (which re-records an *existing* trace that
  has drifted)? Are they the same engine invoked from two entry points, or
  does `heal` stay a separate, narrower operation?
