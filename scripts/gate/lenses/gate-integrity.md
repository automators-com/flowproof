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
