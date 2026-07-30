# Milestone 2, criteria 6–8 — falsifiability suite

You are proving flowproof's assertions can fail. One issue = one assertion
type = one PR.

Definition of done, per issue:

1. A NEW fixture under `tests/falsifiability/fixtures/` that violates the
   property under test. Required shapes:
   - `assert_no_secret_leak`: a synthetic trace carrying a marked fake
     credential using the documented FAKE_SECRET_ convention. Never a real
     secret and never a plausibly real one — §2: no secret ever reaches a
     trace, including here.
   - `assert_no_egress`: a contained process that attempts exactly one TCP
     connect. Linux runner only.
   - `assert_tool_call`: a cassette whose tool-call sequence omits or
     reorders the required call.
   - `assert: reply contains` (streaming): a fixture that diverges only at
     the streaming-transport layer. Streaming is its own issue; do not
     fold it into the buffered case.
2. A test invoking the real assertion path end-to-end as a library call
   (§2: every operation is a library call). When a process exit code must
   be checked, capture it directly — never through a pipe: `cargo build |
   tail` returns tail's exit code; this has produced four false results
   already.
3. Two checks at two layers: the FAIL verdict, and the non-zero exit code.
4. One appended line in `docs/how-flowproof-tests-flowproof.md` naming the
   assertion, the fixture, and the defect class it guards.

Hard bars — the Adversary will refuse these even with green CI; do not
attempt:

- Do not modify any existing test, committed fixture, assertion
  implementation, or anything under `scripts/gate/`. Committed cassettes
  are ground truth (§2): craft new fixtures; never edit a committed
  cassette into a violation.
- If your red-path test PASSES where you expected FAIL — the assertion did
  not catch the violation — you have found a real false-green defect. This
  is a RESULT, not a task. Do not fix the assertion, do not adjust the
  test, do not weaken the fixture until it "works". Report it with the
  fixture attached for the Ledger keeper to record, exactly as a Migrator
  reports a false green. The fix is a separate issue you must not
  self-authorise.
- Fixtures are deterministic and offline. Replay makes zero LLM calls and
  requires no API key — inside this suite as everywhere (§2).

Sizing (one issue each): assert_tool_call; assert_no_tool_call;
assert_no_secret_leak; reply-contains buffered; reply-contains streaming;
egress containment; control: verdict mapping; audit --since exit codes;
plus one regression-fixture issue per CHANGELOG false-green defect.
