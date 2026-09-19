---
status: draft
---
# Plan 14 — Pinned and reviewable: preparing for cheap, capable agents

No issue yet. Strategic; touches `CHARTER.md` only by proposal (see
"Constitution note" — that edit is a human act and is not part of this plan's
implementation).

## Problem

flowproof's engineering weight is in the part a stronger agent makes
redundant. Measured on the workspace at the time of writing:

| Crate | Lines of Rust | Survives a capable agent? |
|---|---|---|
| `flowproof-trace` | ~4,900 | yes — the format is the asset |
| `flowproof-replay` | ~6,800 | yes — replay is defined by *no* agent running |
| `flowproof-agent` | ~24,300 | no — authoring is a workaround for model weakness |
| `flowproof-adapters` | ~23,700 | partly — SAP GUI and Windows outlast web/CDP |
| `flowproof-driver` | ~7,100 | partly — computer-use APIs will click for us |
| `flowproof-cli` | ~28,900 | thin rendering, follows the rest |

About twelve percent of the code carries the durable claim. Three trends make
the rest depreciate on a short horizon:

1. **Authoring gets free.** A frontier agent turns "fill in the vehicle form"
   into grounded actions with less scaffolding than `author.rs` carries.
   The rules grammar, the clarification loop and the prompt engineering are
   scaffolding for a model that is going away.
2. **Decision-only models arrive.** TypeSafe's Jev (September 2026) returns
   typed choices with calibrated probabilities in one forward pass, at a
   fraction of a cent per million input tokens, ~100 ms. Two of flowproof's
   three selling points — cost per commit and sampling flakiness — shrink
   for agents built on that class of model. Replay becomes a *choice*, not a
   necessity.
3. **Platforms ship record/replay natively.** Cassette recording is not
   novel. VCR-style tools are fifteen years old; LangSmith and Braintrust
   trace agent runs today; a lab SDK is one release away from doing it.

What does not depreciate is the position: a recording is ground truth, only
a human changes it, healing is a diff someone approves, and replay
reproduces bit-for-bit with no key and no vendor in the loop. That is what
a bank's change-management control and a pharma validation package want.
The charter already calls it the strategic bet ("deterministic to execute
and cheap to review"). The repository does not yet look like it.

## Decision

Invert the ratio of *effort*, not of code. Five workstreams, in the order
below. Each is small enough to ship inside the ~400-line pull-request bound;
each ships with the test that proves it holds.

| # | Workstream | Why first / why here |
|---|---|---|
| A | Evidence layer: hash, sign, approve | smallest; uses what the header has; first question a regulated buyer asks |
| B | Trace format as a standard | resolves an open policy the format doc already flags; makes a second implementation possible |
| C | Freeze authoring, plan for computer-use drivers | stops adding weight to the depreciating part |
| D | Confidence assertions at the model boundary | what decision-model agents need; additive |
| E | Charter reposition (proposal only) | steers the loops; costs nothing; human edit |

### A. Evidence layer

**Today.** The header carries `spec.hash` (sha256 of the spec) and `agent`
(backend + model id, provenance only). `heal` writes a proposed trace beside
the original plus `diff_html`; there is no record of who accepted it.
`flowproof audit` reads run records and reports control verdicts. Nothing
in the workspace signs, attests, or names an approver — `grep -rn
approver crates/` is empty.

**Build, in this order:**

1. **Trace content hash.** A `flowproof trace hash <trace>` subcommand that
   canonicalises the trace (header + step lines, byte-exact as stored) and
   prints `sha256:…`. Pure library call in `flowproof-trace`, CLI is a
   rendering (invariant 7). No format change.
2. **Approval record for `heal`.** Accepting a proposed trace today is
   "copy the file over the original". Replace with `flowproof heal accept
   <proposed>` which writes an additive header field:

   ```json
   "approvals":[{"kind":"heal","of":"sha256:<original>","by":"<identity>",
                 "at":"2026-09-19T10:12:33Z","diff":"sha256:<diff_html>"}]
   ```

   `by` is whatever identity the environment supplies (`git config
   user.email`, `FLOWPROOF_APPROVER`, or a CI actor variable) — flowproof
   records it, never authenticates it; an organisation's IAM is the
   authority, the trace is the ledger. Additive optional field, so every
   existing trace serialises byte-identical. Updates `docs/trace-format.md`
   and `trace-v1.schema.json` in the same commit (invariant 5).
3. **Detached signature.** `flowproof trace sign <trace> --key <path>`
   writes `<trace>.sig` (ed25519 over the content hash), `flowproof trace
   verify` checks it. Detached, so the trace itself stays a plain JSONL
   file and the cassette ratchet ("never modify a committed
   `*.trace.jsonl`") is untouched. Key handling is the operator's; flowproof
   never stores one.
4. **Evidence export.** `flowproof audit --export evidence/` renders, from
   the existing run record plus the traces it names: protocol (the spec),
   execution record (steps, verdicts, artifact hashes), approvals, and
   signatures — as a directory of Markdown plus the JSON it was rendered
   from. Rendered *from* structured data, same posture as `diff_html`.

**Tests.** Hash is stable across a round-trip (parse, serialise, hash).
Approval field round-trips and an old engine ignores it. `verify` fails on
a one-byte change. Export contains every control id the run record names.
The secret-scan corpus runs over an export.

**Not in A.** Authenticating identities, key management, a hosted approval
UI. Those are the commercial layer (charter DECIDE 1) and sit outside this
repository.

### B. Trace format as a standard

**Today.** `docs/trace-format.md` and three JSON Schemas exist and are
ratchet-enforced. The format doc flags one unresolved policy: an *older*
engine reading a *newer* trace with an unknown `selectors[].tier` fails
schema validation instead of degrading. A format with an open
forward-compatibility policy is not yet something a second implementation
can target.

**Build:**

1. **Decide forward-compat.** Proposal: unknown selector tiers are skipped
   with a logged reason and the next tier in the ladder is tried; a step
   whose *every* tier is unknown fails loudly. `payload`/`params` already
   accept unknown fields; this extends the same posture to the one closed
   enum. Amend the "Versioning" section; loosen the schema enum to a
   pattern; add the replay test.
2. **Conformance corpus.** `crates/flowproof-trace/conformance/`: a set of
   traces (minimal, multi-surface, cassette with deliveries, side-effect
   lane, every optional header block) plus expected parse results. A test
   walks the corpus. This is what a second implementation runs to prove it
   reads the same bytes the same way. Reuse committed fixtures where they
   exist; do not modify any committed cassette (invariant 8).
3. **A third wire shape at the cassette boundary.** The proxy in
   `agent_runner.rs` understands Anthropic Messages and OpenAI
   `chat/completions`. Add one more — the OpenAI Responses API is the
   obvious candidate, a decision-model API is the interesting one once a
   customer has it — as proof the `{request, response}` turn is provider-
   neutral rather than an assertion that it is. Matching rules in
   `cassette.rs` stay as they are.

**Tests.** Conformance walk is a required check. Old-engine-new-trace
degradation has a fixture. The third wire shape has a demo agent and a
committed cassette, same as `scripts/demo/`.

### C. Freeze authoring; plan for computer-use drivers

**Decision, not code.** `flowproof-agent` stops growing beyond what
recording needs: no new step grammar, no new prompt rules, no new
authoring heuristics. Bugs are still fixed; a fix ships with its test.
Model improvement is the upgrade path for grounding quality, not prompt
work.

**One design spike, written up as its own plan:** a driver backend that
*records* a lab computer-use agent instead of competing with it. flowproof
sits at the model boundary; a computer-use API is a model boundary. The
trace gains screen-and-action steps from the agent's own trajectory, the
selector ladder and the replay engine stay the executor. SAP GUI and
Windows desktop keep first-class support — those are the surfaces a
platform will not reach and where the regulated buyers are.

**Ledger rule for the loops.** Until the charter is amended (E), the
Ledger keeper treats "authoring convenience" gaps as `declined` with
reason `plan-14-freeze` unless the gap blocks *recording* a real suite.

### D. Confidence assertions at the model boundary

**Today.** `assert_tool_call` matches equality (`equals`, `contains`,
`exists`, `is absent`) against recorded tool-call arguments. A decision
model returns a distribution, not only a choice; the regression signal for
such an agent is "chose `charge_card` at 0.97 last month, at 0.61 today".

**Build, when a customer has such an agent to record (not before):**

1. **Record the distribution.** The cassette turn gains an optional
   `decision` block: `{"choice":"charge_card","confidence":0.97,
   "alternatives":{"refund":0.02,…}}`. Additive; absent for chat-shaped
   models. Schema and format doc in the same commit.
2. **Assert on it.** `assert_tool_call: charge_card with confidence at
   least 0.9`. Same prose grammar as `agent_steps.rs`, one new clause,
   parsed into an `ArgExpectation` sibling. Replay evaluates it against the
   recorded distribution — zero model calls, as always.
3. **Drift report.** `flowproof run --since` already diffs run records.
   Extend the diff to surface confidence movement on the same choice.

**Tests.** Every new assertion can fail (charter, Milestone 2 extension).
A cassette without `decision` blocks replays byte-identical.

### E. Charter reposition — proposal for a human

`CHARTER.md` §1 leads with cost and flakiness: "a suite that cost money per
commit and flaked on sampling becomes free and repeatable." Both shrink
when agents run on cheap decision models. The durable claim is already in
the same section ("deterministic to execute and cheap to review") — it
should lead. Proposed wording for the founder to apply, verbatim or not:

> Test an AI agent the way you test everything else: run it once, keep the
> recording, assert against it from then on. **The recording is pinned and
> reviewable**: what the agent did is fixed at record time, changes to it
> are diffs a human approves, and replay reproduces it with no model, no
> key and no vendor in the loop. That it is also free and unflaky is a
> consequence, not the claim.

And a new invariant candidate: **11. An approval names a person.** A
recording or a heal diff that changes ground truth carries the identity
that accepted it. (This is what workstream A builds; the invariant is what
keeps a loop from building around it.)

## Constitution note

This plan does not modify `CHARTER.md`, `scripts/gate/`, `scripts/loop/`,
`.github/workflows/` or `CLAUDE.md`. Workstream E is text for a human to
apply. Workstream B.2's conformance walk wants to become a required check;
that is a workflow edit and is also a human act — until then it runs under
`cargo test --workspace`, which is already required.

## Sequence

1. **A.1 + A.2** (hash, heal approval field) — one PR each.
2. **E** — the founder edits the charter; the loops then work under the new
   §1 and, if adopted, invariant 11.
3. **B.1** (forward-compat decision) — one PR; **B.2** (conformance
   corpus) — one PR.
4. **A.3 + A.4** (sign/verify, evidence export) — one PR each.
5. **C** — the spike plan is written; the freeze is in force from the day
   this plan merges.
6. **B.3** and **D** — when a customer brings the agent that needs them.

## Out of scope

- Any new `app:` target (charter §3).
- A hosted approval service, identity provider integration, or key
  management. Commercial layer, outside this repository.
- Replacing the authoring backend with a decision model. Worth a spike for
  the "which listed target" sub-step once such a model exposes selection
  over a caller-supplied list with confidence; not before.
- Rewriting existing cassettes to carry `decision` blocks. Committed
  cassettes are human-only (invariant 8).

## Resolved decisions

- **Signatures are detached.** Keeps traces plain JSONL and the cassette
  ratchet unchanged.
- **flowproof records identity, never authenticates it.** The trace is the
  ledger; IAM is the authority.
- **Confidence assertions wait for a real agent.** Building D against an
  imagined API shape is the kind of speculative scope the charter's ledger
  exists to prevent.

## Open questions

1. **Canonicalisation for the content hash.** Byte-exact over the stored
   file is simplest and matches "a cassette is judged by shape, never
   contents". It also means a whitespace-only edit changes the hash — which
   is arguably correct for evidence. Decide before A.1.
2. **Where approvals live for a trace that is healed twice.** Append to the
   array (full chain) or replace (latest only)? Proposal: append; the chain
   is the audit trail.
3. **Which third wire shape first (B.3).** Responses API is cheap and
   common; a decision-model API is the one that tests the neutrality claim.
   Let the first customer ask decide.
4. **Does the freeze (C) need a ratchet?** A line-count ratchet on
   `flowproof-agent` would be mechanical but crude. Start with the ledger
   rule; add the ratchet only if the loops route around the rule.
