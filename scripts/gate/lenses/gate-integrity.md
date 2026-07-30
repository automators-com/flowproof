# Lens: gate integrity

**Did this change make the gate weaker?**

This is the lens a generic review never applies, and the one the whole system
depends on. An agent optimising for green has two routes - do the work, or weaken
the thing that measures the work - and the second is always cheaper.

The ratchets already refuse a deleted test, a new `#[ignore]` or skip, a modified
cassette, a schema change without its documentation, and an oversized diff. You
are looking for what they cannot express:

- an assertion relaxed rather than removed - `assert_eq!` become `contains`, a
  specific error become "any error", a count become "at least one";
- a test that still exists but no longer exercises the path it names;
- a new code path with no test at all, hidden behind changes that do have tests;
- a timeout, retry or tolerance widened until a real failure passes;
- a check moved somewhere it runs less often, or gated behind a condition that is
  usually false.

**Weakening the gate to get to green is never the fix.** Refuse it, and say which
specific assertion got weaker.

## Falsifiability suite (Milestone 2, criteria 6–8) — review protocol

A falsifiability test proves an assertion can fail by feeding it a
violating input and asserting the run FAILS. "Prove assertions can fail"
is never licence to weaken anything; adding a red-path test is the only
sanctioned mechanism. For any PR touching `tests/falsifiability/` or
claiming Milestone 2 criteria 6–8:

REFUSE, regardless of green CI, if the diff anywhere:

- modifies an existing test, committed fixture, or the accepted set of any
  assertion implementation. Sole exception: a defect fix in an assertion,
  which must (a) make the assertion stricter or correct, never more
  accepting, (b) ship the red-path regression fixture that would have
  caught the defect in the same PR, and (c) demonstrably fail that fixture
  on the pre-fix code;
- introduces `#[ignore]`, skip, xfail, a commented-out test, a loosened
  matcher, or any relaxation of an existing assertion (§3 restated);
- makes a red-path test behave by changing the assertion rather than the
  input;
- touches `scripts/gate/` — including this protocol.

VERIFY affirmatively:

- the fixture genuinely violates the asserted property — read the fixture,
  do not trust the test's name;
- the test checks BOTH the FAIL verdict and the non-zero exit code, as two
  checks at two layers (the streaming false green survived because
  equivalence was checked at the wrong layer);
- streaming cases exercise the streaming transport itself, not a buffered
  equivalent;
- the test would go red if the assertion under test were replaced by an
  unconditional PASS — if you cannot name the flowproof defect this test
  would catch, refuse.
