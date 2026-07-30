# flowproof charter

The steering document for the autonomous development loops. The Scout takes its
direction from here; the Ledger keeper checks proposed work against the
out-of-scope list; the Builder implements only what the Ledger has marked
eligible.

This file is part of the constitution — see `scripts/gate/constitution-check.sh`.
No loop may modify it. Changing direction is a human act, and this is where it
happens.

Four places are still marked **DECIDE** — 1 (commercial boundary), 5 (scope
budget), 6 (token budget) and 7 (escalation channel). They need a human's product
judgement and were deliberately not invented. Until each is resolved the loops
treat that area as out of scope rather than guess.

Decided so far: **Tier 3 open under a 3×-pass guard** (§3 — it was declined, and
that reversed), **diagnostics before coverage** (§4), **the ledger lives at
`docs/loop/ledger.yaml`** (§6), and the loop identities are **`AutomatorsAgent`
plus a workflow** (§9) — no new account, established by measurement rather than
assumption. Numbering is left as-is so earlier references still resolve.

---

## 1. Mission

Test an AI agent the way you test everything else: run it once, keep the
recording, and assert against it from then on.

flowproof sits at the model boundary. It captures a real run — every model
request and every tool-call decision — into a trace, then serves that recording
back on later runs. Replay makes **zero LLM calls**, so a suite that used to
cost money per commit and flake on sampling becomes free and repeatable.

What is asserted is behaviour, not prose: which tools were called, with which
arguments, in which order, and which were **not**.

The strategic bet: AI agents will write and maintain automation, and what will
matter is that their output is **deterministic to execute and cheap to review**.

---

## 2. Invariants

Absolute. A change that violates one is wrong **regardless of green CI**, and the
Adversary must refuse it on the gate-integrity lens even if every test passes.

1. **Replay makes zero LLM calls.** The product's headline claim. If replay
   touches the network for a model call, the claim is false.
2. **Replay never requires an API key.** A committed cassette replays for anyone,
   forever. `flowproof run scripts/demo/order-status.flow.yaml` must stay green
   with no credentials in the environment. *Verified while drafting this charter:
   it does — once the demo agent's own SDK is installed. How that failure
   presents when it is not is #188.*
3. **The workspace always builds on Linux and macOS** via the driver's stub
   backend. The driver is Windows-native; the workspace is not.
4. **Healing proposes a reviewable diff, never a silent mutation.** The moment
   flowproof rewrites a trace without a human seeing the diff, "deterministic to
   execute and cheap to review" is gone.
5. **Trace-format changes update `docs/trace-format.md` and the JSON Schema in
   `crates/flowproof-trace/schema/` in the same commit.** Already required by
   `CONTRIBUTING.md`; mechanically enforced.
6. **Versions move together** — Rust crates, Python wheel, npm package. Guarded
   by the `versions agree` CI job.
7. **Every operation is a library call returning structured results.** The CLI
   and MCP server are thin renderings over the same code paths, never a place
   where logic lives.
8. **A cassette is ground truth.** Adding one is normal work. *Modifying* a
   committed cassette silently redefines what correct means, so it is a
   constitution-level act: human only.
9. **No secret ever reaches a trace.** The secret-scan corpus exists for this;
   a leak is a release-blocking defect, not a bug.

---

## 3. What the loops may not do

Out of scope. A gap that lands here is **recorded and declined**, never built.
This section exists because the loops both request features and judge whether
they are needed — without a written boundary, that is unbounded.

- **No new `app:` target.** Six adapters ship (`web`, Windows desktop, `sap`,
  `vision`, `api`, `agent`). A seventh is a product decision, not a gap fix.
- **No Node or Python sidecar in the engine.** flowproof stays a single Rust
  binary. (See `docs/design.md` on why the recording agent is not an opencode
  wrapper.)
- **No dependency on DataMaker.** Sibling product, not a component.
- **No selector-engine growth to match another framework's idioms.** If a
  migration needs jQuery-style selectors or a Cypress plugin's semantics, that is
  a decline, not a feature. The selector ladder is deliberate.
- **No public API break** without a human. flowproof ships to npm and PyPI;
  revert does not help once someone has installed it.
- **No outbound contribution to third-party repositories.** The corpus is
  read-only. Unsolicited automated PRs would be flowproof's first impression on
  exactly the developers it wants as adopters.
- **No vendoring of third-party test code.** The corpus is a lockfile of
  `{repo, commit SHA, license, test path}`, fetched at run time. Copying GPL
  test code into an Apache-2.0 repo is a licensing defect a loop will not notice.
- **No weakening of the gate.** Deleting a test, adding `#[ignore]`, adding a
  `skip`/`xfail`, or relaxing an assertion to get green is never the fix.

> **DECIDE 1 — commercial boundary.** Is there work that is deliberately *not*
> open-source — a paid tier, hosted service, or enterprise adapter the loops must
> not build in this repo? Nothing in the repo states one, so the loops currently
> assume everything in scope is open. If that is wrong it needs saying here.

- **Tier 3 (web-suite migration) is open, under one guard.** It was declined,
  and the objection was real: Playwright/Cypress/Selenium originals are
  endemically flaky, so a verdict disagreement is ambiguous rather than
  informative. What has changed is that the decline was costing more than it
  saved — Tier 2's record leg needs a model credential the loops do not have, so
  declining Tier 3 as well left the fleet with no runnable migration work at all.

  The guard is the one §6 already named for a revisit: **an original suite must
  pass 3× consecutively before it counts as an oracle.** A candidate that cannot
  is a decline, not a flake to retry. This is what answers the objection —
  disagreement against a suite that has proven itself stable three times running
  is informative, and the ambiguity the decline was protecting against is what
  the guard filters out at candidate time rather than at verdict time.

  The condition originally set for revisiting — a measured revert rate from
  Tiers 1 and 2 to compare against — has **not** been met, and this reopens
  Tier 3 without it. That is a deliberate trade, recorded here so nobody later
  reads the reopening as evidence the comparison was made.

---

## 4. Current milestone

**Milestone 1 — make a failure say what actually failed.** Two issues, both on
the agent boundary, both the same defect class: a real upstream failure
presenting as a flowproof problem.

1. **#187** — a 401 upstream during `record` fails fast instead of retrying with
   backoff and looking like a hang.
2. **#188** — an agent that fails to start says so, and surfaces its stderr,
   instead of reporting "0 model calls" and blaming the replay.

**This comes before adding coverage, and the ordering is deliberate.** Tier 2's
entire oracle rests on telling a flowproof defect apart from a problem in the
corpus repo. Today a dead agent process is indistinguishable from a failed
replay — so a Migrator loop pointed at third-party agents would file confident,
worthless reports, and the frequency gate would happily promote a gap that was
never real. Diagnostics are not polish here; they are the precondition for the
loops' output being worth reading.

Exit criteria: both issues closed, each with a committed flow that proves the
message stays correct.

**Milestone 2 — close the agent-boundary coverage gap.** The README names the
paths that are built but thinly covered, and that list is the honest edge of the
product's central claim. Exit criteria, all testable:

1. The Anthropic Messages dialect has a recorded cassette and a green replay.
2. Streaming replay has a green offline replay.
3. `agent.url` services have a green offline replay.
4. The MCP boundary over streamable HTTP has a green offline replay.
5. No `record` leg on an agent path remains untested (the README currently admits
   these exist).

That is issue **#61**, and it is the prerequisite for the work engine's Tier 2 —
where the loops are strongest and flowproof is differentiated.

#187 and #188 are the same defect class and both sit on the agent boundary: a
real upstream failure surfacing as something that looks like a flowproof problem.
Tier 2 will generate more of these, because a corpus of third-party agents fails
in more ways than a demo does. Diagnostics quality is therefore not polish here —
it is what makes the loops' own output trustworthy.

---

## 5. Priority ordering

When the Scout must choose, lower number wins.

1. **A false green.** flowproof reports PASS where the truth is FAIL. This is the
   only defect class that destroys the product's value, because a customer's
   suite says "safe" when it is not. Always first.
2. **A broken invariant** from §2.
3. **An adopter cannot get to a first green run.** Adoption friction outranks
   features; the goose runs are the precedent, and #188 is the live example —
   found by running the README's own frictionless first command on a clean box.
4. **The current milestone** (§4).
5. **A gap the Ledger has marked eligible** — observed N≥3 times across M≥3
   distinct repositories.
6. **Documentation that is untrue.** This repo's history is full of docs-accuracy
   fixes; prose that describes code that does not exist is a defect.
7. Everything else.

---

## 6. The work engine

Full design in `docs/` (see the autonomous-development concept). The rules the
loops must follow:

**Tier order: 2, then 3, then 1. Tier 4 never autonomous.**

- **Tier 1 — API/HTTP suites** → `app: api`. External oracle, exact verdicts.
- **Tier 2 — agents and MCP servers** → `app: agent`. Usually no existing tests,
  so the oracle is internal (below). flowproof's differentiator; run it first.
- **Tier 3 — web UI suites** → `app: web`. **Open under the 3×-pass guard**
  (§3). An external oracle, once that guard has established the original is
  stable.
- **Tier 4 — desktop/SAP/Citrix.** No public corpus, needs Windows and licensed
  software. Human-driven, always.

**Two sets, and every corpus entry names which it is in.** The tiers say what
kind of thing a candidate is; the sets say which job it serves. The cleavage is
**the `agent` adapter against all the others**, because that is where the
preconditions differ:

| `set:` | Tiers | `app:` | Precondition |
|---|---|---|---|
| `agents` | 2 | `agent` | a model credential reachable from the sandbox, for the record leg |
| `adapters` | 1, 3 | `api`, `web` | tier 3 also needs a browser, and an original that passes 3× |

`adapters` is runnable today and `agents` is not, which is the whole reason for
the distinction: with one undifferentiated pool, a blocked record leg reads as a
dry queue rather than as one lane blocked and one open.

**Tier 4 is an adapter and still stays out of `adapters`.** The set is a
*prospecting* lane, and there is no public corpus of SAP or Windows suites to
prospect. It is named after the adapters rather than after a tier because that
is where Tier 4 work would go if it ever became autonomous — but today a loop
may not put a Tier 4 candidate in it.

**Acceptance is never "the migrated test passes."** Agreement plus demonstrated
sensitivity:

- Verdicts must match the original: `PASS/PASS` or `FAIL/FAIL`.
- **Non-vacuity by mutation** — break the system under test; both suites must now
  fail. A migration that still passes when the original fails asserts nothing and
  is rejected. No migration enters the corpus without this.
- `FAIL → PASS` (original fails, flowproof passes) is a **false green**: priority
  1, always.

**Tier 2 oracle**, where no reference implementation exists:

- **R1** replay reproduces the recorded tool calls exactly — same calls,
  arguments, order.
- **R2** N replays agree byte-for-byte.
- **R3** containment proves replay made no network call.
- **R4** non-vacuity: perturb the agent (swap the system prompt, remove a tool,
  inject an adversarial response) and the assertion **must** flip to failing.
- **R5** guard inversion: patch the agent so it *does* make a forbidden call;
  `assert_no_tool_call` must fail. Enforcement, not compliance.

What Tier 2 validates is **flowproof**, not the third-party agent. The output is
flowproof defect reports and a public corpus, never an assessment of someone
else's agent.

**The frequency gate.** A gap is eligible only after **N≥3 observations across
M≥3 distinct repositories**. First occurrence is recorded, not built. The loop
that felt the pain must not authorise the fix: Migrator → Ledger → Builder is
one-way.

**The ledger lives at `docs/loop/ledger.yaml`.** Tracked, so it is reviewable in
a diff — the whole point of a ledger you can read. It cannot live under `.loop/`,
which `.gitignore` marks as local scratch and "not product". `docs/` is also
already in the CI scope filter's docs-only allowlist, so a ledger write skips the
engine jobs: correct for a bookkeeping file, and it keeps loop overhead cheap.

Only the Ledger keeper writes it. The Migrator emits gap *observations*; the
Builder reads `status: eligible` and nothing else.

> **DECIDE 5 — scope budget.** A cap on net new public API per period, so the
> loops prefer one generalisation over five special cases. No number can be
> derived from the repo. Suggested starting point: **2 net new public API items
> per week**, which forces consolidation without stalling the milestone.

---

## 7. Quality bar

- **Conventional Commits** with optional crate scope (`feat(trace):`).
- **Small, incremental commits.** A PR over ~400 changed lines escalates to a
  human: large diffs are where review stops working.
- **The CHANGELOG explains why, not what.** The existing voice is distinctive —
  it names the thing that was wrong, why it mattered, and what now holds instead.
  Match it. Read the last few entries before writing one.
- **A fix ships with the test that proves it stays fixed.** Every defect the
  loops find becomes a committed flow.
- **Docs and code move together.** Prose describing code that does not exist is a
  defect (§5.6).
- **`cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, and the
  test suite must pass locally before pushing.** Never pipe these — a pipe's exit
  code masks the real one, and a Builder that pipes `cargo build` will confidently
  push a failed build.

---

## 8. Escalation

Stop and label `needs-human` rather than guess:

- Any change to a **public API**, the **trace format**, or a **committed
  cassette**.
- Any **invariant** (§2) that seems to need an exception.
- Any gap the Ledger cannot place in or out of scope.
- **Three failed attempts** on one issue. Unassign, label, stop. No infinite
  retry.
- Anything that would require **outbound action** on a third-party repository.
- A **licence** on a corpus candidate that is not on the allowlist.

**Circuit breaker.** The Warden halts the fleet — writing a `HALTED` sentinel
every loop checks before starting — when `main` is red for more than ~20 minutes,
after 2 consecutive auto-reverts, when the token budget is spent, or when an issue
has burned its attempt budget repeatedly. Only a human clears it.

> **DECIDE 6 — token budget per period.** The one irreducible spend is recording
> against real models; replay is free. Needs a number, and a decision on whether
> the loops may use third-party agents' own API keys when a corpus repo requires
> one.

> **DECIDE 7 — the escalation channel.** `needs-human` on a GitHub issue is the
> default. Is that enough, or should the Warden's daily digest go somewhere you
> actually read?

---

## 9. Identity requirements

The design assumes independence that GitHub must enforce, not the prompts:

- **Builder ≠ Adversary.** Distinct GitHub identities, so the Adversary's
  approval satisfies `required_approving_review_count: 1`. GitHub blocks
  self-approval, which makes this free once the identities exist.
- **No loop identity may bypass a ruleset.** Bypass is evaluated on the actor's
  **role**, not the token's scopes — so a fine-grained token on an admin account
  bypasses review however narrow its permissions are. Loops need their own
  `write` identities. Enforced by `scripts/gate/token-scope-check.sh` check 5.
- **Ledger ∉ {Migrator, Builder}.** The one-way gate of §6.

**Mechanism: one existing machine account, and a workflow.** No new account is
needed — measured, not assumed (see below).

| Identity | Role | Form |
|---|---|---|
| `AutomatorsAgent` | Builder, Migrator, Prospector, Ledger keeper | collaborator, `write` |
| `github-actions[bot]` | Adversary — the approving reviewer | a workflow |

`write` is the whole point for the Builder: it is **not** in the ruleset's bypass
list (`OrganizationAdmin`, `RepositoryRole: 5`), so it cannot merge without a
review. It must never be granted `maintain` or `admin` —
`scripts/gate/token-scope-check.sh` check 5 refuses to start if it is.

Its token is a fine-grained PAT scoped to this repository only: Contents,
Pull requests and Issues write, Metadata read, nothing else. Never `Actions`,
`Administration` or `Workflows`. It lives in a `0600` env file outside any
container mount, on a 90-day rotation.

**The Adversary needs no account at all**, and that is better than a second
machine user rather than merely cheaper. Its logic lives in `.github/workflows/`,
which is constitution-protected *and* unreachable by a loop token lacking
`workflow` scope — so the Builder cannot influence what reviews it. A bot
account's prompt would sit in a mutable file; this does not.

### Measured, not assumed

Two probes settled questions the documentation left open. Both ran against this
repo and both branches were deleted afterwards; an auto-approving workflow must
never reach `main`.

| Question | Result |
|---|---|
| Does a `github-actions[bot]` approval satisfy `required_approving_review_count: 1`? | **Yes** — `reviewDecision: APPROVED`, `mergeStateStatus: CLEAN` (#191) |
| Does the repo ruleset's `dismiss_stale_reviews_on_push: true` beat the org ruleset's `false`? | **Yes** — the approval went to `DISMISSED` and `reviewDecision` fell back to `REVIEW_REQUIRED` (#192) |

The second matters most: without dismissal the Adversary approves a benign diff,
the Builder pushes anything afterwards, and the approval still stands. GitHub's
most-restrictive-wins aggregation across stacked rulesets holds for this
behaviour, so the hole is **provably** closed rather than probably closed.

The stale probe approved on `opened` only. Had it also fired on `synchronize` it
would have re-approved the instant the second commit landed and masked the very
thing being measured — exactly one workflow run confirms it did not.

Note also that `copilot-pull-request-reviewer` posts `COMMENTED`, never
`APPROVED`, so the org-wide Copilot review does **not** satisfy the gate. Only a
deliberate approver does.

The independence the design assumes is now **enforced by GitHub rather than by
prompts**: the Builder cannot merge without a review it cannot give itself, and
an approval does not survive a later push.

Two things remain before a loop may run unattended, and neither is a design
question:

1. **The Adversary workflow does not exist yet.** Only that the mechanism works
   has been proven. Until it is written, nothing supplies the required approval
   and the Builder simply cannot merge — which is the safe failure direction.
2. **The Builder's fine-grained PAT has not been minted**, so
   `scripts/gate/token-scope-check.sh` checks 2–5 have never run against a real
   token. Run it once before trusting it.

**The allowlist names humans who work on this repository, not everyone with
admin.** `scripts/gate/constitution-check.sh` allows `AminChirazi` and
`HappyDevs1`, the two people who have authored pull requests here.

Three further org admins — `RatulMaharaj`, `romanrehm`, `JonsBori00` — are
deliberately **not** on it. They can already bypass the review ruleset and, with
`enforce_admins: false`, merge past a failing required check. Adding them to the
allowlist would widen who can change the loops' *direction* to people who do not
work on the loops, which is a different and larger permission than the one they
already hold.

So the residual is named rather than closed: **an org admin can merge a change to
the constitution past a failing `constitution` check.** That is the human escape
hatch working as designed — the same property that lets a human clear the circuit
breaker and revert a bad merge. It is not a hole, because the check still fails
loudly and visibly in the pull request; what it is not is a wall.

The property that actually matters is unaffected: **no loop identity can bypass
anything.** `AutomatorsAgent` holds `write`, which is not in the ruleset's bypass
list, and `scripts/gate/token-scope-check.sh` refuses to start if that ever
changes.

Keep this in step with reality. If someone starts contributing, add them; if
someone stops, remove them. An allowlist nobody maintains drifts into either
noise or an obstacle, and both teach people to route around the gate.
