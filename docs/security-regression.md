# Deterministic security regression and control validation

Status: **design**. Nothing in this document is implemented; nothing here
changes what ships today. It exists so that when this capability axis is
built, it is built as one coherent piece rather than accreted assertion by
assertion. The concrete seed already in flight is the egress containment
work (`agent.allow_egress` + `assert_no_egress`, see
`crates/flowproof-trace/src/egress.rs` and
`crates/flowproof-adapters/src/egress.rs` on its branch); this design
generalizes that seed without changing it.

## The problem

Teams that pass a security review once have no standard way to prove the
reviewed controls still hold on every subsequent CI run. The failing
pattern in practice:

- Access control is tested manually before an audit ("log in as a viewer,
  confirm the delete button is gone"), then never again. Six months later
  a refactor drops the permission check and nobody notices until the next
  audit, or the incident.
- The checks that DO run in CI are unit tests of the authorization
  function, not of the deployed behavior: they prove the function works
  when called, not that the route still calls it.
- Evidence for auditors is assembled by hand: screenshots, ticket links,
  a spreadsheet mapping controls to "how we know". None of it is produced
  by the systems it describes, and all of it goes stale the day it is
  written.
- Security tooling that does run continuously (scanners, fuzzers, DAST)
  answers a different question: "can you find a NEW weakness?" Its
  verdicts are probabilistic, its findings need triage, and it cannot be
  a merge gate, because it can fail on an unchanged system.

Two problems hide in "test the security", and they are very different:

1. **Validating that known controls hold**: the team has already decided
   what must be true (a viewer cannot delete, the service talks only to
   its declared hosts, secrets never appear in output). The test asks:
   is that property still true, on this build, with evidence? This is a
   regression-testing problem and it can be made **fully deterministic**.
2. **Discovering unknown weaknesses**: pentesting, fuzzing, scanning.
   That is an exploration problem: probabilistic coverage, ranked
   findings, expert triage, no fixed expected output.

flowproof takes on **problem 1**. Problem 2 is explicitly out of scope
(see the decision at the end): a deterministic replay engine is the wrong
runner for exploratory security work, and pretending otherwise would make
both worse.

### The binding invariants

Everything below is constrained by the invariants the engine already
holds, stated here so no later section can soften them:

- **Deterministic replay.** No step type may fail on an unchanged system
  ([design.md](design.md)). A control assertion's verdict is a function
  of the recorded run, never of a clock, a sample, or a score.
- **Assertions are minted at record time.** A trace is only minted for a
  run that satisfied the spec, and drift produces a reviewable diff for
  human approval, never a silent mutation.
- **Secrets stay `${VAR}` references.** Resolved at the moment of use on
  record and on every replay, never stored
  (`crates/flowproof-trace/src/secret.rs`). A security feature does not
  get an exemption from the security property.
- **Library-first.** The capability lands in the engine crates; the CLI,
  MCP tools, and audit report are thin renderings of library results.
- **Exploratory and statistical capabilities are exiled.** Same rule that
  keeps evals out ([agent-testing.md](agent-testing.md)) and
  `page.evaluate` out ([design.md](design.md)).

## The key simplification: a control is a property that must hold

A security control is not a special kind of test. It is a **property that
must hold, expressed as a deterministic assertion over a recorded flow**.

That one sentence does all the work, so develop it:

- "A property that must hold" means the team already knows what true
  looks like. The spec states it; nothing is discovered at run time.
  This is exactly the shape of every assertion the engine already has:
  `page does not show`, `assert_no_tool_call`, `assert_api` expecting a
  `403`. The security framing changes what the property is ABOUT, not
  what kind of thing it is.
- "Deterministic assertion" means the verdict is decidable from what the
  run observed, and identical on every replay of an unchanged system.
  A control assertion never samples, never scores, never consults a
  knowledge base that updates under it.
- "Over a recorded flow" means the evidence is the trace. The run that
  proves the control also records the proof: the attempt that was made,
  the denial that answered it, the boundary that held. An auditor reads
  what happened, not a claim about what would happen.

The consequence is that this axis needs almost no new machinery. The
engine already performs actions as a chosen identity (`session:`,
[getting-started.md](getting-started.md)), already asserts absence
(`does not show`, `is not visible`, `assert_no_tool_call`), already
probes out-of-band state (`assert_api`, `assert_sql`,
[authoring.md](authoring.md)), and already produces a reviewable evidence
bundle per run. What is missing is small and specific: one coherent
grammar for "this must never occur" (instead of a per-feature zoo), a way
to name a control stably so a suite becomes a coverage map, and an audit
rendering of results that already exist.

## The control-assertion grammar

This section is the gate. Every future "security assertion" proposal is
an instance of this grammar or it is rejected; the grammar itself grows
only by design review, never by shipping one more special case.

### The general form

The engine already has two whole-run negative guards, and they have the
same shape:

- `assert_no_tool_call: <tool> [where ...]` - no tool call matching the
  predicate occurred anywhere in the model-boundary trajectory
  ([agent-testing.md](agent-testing.md)).
- `assert_no_egress` - no undeclared destination was contacted, judged
  over the egress supervisor's whole-run log, failing as a **capability
  error** when containment was not enforced (the egress containment
  design).

Generalized, a **control assertion** is:

```text
assert_no_<lane>[: <selector>] [where <path> <matcher> <value> [and ...]]
```

with exactly these semantics, all inherited, none new:

- **`<lane>` names an observation lane the engine records** - a stream of
  events captured identically at record and replay. The lane list is
  CLOSED: `tool_call` (the model-boundary trajectory), `egress` (the
  containment supervisor's log), and `secret_leak` (the run's captured
  outputs, defined in its own section below). A lane not on the list is a
  parse error naming the list, exactly as the `style` allowlist behaves
  ([authoring.md](authoring.md)).
- **The scope is the whole run, regardless of the step's position** -
  the rule `assert_no_tool_call` already set. A control that holds only
  between steps 3 and 5 is not a control, it is a race.
- **`<selector>` and `where` clauses narrow the forbidden event**, reusing
  the existing matcher vocabulary unchanged: `equals` (alias `is`),
  `contains`, `matches`, `exists`, `is absent`. No new matchers ride in
  with this feature.
- **Absence is only certified over an observed lane.** If the platform
  could not enforce or observe the lane (egress containment off Linux, a
  `url:` service flowproof does not own), the assertion **fails as a
  capability error stating why** - it never passes vacuously. This is the
  egress design's honesty rule (`Containment::NotContained` is printed,
  and `assert_no_egress` cannot certify over it), promoted to a property
  of the whole family: a control assertion never certifies what it
  cannot observe.
- **The verdict is deterministic.** The lane's contents at replay are the
  recorded contents, so a control assertion can never fail on an
  unchanged system. It is judged at record time too: a trace is only
  minted for a run where every control held, the same rule every
  assertion already follows.

### One grammar, four subjects

The test of a general form is that the concrete cases fall out of it
without special pleading:

```yaml
# 1. A forbidden call must never fire (shipped today).
- assert_no_tool_call: issue_refund where status equals approved

# 2. Data must never cross an undeclared network boundary (in flight).
#    The allow-set is declared as policy on the flow (see "Declared
#    surface" below), and the assertion certifies the log is clean.
- assert_no_egress

# 3. A secret must never appear in the run's outputs (this design).
- assert_no_secret_leak: ${DB_PASSWORD}

# 4. An unauthorized action must never SUCCEED (this design).
#    No new step at all: the forbidden event here is the success state,
#    and the shipped negative grammar already spells it. The flow
#    performs the attempt, then asserts the denial and the absence of
#    effect:
- assert: page shows Access denied
- assert: page does not show Deleted
- assert_api:
    request: DELETE ${API}/customers/4711
    headers: { Authorization: "Bearer ${VIEWER_TOKEN}" }
    status: 403
- assert_sql:
    connection: main
    query: SELECT count(*) FROM customers WHERE id = 4711
    equals: "1"
```

Case 4 is deliberately NOT a new `assert_no_*` subject. "Unauthorized
access" is not a lane the engine observes; it is an attempt the flow
performs plus a denial the existing grammar asserts. Forcing it into the
`assert_no_*` shape would mean inventing an "access" event stream that
does not exist, which is exactly the kind of grab-bag growth this section
exists to refuse. What unifies case 4 with the others is not the spelling
but the control identity, which the `control:` block below provides.

### The admission test for a new lane

A proposed `assert_no_<lane>` subject is admitted only if ALL of:

1. The engine records the lane's events **deterministically at both
   phases** - the events are facts of the run, present in the trace or
   derivable from it byte-for-byte.
2. The forbidden event is **decidable from the lane alone** - no judge,
   no threshold, no external knowledge base.
3. The engine can be **honest about non-observation** - there is a
   defined capability error for every platform or configuration where the
   lane cannot be enforced or captured.

`tool_call` passes (the cassette is the lane). `egress` passes (the
supervisor's log is the lane, and `NotContained` is the honesty story).
`secret_leak` passes (the scanned corpus is defined below). Examples that
FAIL the test, and stay out: "no suspicious behavior" (not decidable),
"no known-vulnerable dependency" (external knowledge base that updates
under the verdict), "no anomalous timing" (statistical).

## Access-control regression

The highest-value control in practice: **identity X must be denied
action Y**. As a flow, it is three ordinary moves - become the identity,
perform the attempt, assert the denial - and every one of them uses
shipped grammar.

### Identities are fixtures, declared once

The `session:` block already injects an authenticated identity before the
page loads, with `${VAR}` cookie values resolved at apply time and never
stored ([getting-started.md](getting-started.md)). Access-control flows
need the same thing, times N identities, shared across a suite. The
proposal is a suite-level `identities:` map whose entries are exactly the
`session:` shape:

```yaml
# suite.yaml
identities:
  viewer:
    cookies:
      - name: app.session
        value: ${VIEWER_SESSION_COOKIE}   # resolved at apply time, never stored
  admin:
    cookies:
      - name: app.session
        value: ${ADMIN_SESSION_COOKIE}
    local_storage:
      role: admin
```

A flow references one by name, or keeps an inline mapping exactly as
today (a name and a mapping are distinguished by YAML type, so nothing
shipped changes meaning):

```yaml
session: viewer          # a string: resolved against suite identities
```

An unknown name is a parse error listing the declared identities. The
secret property is inherited, not re-argued: every value in an identity
is a `${VAR}` reference resolved at apply time on record and on every
replay, so the trace carries role NAMES and variable NAMES, never
credentials (`crates/flowproof-trace/src/secret.rs`). How the sessions
are minted is outside the engine, exactly as it is for `session:` today:
a suite `before_each`, or an external fixture script, populates the
variables.

### The denial flow

```yaml
name: A viewer cannot delete a customer
app: web
url: ${APP_URL}/customers
control:
  id: ac.customers.delete.viewer-denied
  title: Viewer role is denied customer deletion
session: viewer
steps:
  - Click "Grace Hopper"
  - assert: the "Delete" button is not visible        # the UI withholds it
  - assert_api:                                       # the API refuses it
      request: DELETE ${API}/customers/4711
      headers:
        Authorization: Bearer ${VIEWER_TOKEN}
      status: 403
  - assert_sql:                                       # the state is unchanged
      connection: main
      query: SELECT count(*) FROM customers WHERE id = 4711
      equals: "1"
```

Three layers of evidence, deliberately: the UI hiding the button is
necessary but famously insufficient, so the flow also proves the API
refuses a direct request and the record survives. All three are shipped
assertion forms; the only new token in the file is the `control:` block.

The denial itself must be asserted POSITIVELY where the app expresses one
(`status: 403`, `page shows Access denied`, an empty result set), not
only as absence of success. An attempt that times out or errors for an
unrelated reason must read as a broken test, never as a passing control.

Record and replay work exactly as for any web flow: the attempt is
recorded once, the assertions are minted with the trace, and every CI run
replays the attempt deterministically. When the app legitimately changes
(the denial message is reworded, the button moves), that is ordinary
drift: heal proposes a diff, a human approves it - and because the flow
carries a control id, the review is explicitly a review of control
evidence, not just of a selector.

## Declared surface: boundary and egress assertions

The egress containment design established the pattern for network
boundaries, and its decisions are adopted here wholesale rather than
redesigned:

- A flow **declares** the destinations its process may reach
  (`agent.allow_egress`, default-deny).
- Enforcement is live in BOTH phases: record and replay run under the
  same containment, so the two executions stay equivalent.
- A blocked attempt is a **logged event** (destination, protocol,
  monotonic ms), not a silent drop, so it is assertable and diffable.
- `assert_no_egress` certifies a clean log, and **fails as a capability
  error** on a platform where containment is not enforced
  (`Containment::NotContained` names the reason). It never certifies
  what it cannot observe.
- The allow-set is **policy, not observation, so it does not travel in
  the trace** - unlike `mock:` rules, which are part of the recorded
  environment. Tightening policy must be able to fail a replay; that is
  the point of policy.

The generalization is one move: the declaring party stops being only
`agent:` and becomes **any flow that spawns the process under test**. An
`app:` mapping flow (a Windows program, [authoring.md](authoring.md)) or
any future spawned-process driver gets the same block under a
capability-neutral name:

```yaml
name: Exporter reaches only its declared services
app:
  command: '"C:\Program Files\Exporter\exporter.exe" --run'
  window_title: Exporter
surface:
  egress:
    - api.example.com:443
    - ${DB_HOST}:5432        # resolved at execution, never stored
steps:
  - Press the "Export" button
  - assert: page shows Export complete
  - assert_no_egress
```

`surface.egress` entries use the exact grammar `allow_egress` shipped
(`host:port`, bare host, `ip:port`, `cidr:port`, `${VAR}` deferral -
`crates/flowproof-trace/src/egress.rs`); on an `app: agent` flow,
`agent.allow_egress` remains the spelling and `surface:` is not a second
one. The honesty rule generalizes with it: a flow flowproof did not spawn
(`url:` services, `app: web` pages, vision-attached windows) has no
containable process, so `surface:` there is a parse error saying so, not
a silently unenforced wish. The browser's network boundary already has
its own first-class tool (`mock:` intercepts inside the browser), and the
two are not merged: `mock:` shapes what the page sees, `surface:`
contains what a process may touch.

`surface:` is named for the future it must not overreach into: a
`surface.filesystem:` or `surface.data:` axis is imaginable, but each
would need its own enforcement mechanism with the same
enforced-or-capability-error honesty, and none is designed here. The
block exists so that when one is, it has a home instead of a new
top-level key.

## Secret-handling regression

The engine already guarantees the TRACE never stores a secret: `${VAR}`
references resolve at the moment of use and only the reference is written
(`crates/flowproof-trace/src/secret.rs`). That protects flowproof's own
artifacts. It says nothing about the APP under test, which can happily
render a connection string into an error page or echo a token into an
API response. That is the leak a control must catch.

```yaml
- assert_no_secret_leak                       # every ${VAR} the flow referenced
- assert_no_secret_leak: ${DB_PASSWORD}       # or one named secret
```

Semantics, as an instance of the control-assertion grammar:

- **The lane is the run's captured outputs**, defined as a closed corpus:
  the page text the surface shows at each recorded step (the same text
  `page shows` reads), every `assert_api` response body, and on agent
  flows the model-boundary trajectory and MCP lanes. A closed corpus,
  not "everything", because a control must name what it checked; the
  audit output lists the corpus so nobody mistakes it for a proof about
  channels the engine never saw (server logs, third-party sinks).
- **The forbidden event is an occurrence of a resolved secret value** in
  that corpus. The value resolves at execution on record and on every
  replay and is scanned in memory; the trace stores only the variable
  NAMES asserted. A leak failure message says which variable and where,
  and never prints the value.
- **Whole-run scope**, like every control assertion: position in
  `steps:` does not narrow it.
- **Capability honesty**: a corpus element the platform cannot read as
  text (a vision flow's OCR frame is lossy, a screenshot is pixels) is
  excluded from the corpus BY NAME in the report, and a flow whose
  corpus would be empty fails as a capability error rather than
  certifying nothing.
- A secret value too short to be meaningful (under a small fixed length)
  is a parse-time error on the assertion, because scanning for `"1"`
  would fail on any page showing a 1: a control that cannot be asserted
  precisely is not weakened, it is refused.

Determinism holds because the corpus at replay is the recorded corpus
and the resolved values are the same environment indirection replay
already performs. An unchanged system cannot fail this step.

## Evidence and audit output: the control map

The run bundle already contains the evidence: the trace, the step
artifacts, the divergence or verdict. What an auditor needs ON TOP is
small, and it is naming, not new capture:

**A control gets a stable id.** Flow-level block, at most one per flow:

```yaml
control:
  id: ac.customers.delete.viewer-denied
  title: Viewer role is denied customer deletion
```

The id is author-chosen, dotted, lowercase (`[a-z0-9._-]+`), and its one
hard property is STABILITY: it survives renames of the flow file, moves
between suites, and re-records, because it is the join key between "what
the auditor tracks" and "what CI ran". Uniqueness is enforced per suite;
a duplicated id is a parse error naming both flows. A recommended (not
enforced) convention is `<domain>.<resource>.<action>.<expectation>`, and
teams mapping to an external framework put the mapping in `title` or in
their own catalog, keyed by the id - flowproof does not model SOC 2 or
ISO clauses, it provides the stable key those mappings need.

**The audit report is a rendering, not a new pipeline.** A suite run
already yields per-flow verdicts; `flowproof audit` (CLI and MCP, both
thin over the library, per the library-first rule) folds the flows that
carry a `control:` block into a control-coverage report:

```yaml
# audit.yaml (rendered from run results; also emitted as JSON)
suite: customer-portal
run: 2026-07-24T09:14:03Z
controls:
  - id: ac.customers.delete.viewer-denied
    title: Viewer role is denied customer deletion
    flow: flows/viewer-cannot-delete.flow.yaml
    verdict: pass
    evidence: runs/2026-07-24/viewer-cannot-delete/   # the ordinary bundle
    asserted:
      - the "Delete" button is not visible
      - assert_api DELETE /customers/4711 -> 403
      - assert_sql count unchanged
  - id: net.exporter.egress.declared-only
    title: Exporter reaches only its declared services
    verdict: capability-error
    reason: egress containment is Linux-only; this platform is not contained
```

Three verdicts, not two: `pass`, `fail`, and `capability-error`, kept
distinct so a report can never launder "we could not check" into "it
held". Diffing two reports over time is the coverage map: controls
added, controls removed (a REMOVED control id is called out, because
silently dropping a control is itself a security regression), and
controls whose evidence changed (a heal touched the flow - the diff link
is the review artifact).

That is the whole enterprise-audit hook, deliberately. No dashboards, no
retention policy, no signing, no framework ontology inside flowproof:
the report is a stable, diffable file that external tooling can ingest,
and everything richer builds on it from outside.

## Phasing

1. **v1 - the smallest useful slice.** The `control:` block with id
   uniqueness, the access-control regression pattern (suite
   `identities:` + `session:` by name; the denial flow is already
   expressible with shipped grammar), and the audit rendering
   (`flowproof audit` over run results, YAML + JSON). The control
   grammar's rules are normative from v1 even though v1 adds no new
   `assert_no_*` lane: the family and its admission test are the gate
   new proposals must pass.
2. **v2 - declared surface beyond agents.** `surface.egress` on spawned
   `app:` mapping flows, riding the egress containment mechanism and its
   containment-tier honesty unchanged.
3. **v3 - `assert_no_secret_leak`.** The corpus definition, the
   in-memory scan on both phases, the never-print-the-value failure
   shape.
4. **v4 - the full coverage map.** Report diffing across runs,
   removed-control detection, and whatever the field demands of the
   JSON shape once real audit tooling consumes it.

## Open questions

Settled here so implementation does not relitigate them:

- **Is the grammar one vocabulary or a grab-bag?** One vocabulary, held
  by the admission test: a new `assert_no_<lane>` subject needs a
  deterministic recorded lane, a decidable forbidden event, and a
  defined capability error. Access-denial deliberately stays OUT of the
  family (it is a flow pattern over shipped grammar), which is the
  proof the gate has teeth.
- **How does a control get a stable identity?** A flow-level `control:`
  block with an author-chosen dotted id, unique per suite, stable across
  renames and re-records. flowproof enforces uniqueness and stability
  mechanics only; compliance-framework mapping lives outside, keyed by
  the id.
- **How are multiple identities declared and injected?** Suite-level
  `identities:` whose entries are the `session:` shape; flows reference
  by name; values are `${VAR}` refs resolved at apply time. Minting the
  sessions is fixture work outside the engine, as it already is today.
- **THE LINE - what makes a proposed feature in scope?** The test, and
  it is the spine of this whole document: **does it assert that a known,
  named property held on this run, decidably from the recorded lanes,
  with a verdict that cannot change on an unchanged system - or does it
  search for unknown weaknesses?** Asserting a known property holds:
  in scope. Searching, scanning, scoring, sampling, ranking: out. Every
  in-scope section above passes it; every out-of-scope item below fails
  it. A future proposal that needs a paragraph to explain which side it
  is on is on the wrong side.

## Out of scope, by name

- **Active pentesting, exploitation, fuzzing.** Exploratory and
  non-deterministic: the whole value of a fuzzer is inputs nobody
  enumerated, so its verdicts cannot be replayed and its runs cannot be
  evidence of an unchanged system. Wrong engine philosophy, and also a
  different market with different trust and liability expectations: a
  tool that attacks systems is sold, consented to, and audited
  differently from a tool that replays tests. If it is ever built, it is
  a SEPARATE product on shared primitives, never a step type here.
- **Vulnerability scanning and CVE knowledge bases.** A scanner's
  verdict changes when the database updates, so an unchanged system
  fails on Tuesday that passed on Monday - the exact property the engine
  exists to forbid. Beyond that, CVE coverage is a content and research
  operation with a full-time pipeline behind it, not a framework
  feature.
- **Runtime and production policy enforcement.** The containment
  supervisor as a live production gateway is an adjacent strategic fork
  with its own availability, latency, and fail-open-versus-fail-closed
  questions. It is designed elsewhere if ever; this document covers
  enforcement during a TEST RUN only.
- **Anything statistical or scored.** Risk scores, anomaly detection,
  "security posture" ratings. A score is not a property that holds; the
  same rule that keeps model-output evals out
  ([agent-testing.md](agent-testing.md)) keeps these out.

## Decision: offensive and exploratory security is out of scope

The second problem - "can you find a NEW weakness?" - needs attack
generation, mutation, coverage feedback, and expert triage. Its verdicts
are probabilistic, not deterministic, and its artifacts are FINDINGS
that need judgment, not traces that need review. A future offensive
product could exist as a *separate* tool sharing primitives with this
one (the trace format for reproducing a finding, the containment
supervisor for sandboxing an attack run, the identity fixtures for
authenticated testing), but the replay engine's promise ("recorded once,
passes forever unless the system changed") must not be blurred by a step
type that can fail on an unchanged system - and every exploratory
technique fails on unchanged systems by design, because that is what
searching means. Blurring the two would also blur what a green run
MEANS: today it is a certificate that the recorded properties held, and
the moment one step type means "we looked and found nothing today", no
step means anything. Same philosophy as the `page.evaluate` rejection in
[design.md](design.md) and the eval rejection in
[agent-testing.md](agent-testing.md): protect the invariant that makes
the tool trustworthy.
