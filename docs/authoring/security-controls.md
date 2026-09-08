---
title: "Security controls"
description: "Naming a control, access-control regression patterns, assert_no_secret_leak, and the flowproof audit control map."
---

A security control is not a special kind of test. It is a property that
must hold, expressed as an ordinary deterministic assertion over a recorded
flow: a viewer cannot delete, a secret never surfaces in output. The forms
below add just enough to NAME a control stably and to assert one class of
"this must never appear" that the shared grammar could not spell before. The
access-control pattern needs no new step at all (see below); it is composed
from grammar you already have.

What v1 ships, stated plainly so nothing here is mistaken for more:

- The `control:` block on any flow (a stable id for coverage).
- `assert_no_secret_leak: ${VAR}`, the **named form only**, on `app: agent`,
  `app: web`, and `app: api` flows (the scanned corpus is the agent trajectory,
  the page surface text, or the `assert_api` response bodies). A flow kind with
  no readable corpus fails as a capability error, not a vacuous pass.
- `flowproof audit`, the control map. It reads the structured run record
  `flowproof run` persists (never re-replays), renders each control-bearing
  flow's verdict with an evidence pointer, and with `--since <run-id>` diffs
  two runs by control id (added, removed, verdict-changed).

### Naming a control: the `control:` block

A flow-level block, at most one per flow, gives the control a stable id:

```yaml
name: A viewer cannot delete a customer
app: web
url: ${APP_URL}/customers
control:
  id: ac.customers.delete.viewer-denied      # required
  title: Viewer role is denied customer deletion   # optional
  description: >-                              # optional
    The viewer session may read a customer but the API refuses its DELETE.
steps: [ ... ]
```

The `id` is author-chosen, dotted, lowercase (`[a-z0-9._-]+`); a value with
whitespace or an out-of-range character is a parse error. Its one hard job
is STABILITY: it survives renames of the flow file, moves between suites, and
re-records, because it is the join key between what an auditor tracks and
what CI ran. `title` and `description` are author metadata. A recommended
(not enforced) convention for the id is
`<domain>.<resource>.<action>.<expectation>`. Teams mapping to an external
framework (SOC 2, ISO) keep that mapping in their own catalog keyed by the
id; flowproof models no compliance ontology, it provides the stable key.

**Uniqueness is a suite property.** Two flows in one suite sharing a control
id is a suite-load error naming BOTH flows, because a duplicated join key
would corrupt the coverage map. A lone `flowproof run` on a single flow sees
only that flow, so it neither checks nor needs uniqueness.

### Access-control regression (a pattern, not a step)

The highest-value control in practice is "identity X must be denied action
Y". It is NOT a new `assert_no_*` subject. "Unauthorized access" is not a
lane the engine observes; it is an attempt the flow performs plus a denial
the shipped grammar already asserts. So the flow is three ordinary moves:
become the identity, perform the attempt, assert the denial.

The one rule that makes it a real control: **a denial is only evidence when
the same run proves the identity was alive.** If the app returns `403` for
both an unauthorized-but-valid session AND a dead one (an expired token, a
logged-out browser), then a credential that quietly expired reads as a
PASSING control while testing nothing. So a denial flow MUST also assert that
the identity is entitled to succeed at something: a `200` on an action it is
allowed, or a UI fact only the signed-in session shows. A denial flow with no
liveness assertion is an incomplete control.

The worked example lives at
[`examples/access-control/`](../examples/access-control/): a `suite.yaml`
declaring identities and a `viewer-cannot-delete.flow.yaml` that carries the
liveness proof and the denial side by side. See it for the full flow.

### `assert_no_secret_leak: ${VAR}` (v1)

The engine already guarantees the TRACE never stores a secret (`${VAR}`
resolves at the moment of use, only the reference is written). That protects
flowproof's own artifacts. It says nothing about the APP under test, which
can render a connection string into an error or echo a token into a response.
That is the leak this control catches.

v1 ships the **named-selector form only** (one `${VAR}`, or a list):

```yaml
- assert_no_secret_leak: ${DB_PASSWORD}        # one named secret
- assert_no_secret_leak:                       # or several at once
    - ${DB_PASSWORD}
    - ${API_TOKEN}
```

Semantics, all inherited from the shared grammar:

- **The lane is the run's captured outputs.** Which outputs depends on the
  flow kind (detailed below): a closed corpus, not "everything", so the control
  can name what it checked. Channels the engine never observed (server logs,
  third-party sinks) are out of scope and the audit output says so.
- **The forbidden event is an occurrence of the resolved secret value** in
  that corpus. At execution (record) and on every replay, each asserted
  `${VAR}` is resolved through the same resolve-refs machinery and the
  in-memory corpus is substring-scanned for the resolved value. The trace
  stores only the variable NAMES; the value is never written or printed.
- **Whole-run scope.** Position in `steps:` does not narrow it.
- **Only names travel.** A failure names every matching variable (in a stable
  order, so a run leaking two secrets reports both), the corpus element it
  appeared in, and the step index. It never prints the value.
- **A secret too short to scan is refused, not weakened.** A resolved value
  under a small minimum length (4 characters) fails the run at execution, in
  the same shape as the `MissingSecret` error, naming the variable and the
  minimum but never the value (scanning for `"1"` would fire on any page
  showing a 1).

**Bonus: the record-time scan is a store-guard.** On an agent flow the
model-boundary trajectory is persisted into the trace as a cassette, so a
leaked secret would otherwise be written to disk. The scan runs BEFORE the
trace is minted, so a leak fails the run and NO trace is written: the leaked
secret never reaches disk. Determinism holds because the corpus is
re-observed by the same mechanism at both phases, so an unchanged system
yields the same scan and the same verdict.

The corpus depends on the flow kind: an `app: agent` flow scans the
model-boundary trajectory and its MCP lanes; a `web` flow scans the surface
text read at each step boundary (not page source, and not continuously
between steps); an `api` flow scans each `assert_api` response body. A flow
kind with no readable corpus fails as a capability error rather than passing
vacuously.

One thing is deliberately NOT in v1: the **bare** form ("scan for every
`${VAR}` the flow referenced") is deferred until a suite-level `secrets:`
declaration gives it a defined domain (`${APP_URL}`, `${API}`, and minted
test data legitimately appear in output, so a bare scan would false-fail on
nearly every flow).

### `flowproof audit`: the control map

A suite run already yields per-flow verdicts and writes one structured run
record at `.flowproof/runs/<run-id>/report.json`. `flowproof audit <dir>` READS
that record and folds the flows that carry a `control:` block into a
control-coverage report. It never re-replays: the verdicts come from the record
`flowproof run` wrote, so audit is a pure rendering and stays fast and
side-effect-free. If no run has been recorded yet, audit refuses with an error
pointing you at `flowproof run` rather than silently re-running anything.

```text
$ flowproof run examples/access-control              # writes the run record
$ flowproof audit examples/access-control            # YAML on stdout
$ flowproof audit examples/access-control --json     # JSON instead
$ flowproof audit examples/access-control --run <id> # a specific past record
```

```yaml
suite: access-control
run: 2026-07-24T09-14-03Z-a1b2
controls:
  - id: ac.customers.delete.viewer-denied
    title: Viewer role is denied customer deletion
    flow: viewer-cannot-delete.flow.yaml
    verdict: pass
    evidence:
      trace: viewer-cannot-delete.trace.jsonl
  - id: sec.assistant.no-db-password-leak
    title: The DB password never surfaces in agent output
    flow: assistant-no-leak.flow.yaml
    verdict: pass
    lanes: [secret_leak]
    evidence:
      trace: assistant-no-leak.trace.jsonl
    secrets_checked: ["${DB_PASSWORD}"]        # variable names, never values
    corpus:
      - model-boundary trajectory (cassette request and response bodies)
      - MCP lanes
    excluded:
      - channels the engine never observed (server logs, third-party sinks)
```

Each control row carries an `evidence` pointer to the trace its proof lives in
(and, for a contained agent flow, any egress destinations containment blocked),
so a reader can go from the coverage map to the underlying artifact. Blocked
destinations appear only when THIS run was contained: they are read from the
recorded trace, so a recording made under containment and replayed on a host
without it would otherwise present another machine's blocks as evidence here.

A flow that engages egress also carries `containment:` - the tier the run
actually ran under (`enforced (linux seccomp)`, or the honest reason it was
not). `lanes` says what the flow ASSERTED; `containment` says what was
ENFORCED. On a host where the mechanism does not exist the flow can still
pass, so without this field a passing row would imply a certification the
run never made.

**Diffing runs.** `flowproof audit <dir> --since <run-id>` compares the latest
record against an earlier one, folded by `control.id`: controls **added**,
controls **removed** (present in the older record, gone in the newer - coverage
that shrank), and controls whose **verdict changed** (old -> new). It exits
non-zero on a regression - a removed control or a control that changed to
`fail` - so CI catches coverage silently shrinking.

```text
$ flowproof audit examples/access-control --since 2026-07-24T09-14-03Z-a1b2
```

```yaml
base: 2026-07-24T09-14-03Z-a1b2
head: 2026-07-24T11-02-55Z-9f3c
added:
  - id: ac.orders.refund.viewer-denied
    verdict: pass
removed: []
changed:
  - id: sec.assistant.no-db-password-leak
    old: pass
    new: fail
```

Three verdicts, kept distinct so a report can never launder "we could not
check" into "it held":

- `pass` - the control held on replay.
- `fail` - the control did not hold. `flowproof audit` exits non-zero when
  any control failed.
- `capability-error` - the platform could not enforce or observe the lane,
  or the flow never ran (a missing trace is a capability error naming the
  `flowproof record` to run, never a silent pass).

`secrets_checked` / `corpus` / `excluded` appear only for a flow that ran a
secret-leak scan. The audit surface is a stable file external tooling can
ingest, sourced from the persisted run record at
`.flowproof/runs/<run-id>/report.json`. Both once-absent pieces now ship on top
of that record: **evidence pointers** (the `evidence.trace` on each control row)
and **cross-run report diffing** (`audit --since <run-id>`, including
removed-control detection). Retention keeps the most recent 10 records per
suite, pruned after each run, so the `--since` window stays bounded.
