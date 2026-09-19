---
status: draft
---
# Plan 14: Pinned, reviewable, and useful for enterprise test authoring

No issue yet. This is a strategic proposal. Merging this document does not
activate a development freeze, amend `CHARTER.md`, or create a ledger rule.
Any policy or constitution change requires a separate, explicit human action.

## Problem

Flowproof has two related jobs:

1. turn a business scenario into an accepted, maintainable enterprise test,
   including a validated Tosca handover; and
2. make agent behavior deterministic to execute and cheap to review by
   recording, replaying, and approving changes.

Capable computer-use and decision models may replace parts of today's
authoring implementation. They do not by themselves solve grounding,
durable selectors, meaningful business assertions, test data, maintenance,
or Tosca handover. Likewise, evidence infrastructure has value only when it
supports a real test workflow.

The question is therefore not whether stronger models make authoring code
obsolete. It is:

> What is the least expensive, most reliable path from a business scenario
> to an accepted, maintainable Tosca test, with independently checked
> outcomes and a reviewable maintenance path?

Claims about a vendor model's price, latency, or capability are inputs to a
benchmark, not sufficient evidence for a roadmap freeze. Source line counts
also do not measure customer value.

## Decision

Pursue three immediate workstreams:

| # | Workstream | Outcome |
|---|---|---|
| A | Benchmark replaceable authoring backends | Evidence for where to invest or retire custom authoring |
| B | Ship a minimal evidence package | Exact artifacts and approved transitions that a reviewer can verify |
| C | Harden format compatibility | Safe interoperability with explicit rejection behavior |

Two further changes remain gated:

- confidence-specific assertions wait for a customer agent and fresh-response
  evaluation use case;
- charter repositioning waits until the benchmark and evidence package show
  that the proposed wording reflects the product in practice.

The engineering preference during the benchmark is:

> Prefer replaceable model-backed authoring over new bespoke heuristics, but
> continue work that demonstrably improves an agreed enterprise workflow.

There is no blanket authoring freeze in this plan.

## A. Benchmark the authoring replacement path

### Representative workflow

Choose at least one enterprise process that includes:

- application navigation and data entry;
- a business assertion that is independent of successful clicking;
- test-data setup or selection;
- at least one maintenance event, such as a changed label or selector;
- export or handover to Tosca; and
- a deliberately broken business outcome, so replay completion alone cannot
  qualify as success.

Compare three paths against the same acceptance criteria:

1. the current Flowproof authoring path;
2. a replaceable model-provider or computer-use backend behind a narrow
   Flowproof boundary; and
3. direct authoring in Tosca.

The spike may adapt existing drivers, adapters, and export code. It should
avoid introducing another permanent authoring architecture before results
are known. SAP GUI and Windows remain first-class benchmark surfaces.

### Measures

Record:

- total human minutes from scenario to accepted test;
- first-pass and final business-outcome assertion quality;
- false-green and false-negative results;
- selector robustness and maintenance effort after the planned change;
- test-data preparation effort;
- Tosca import, cleanup, and validation effort;
- elapsed time and provider cost; and
- reviewer confidence in understanding what changed.

The result must separate model work from Flowproof's durable responsibilities:
grounding, assertions, test data, replay, evidence, and Tosca handover.

### Decision gate

After the benchmark:

- retire or stop expanding custom authoring behavior that a replaceable
  backend matches with less total human effort;
- keep or improve behavior that materially reduces accepted-test effort or
  false greens; and
- document any remaining provider dependency behind an explicit interface.

No line-count ratchet or ledger decline rule is introduced. A later freeze
requires benchmark results and a separate approved policy change.

## B. Minimal evidence package

### One approval contract for both recording formats

Flowproof has JSONL traces and agent cassettes represented as JSON documents.
The approval mechanism must cover both. Do not place the common contract
inside only the JSONL trace header.

Use a detached, versioned approval manifest. A conceptual record is:

```json
{
  "version": 1,
  "kind": "heal",
  "subject": {
    "from": {"sha256": "<original-bytes>"},
    "to": {"sha256": "<replacement-bytes>"},
    "media_type": "application/vnd.flowproof.trace+jsonl"
  },
  "change": {"sha256": "<structured-diff-bytes>"},
  "approval": {
    "identity": {"value": "<identity>", "source": "<source>"},
    "at": "2026-09-19T10:12:33Z"
  }
}
```

The media type may instead identify an agent cassette. The final schema may
use paths or artifact identifiers for discoverability, but digests are the
authority.

Each approval binds:

- the exact original recording;
- the exact proposed replacement;
- a structured change description;
- the recorded identity and its source; and
- the approval time.

`flowproof heal accept` must fail if the current original no longer matches
the manifest's `from` digest. Repeated healing produces separate transition
manifests rather than mutating an approval history inside the recording.

### Hashing rule

Use SHA-256 over exact stored file bytes, with no parsing, normalization, or
reserialization. A whitespace-only change intentionally changes the digest.
Tests must not claim that a digest survives parse and reserialize.

A semantic digest can be added later only for a demonstrated use case and
must never be confused with the evidence digest.

### Identity and trust

Flowproof may record an identity supplied by `git config user.email`,
`FLOWPROOF_APPROVER`, a CI actor, or an authenticated external system. It
must not render all sources as equivalent proof.

Evidence output distinguishes:

1. **claimed**: a value recorded from the local environment;
2. **externally authenticated**: an identity assertion supplied by a named
   trusted integration; and
3. **signature verified**: a signature that verifies against an explicitly
   trusted key.

Recording an email address does not prove that a person approved a change.
The first release may support only claimed identity, provided the UI and
export label it accurately.

### Evidence export

`flowproof audit --export evidence/` emits a machine-readable manifest and
a human-readable rendering. The manifest binds by exact-byte digest:

- the spec or protocol;
- the recording;
- the execution record;
- all referenced artifacts;
- structured diffs and approval manifests; and
- signatures, if present.

The export must report missing, changed, or unverifiable artifacts. A signed
recording alone is not proof that its execution report or attachments are
complete.

### Delivery order

1. exact-byte hashing for traces and agent cassettes;
2. detached approval manifest and stale-proposal protection;
3. evidence export with completeness and integrity checks;
4. detached signing only after a trust model or pilot requires it.

### Required tests

- one-byte and whitespace changes alter the digest;
- manifests work for both JSONL traces and JSON agent cassettes;
- an approval binds both original and replacement;
- acceptance rejects a stale original or changed replacement;
- claimed identity is never displayed as authenticated;
- an export detects a missing or changed referenced artifact;
- every run-record control and artifact is present in the export manifest;
- secret scanning runs over the exported package; and
- signature verification, when built, fails for changed content and
  untrusted keys.

Authentication, key custody, and a hosted approval UI remain outside the
first implementation.

## C. Safe format compatibility

The near-term goal is a documented, interoperable format, not a declaration
that the format is already a standard.

### Compatibility policy

Document:

- the first reader version that implements each compatibility behavior;
- required versus optional fields and features;
- which unknown values may be skipped;
- which unknown values must fail closed; and
- how unsupported behavior is surfaced to users.

`SelectorTier` is currently a closed Rust enum. Relaxing JSON Schema alone
does not make existing readers forward-compatible. The parser, runtime
behavior, schema, documentation, and fixtures must change together.

An unknown selector strategy may be skipped only when:

- the format marks it as optional;
- a supported fallback remains; and
- the skip is visible in the run record.

A step with no supported selector fails loudly. Unknown assertions, safety
requirements, side-effect controls, or other required semantics must never
be skipped into a passing result.

### Conformance corpus

Add fixtures for both successful and rejected inputs:

- minimal valid recordings;
- multi-surface and side-effect cases;
- every optional header or document block;
- unknown optional selector with a supported fallback;
- unknown required feature;
- malformed recording;
- invalid approval transition;
- changed referenced artifact; and
- invalid or untrusted signature when signing exists.

The corpus records expected parse and execution outcomes. It becomes a
required check only through a separate authorized workflow change.

A third provider wire shape should be added only when it exercises a real
integration. Its purpose is to validate the cassette boundary, not to claim
provider neutrality from a single extra example.

## D. Confidence and fresh model evidence

Keep this work customer-gated.

Two capabilities must stay distinct:

- **offline regression** runs current surrounding code against pinned model
  responses and checks deterministic behavior;
- **model evaluation** records fresh responses from a candidate model or
  configuration and compares them with an accepted baseline.

Offline replay cannot detect that a live model's confidence changed. Fresh
recordings are always review candidates, never automatic baseline
replacements.

Do not standardize one generic `decision` object around an assumption that
probability and confidence are equivalent. When a real provider and customer
case exist, preserve the provider's question, choice, distribution, and
confidence semantics in a provider-neutral envelope. One response may
contain multiple decisions.

## E. Charter proposal, not activation

If the benchmark and evidence work validate the direction, a human may
separately propose wording that leads with pinned and reviewable behavior.
A candidate principle is:

> Test an AI agent the way you test everything else: run it once, keep the
> recording, and assert against it from then on. The recording is pinned and
> reviewable: what the agent did is fixed at record time, changes are
> explicit transitions a reviewer approves, and replay needs no model, key,
> or vendor.

Any approval invariant must distinguish a recorded identity from an
authenticated person. For example:

> An accepted change records who or what approved the exact transition and
> states the assurance level of that identity.

This plan does not modify `CHARTER.md`, `scripts/gate/`,
`scripts/loop/`, `.github/workflows/`, or `CLAUDE.md`.

## Sequence

1. Define the representative Tosca workflow and acceptance criteria.
2. Run the three-way authoring benchmark and publish the measurements.
3. Implement exact-byte hashing for both recording formats.
4. Implement detached approval manifests with stale-proposal protection.
5. Implement the bound evidence export.
6. Add the compatibility policy and positive and negative conformance corpus.
7. Decide authoring investment from benchmark evidence.
8. Add signing, another provider wire shape, confidence support, or charter
   changes only when their stated gates are met.

Each implementation PR remains within the repository's size limits and ships
with the test that proves its contract.

## Success criterion

> Less human effort per accepted enterprise test, with independently checked
> business outcomes and a reviewable maintenance path.

Evidence infrastructure succeeds when a reviewer can determine exactly what
ran, what changed, who or what approved that exact transition, and what level
of trust supports the identity claim.

## Out of scope

- a blanket authoring freeze;
- a line-count or ledger ratchet;
- automatic acceptance of newly recorded or healed baselines;
- a hosted identity provider, approval service, or key-management system;
- claiming confidence drift from pinned responses;
- declaring the trace format an industry standard; and
- rewriting committed cassettes to adopt speculative fields.

## Resolved decisions

- Merging this draft does not activate policy.
- Authoring investment is decided by an end-to-end Tosca benchmark.
- Evidence uses exact stored bytes.
- Approvals are detached transitions and cover both recording formats.
- An approval binds the original, replacement, and structured change.
- Identity assurance is explicit.
- Exports bind all evidence artifacts, not only the recording.
- Compatibility tests include rejection and fail-closed behavior.
- Confidence support waits for fresh evidence and a real customer API.

## Open questions

1. Which enterprise workflow and Tosca acceptance owner will be used for the
   benchmark?
2. What is the smallest structured diff representation that works for both
   trace and cassette transitions?
3. Which external identity assertion or signature trust model does the first
   regulated pilot require?
4. Which features in the current formats are required, optional, or safe to
   skip?
