# How the autonomous system works

flowproof is built partly by AI loops that run unattended: they find work, do it,
review it, and merge it. This is how that is arranged so quality does not drift.

## The problem

An agent asked to make CI green has two routes: **do the work, or weaken the
thing that measures the work.** The second is always cheaper. Delete a failing
test, add `#[ignore]`, re-record the expectation, relax an assertion to
"contains".

None of that is malice — it is the shortest path to the stated goal. So the
system is not built on the agent behaving well. It is built on two properties:

1. **The gate is harder to weaken than the work is to do.**
2. **The work is chosen so its correctness can be checked without a human.**

## The flywheel

```
  Prospector ──▶ Migrator ──▶ Ledger ──▶ Builder ──▶ Adversary ──▶ Integrator
   finds        runs their    counts     writes      reviews       merges
   public       tests, and    gaps       the fix                     │
   repos        ours, and     across                                 ▼
                compares      repos                                main
                                    ▲                                │
                                    └──── Warden watches, and halts ──┘
```

**Prospector** searches public repositories for test suites and MCP servers.
It records pointers — `{repo, commit SHA, licence}` — never copied code.

**Migrator** fetches one at its pinned SHA, runs *its own* suite inside a
container, then migrates it to flowproof and runs that. **The third party's
verdict is the oracle**: flowproof must agree with it. Agreement alone is not
enough — the migration must also *fail* when the code under test is broken,
proved by deliberately breaking it. A test that passes but asserts nothing is the
most common failure in generated tests, and this is what catches it.

The prize outcome is a **false green**: flowproof says PASS where the truth is
FAIL. That is the only defect class that destroys the product's value, because a
customer's suite would say "safe" when it is not.

**Ledger** collects what flowproof could not express. A gap becomes buildable
only after **three observations across three *distinct* repositories** — three
sightings in one project is that project's idiom, not a gap. This stops the loops
laundering a single opinion into a feature request, and the ranked table it
produces is evidence of what real suites need.

**Builder** takes one issue, one branch, one pull request. It must run the
formatter, linter and full suite locally and read the output before pushing.

**Adversary** reviews in two layers. **Ratchets** are mechanical and cannot be
argued with: test counts may not fall, `#[ignore]` may not appear, a committed
recording may not be edited, a diff over 400 lines is refused. **Four model
lenses** then look for what mechanism cannot express — correctness, gate
integrity, API compatibility, documentation truth — each as an independent
review, so a blind spot in one is not a blind spot in all.

**Integrator** merges. Deliberately *not* an AI — finding an approved, green pull
request needs no judgement, and a script cannot talk itself into a merge. It
verifies `main` afterwards, because `main` runs jobs the pull-request path skips.

**Warden** watches and can stop everything by writing one file that every role
checks before starting. It is biased toward halting: a stopped fleet costs idle
time, a running fleet with a broken gate costs the repository.

## Why it cannot quietly go wrong

- **The constitution.** The charter, the gate scripts, the role prompts and CI
  are the things that *constrain* the loops, and a loop cannot change them. An
  instruction its subject can rewrite is a suggestion.
- **Separated identities.** The Builder and the reviewer are different GitHub
  accounts, and neither can bypass branch protection. GitHub enforces this, not
  the prompts.
- **A sandbox for other people's code.** Third-party test suites run in a
  container with no credentials and no network during replay. Their `npm install`
  runs arbitrary code; this box holds deploy keys.
- **Fail-closed everywhere.** An unrecognised identity is treated as a loop. A
  reviewer that errors has not approved. A check that cannot answer refuses.

## What stays human

Direction, and anything not reversible. The charter; the constitution; changes to
the published API; clearing the circuit breaker. Everything else runs unattended,
and the Warden's daily digest is the window into it.

## Honest status

Every part is built and tested, and several are verified against reality — the
Migrator established a real oracle against a live third-party repository, and the
Warden's first turn caught a genuine day-old CI outage nobody had noticed.

The full chain has **not yet run end to end as a fleet**. Until it has, treat
this as a system that is ready to start rather than one with a track record.
