# How flowproof tests flowproof

A test suite proves a tool does the right thing when the input is good. This
page is about the other half: proving each assertion flowproof ships can
actually **fail**.

The distinction is not academic here. In 0.9.0 the streaming replay path had no
test that could fail for its own bug — a streaming client handed one buffered
body assembles the identical final text, so `assert: reply contains` stayed
green for exactly the defect it existed to catch. A green suite said the feature
worked. It did not.

That is the failure mode this suite exists to make impossible: an assertion that
cannot fail is not an assertion, and a tool whose business is evidence cannot
afford one.

## What a red-path proof is

A **violating input** plus a check that the run **fails**. Never a change to
what an assertion accepts.

Each proof supplies a guilty fixture and shows flowproof convicts it. The
fixture is the criminal; the assertion is the judge. We test the judge by
supplying a criminal, never by bribing the judge — weakening an assertion until
a red path exists would produce a green suite that proves less than before.

Each proof checks **two layers**, because the streaming false green got through
by being checked at only one:

1. the **verdict** flowproof records, and
2. the **process exit code** a CI run would actually see.

A run that failed loudly while recording `verdict: pass` passes a one-layer
check and is exactly the defect worth catching.

## The proofs

| Property | Fixture | Guards against |
|---|---|---|
| `control:` verdict mapping | [`control-verdict-fail.flow.yaml`](../tests/falsifiability/fixtures/control-verdict-fail.flow.yaml) | a control whose flow FAILED being recorded as `pass`, or vanishing from the audit map instead of reporting `fail` — the map is what a reader trusts precisely when they cannot read the trace |
| `audit --since` regression gate | [`audit-since/`](../tests/falsifiability/fixtures/audit-since/) | a gate that cannot decline to fire. Existing tests prove it goes red on a regression; these prove it goes GREEN on a clean pair and on an added control, which is what makes its red worth acting on |
| `capability-error` as a regression | [`audit-since/head-capability-error.report.json`](../tests/falsifiability/fixtures/audit-since/head-capability-error.report.json) | a control that silently stops being certifiable slipping the gate. `capability-error` exists so "we could not check this" never reads as "this is fine"; until #263 that distinction was lost at the diff layer |
| `assert_no_tool_call` (the guard path) | [`guard-agent.py`](../tests/falsifiability/fixtures/guard-agent.py) | a guard assertion that cannot fail. Every other end-to-end use sits in a flow where the forbidden tool was never going to be called, so it would pass even if the assertion were an unconditional PASS |
| `assert: reply contains` | [`reply-missing-text.json`](../tests/falsifiability/fixtures/reply-missing-text.json) | the same shape of hole in the assertion that has already hosted one false green: every existing use asserts text the model was always going to produce, so none of them would notice the assertion ceasing to work |
| cassette call-order tolerance | [`two-call-agent.py`](../tests/falsifiability/fixtures/two-call-agent.py) | a tolerance that quietly became "the request is never checked". Reordering two INDEPENDENT calls must not diverge; changing what one of them SENDS still must |
| `assert_tool_call` argument matchers | [`tool-call-wrong-argument.json`](../tests/falsifiability/fixtures/tool-call-wrong-argument.json) | a matcher vocabulary that cannot fail. The tool-NAME layer is proven; every `where` clause in the suite asserts an argument the model was always going to produce. One guilty call violates both a value matcher (`equals`) and a presence matcher (`is absent`) |

### A note on gates specifically

For an assertion, the failure worth proving against is a false green. For a
**gate** — anything whose output is an exit code others branch on — there are
two, and the second is easy to forget: a gate that always fires is as useless as
one that never does, and it satisfies every test written only for the firing
direction. So a gate needs its discriminating case proved as well: not just
"red when it should be", but "green when it should be".

## Rules for adding one

- **Only ever add.** A red-path proof never modifies an existing test, fixture,
  or assertion implementation. If your proof only goes red once you have
  loosened something, you have written a weaker suite, not a stronger one.
- **Fixtures are committed evidence**, not throwaway strings. They live in
  `tests/falsifiability/fixtures/` so a reviewer can read what makes the input
  guilty without reading the harness.
- **Deterministic and offline.** Replay makes zero LLM calls and needs no API
  key, inside this suite as everywhere else. Where a live endpoint is genuinely
  required to mint an honest recording, bind loopback and let `${VAR}`
  indirection keep the host out of the trace.
- **Never join a server thread you cannot prove will be released.** A hung test
  reports nothing at all, which is worse than a failing one.
- **If your red-path proof comes back GREEN, you have found a false green.**
  That is the most valuable outcome available here, and it is a discovery to
  report rather than a bug to quietly fix — the fix is somebody's reviewed
  change, not a silent one made by the person who found it.
