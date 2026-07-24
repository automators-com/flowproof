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

That one line is a family, not a single parser: the optional `<selector>`
and the `where` clause are present, absent, or a different KIND of thing
per lane, so each lane has its own tight grammar and the table below is
normative, not the line above. What is shared is the meaning of "no
event on this lane matched", not a uniform syntax.

| lane | selector | `where` clauses | valid on app kinds |
| --- | --- | --- | --- |
| `tool_call` | tool name (string) | yes: full existing matcher vocabulary | agent flows (a model boundary exists) |
| `egress` | none (bare form only) | no | flows flowproof spawns and contains: `agent:` today, Linux-spawned `app:` mapping in v2 |
| `secret_leak` | `${VAR}` (one or more, a different selector KIND) | no | any flow with a readable output corpus |

Read the table as three distinct grammars that happen to rhyme:
`tool_call` selects by tool name and refines with `where`; `egress`
takes neither a selector nor a `where` and certifies the whole log;
`secret_leak` takes a `${VAR}` selector - an env reference, not a
predicate over event fields - and no `where`. The claim that this "reuses
existing matchers unchanged" is true of `tool_call` only, and is not
generalized to the other two.

With exactly these semantics, all inherited, none new:

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
- **`<selector>` and `where` clauses narrow the forbidden event where the
  lane supports them.** On `tool_call` this reuses the existing matcher
  vocabulary unchanged: `equals` (alias `is`), `contains`, `matches`,
  `exists`, `is absent`, and no new matchers ride in with this feature.
  `egress` takes no selector and no `where`; `secret_leak` takes a `${VAR}`
  selector and no `where`. The per-lane table above is the authority on
  which lane admits which.
- **Absence is only certified over an observed lane.** If the platform
  could not enforce or observe the lane (egress containment off Linux, a
  `url:` service flowproof does not own), the assertion **fails as a
  capability error stating why** - it never passes vacuously. This is the
  egress design's honesty rule (`Containment::NotContained` is printed,
  and `assert_no_egress` cannot certify over it), promoted to a property
  of the whole family: a control assertion never certifies what it
  cannot observe.
- **The verdict is deterministic.** The lane is observed by the SAME
  mechanism at both phases, so on an unchanged system the observation, and
  therefore the verdict, is identical - the same determinism grade as
  `page shows`, which re-reads live surface text at replay rather than
  serving it from the cassette. Only `tool_call` has the stronger grade
  where the recorded cassette IS the lane; `egress` is live containment
  enforcement at both phases, and the `secret_leak` corpus (page text,
  `assert_api` bodies) is re-observed live at replay. It is judged at
  record time too: a trace is only minted for a run where every control
  held, the same rule every assertion already follows. The corollary is
  that replay never diffs a live lane against the recorded one - the
  egress log's `at_ms` values alone would churn a byte-for-byte compare on
  every run - so the verdict comes from re-observing the lane, not from
  comparing it to a recording. And in a minted trace the recorded egress
  lane is empty by construction: record refuses to mint when a control
  failed, so a clean log is the only log a trace can carry.

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
#    effect - and, per "The denial flow" below, must ALSO carry a
#    liveness assertion proving the identity was really authenticated
#    (elided here; the full pattern shows it):
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

1. The engine observes the lane by the **same mechanism at both phases** -
   the events are facts of the run, so an unchanged system yields the same
   lane whether the lane is served from the cassette (`tool_call`) or
   re-observed live (`egress`, `secret_leak`).
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
today. The `session` field becomes an untagged string-or-mapping form -
a bare name resolves against suite identities, a mapping is inline setup -
with the same "distinguished by YAML type" rule the `app:` and `window:`
spec forms already use ([authoring.md](authoring.md)), so nothing shipped
changes meaning:

```yaml
session: viewer          # a string: resolved against suite identities
```

**Dereference is a load-time copy, not a runtime lookup.** When the flow
is LOADED, the named identity is dereferenced and its `${VAR}`-bearing
session setup is copied into the trace header EXACTLY as an inline
`session:` mapping is copied today. The trace therefore stays
self-contained: it carries the identity's setup, not a pointer to the
suite. The consequence is deliberate - a later edit to the suite's
identity definition is a re-record or heal event on the flows that use
it, never a silent change to existing traces, the same way editing an
inline `session:` is. A bare `session: viewer` in a flow with no
governing suite is a load-time error naming the missing suite (there is
nowhere to resolve the name), and an unknown name under a suite is a
parse error listing the declared identities.

The secret property is inherited, not re-argued: every value in an
identity is a `${VAR}` reference resolved at apply time on record and on
every replay, so the trace carries role NAMES and variable NAMES, never
credentials (`crates/flowproof-trace/src/secret.rs`). How the sessions
are minted is outside the engine, exactly as it is for `session:` today:
a suite `before_each`, or an external fixture script, populates the
variables.

**Scope of an identity: browser session only.** An `identities:` entry
carries the browser session - cookies and `local_storage` - and nothing
else. The denial flow below also needs `${VIEWER_TOKEN}` on an
`assert_api` call, and that token is NOT part of the browser session, so
it does not live in the identity block. API credentials stay plain suite
env by convention: `${VIEWER_TOKEN}` is a suite variable resolved at apply
time like any other, and the identity block is not stretched to model
exported HTTP credentials. This keeps the identity a faithful mirror of
the shipped `session:` shape rather than a new credential container; a
future flow that genuinely needs an identity to export non-cookie
variables is a deliberate extension to design then, not an accident of
this one.

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
  - assert: page shows Signed in as viewer            # the identity is ALIVE
  - assert_api:                                       # and entitled to succeed
      request: GET ${API}/customers/4711
      headers:
        Authorization: Bearer ${VIEWER_TOKEN}
      status: 200
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

Four layers of evidence, deliberately. The first two are the LIVENESS
proof: the page confirms the viewer session is signed in, and the same
`${VIEWER_TOKEN}` that will be refused on `DELETE` is first shown to
SUCCEED on a `GET` it is entitled to. The last three are the denial: the
UI hiding the button (necessary but famously insufficient), the API
refusing a direct request, and the record surviving. All are shipped
assertion forms; the only new token in the file is the `control:` block.

**A denial is only evidence when the same run proves the identity was
alive.** A bare `status: 403` is not proof the control held: if the app
returns `403` for both an unauthorized-but-valid session AND a dead one
(an expired or revoked `${VIEWER_TOKEN}`, a logged-out browser), then a
credential that quietly expired reads as a PASSING control while actually
testing nothing. So a denial flow MUST include a positive assertion that
the identity is entitled to succeed at something alongside the denial: a
`200` probe on an action the identity is allowed (as above), or a UI fact
only the logged-in session shows (`page shows Signed in as viewer`). This
is a stated requirement of the pattern, not a nicety - a denial flow
without a liveness assertion is an incomplete control, because it cannot
distinguish "correctly denied" from "never really authenticated".

The denial itself must also be asserted POSITIVELY where the app
expresses one (`status: 403`, `page shows Access denied`, an empty result
set), not only as absence of success. An attempt that times out or errors
for an unrelated reason must read as a broken test, never as a passing
control.

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
`agent:` and becomes **any flow that spawns the process under test AND can
contain it**. That second clause is load-bearing, because containment is
not universal. Egress containment is Linux-only seccomp: off Linux the
supervisor returns `Containment::NotContained` and `assert_no_egress`
fails as a capability error (`crates/flowproof-adapters/src/egress.rs`).
So v2 scopes `surface.egress` to a process flowproof **spawns on Linux**,
where the same seccomp mechanism the `agent:` seed already uses applies
unchanged:

```yaml
name: Exporter reaches only its declared services
app:
  command: ./bin/exporter --run     # a Linux binary flowproof spawns and contains
surface:
  egress:
    - api.example.com:443
    - ${DB_HOST}:5432               # resolved at execution, never stored
steps:
  - assert_api:
      request: POST ${API}/export
      status: 200
  - assert_no_egress
```

`surface.egress` entries use the exact grammar `allow_egress` shipped
(`host:port`, bare host, `ip:port`, `cidr:port`, `${VAR}` deferral -
`crates/flowproof-trace/src/egress.rs`); on an `app: agent` flow,
`agent.allow_egress` remains the spelling and `surface:` is not a second
one. The honesty rule generalizes with it, and cuts two ways. A flow
flowproof did not spawn (`url:` services, `app: web` pages,
vision-attached windows) has no containable process, so `surface:` there
is a parse error saying so, not a silently unenforced wish. And a process
flowproof spawns on a platform where seccomp does not exist - a Windows
`app:` mapping driven through UI Automation ([authoring.md](authoring.md))
- has no enforcement mechanism, so `surface.egress` is not offered there:
its `assert_no_egress` could only ever be a permanent capability error on
its one runnable platform, which is worse than absent. Windows containment
waits on a future host mechanism (Windows Filtering Platform), designed if
ever in its own work, not assumed here. The browser's network boundary
already has its own first-class tool (`mock:` intercepts inside the
browser), and the two are not merged: `mock:` shapes what the page sees,
`surface:` contains what a process may touch.

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

v1 ships the **named-selector form only** - one or more `${VAR}` names,
each a secret the flow declares must never surface:

```yaml
- assert_no_secret_leak: ${DB_PASSWORD}       # one named secret
- assert_no_secret_leak: ${API_TOKEN}         # or several, one per line
```

The BARE form - "scan for every `${VAR}` the flow referenced" - is NOT
buildable now and is deferred (see phasing). `${VAR}` is the engine's
universal env indirection, not a secret marker: `${APP_URL}`, `${API}`,
and `env_from`-minted test data (`${MATERIAL}`, `${SUPPLIER}`,
[authoring.md](authoring.md)) legitimately appear in page text and
`assert_api` bodies, so a bare scan of all referenced variables would fail
at record on nearly every real flow. The bare form is admissible only once
a suite-level `secrets:` declaration gives it a defined domain to scan
(the set of variables the suite calls secret), which is future work; until
then the author names each secret explicitly.

Semantics, as an instance of the control-assertion grammar:

- **The lane is the run's captured outputs**, defined as a closed corpus:
  (a) the surface text at each STEP BOUNDARY (the same text `page shows`
  reads), (b) every `assert_api` response body probed, and (c) on agent
  flows the model-boundary trajectory and the MCP lanes. A closed corpus,
  not "everything", because a control must name what it checked; the audit
  output lists the corpus so nobody mistakes it for a proof about channels
  the engine never saw (server logs, third-party sinks). Two exclusions
  are part of the definition, not caveats bolted on, and both are echoed
  in the audit output exactly as the OCR exclusion is:
    - **Step-boundary capture, not continuous.** Surface text is sampled
      when a step completes, not streamed, so a secret that flashes into
      the DOM and is gone before the next boundary is invisible to this
      control. It proves nothing about the gaps between steps.
    - **Surface text, not page source.** "Page text" is the surface/scene
      text `page shows` reads, NOT the page source, so a secret hidden in
      a `value` attribute of an off-screen field, an HTML comment, or a
      `data-` attribute is invisible. The control sees what a user sees,
      not what a `view-source` would.
- **The forbidden event is an occurrence of a resolved secret value** in
  that corpus. Match mechanism, identical at both phases: at execution on
  record and on every replay, resolve the asserted `${VAR}` names through
  the existing resolve-refs machinery (`crates/flowproof-trace/src/secret.rs`)
  and substring-scan the in-memory corpus for each resolved value. The
  trace stores only the variable NAMES asserted, never the values. A leak
  failure message names the VARIABLE, the corpus element it appeared in,
  and the step index, and it names ALL matching variables deterministically
  (not just the first one found), so a run that leaks two secrets reports
  both in a stable order. It never prints the value.
- **Whole-run scope**, like every control assertion: position in `steps:`
  does not narrow it.
- **Capability honesty**: a corpus element the platform cannot read as
  text (a vision flow's OCR frame is lossy, a screenshot is pixels) is
  excluded from the corpus BY NAME in the report, and a flow whose corpus
  would be empty fails as a capability error rather than certifying
  nothing.
- **A secret value too short to scan precisely is refused at execution,
  not parse.** The length of a `${VAR}` is unknown until it resolves, and
  it resolves at execution, not parse - so this cannot be a parse-time
  error. It is an execution-time refusal at BOTH phases, in the same phase
  and shape as the existing `MissingSecret` error: if a resolved secret is
  under a small fixed minimum length (scanning for `"1"` would fire on any
  page showing a 1), the run fails naming the VARIABLE and the minimum,
  never the value. A control that cannot be asserted precisely is not
  weakened, it is refused.

Determinism holds because the corpus is re-observed by the same mechanism
at both phases and the resolved values are the same environment
indirection replay already performs, so an unchanged system yields the
same corpus, the same scan, and the same verdict - the `page shows`
determinism grade, not a diff against a recorded corpus. An unchanged
system cannot fail this step.

**Bonus property: the record-time scan is also a store-guard.** On agent
flows the model-boundary trajectory is persisted into the trace as a
cassette, so a secret leaked into a cassette body would otherwise be
written to disk. The record-time scan runs before the trace is minted, so
a leak fails the run and NO trace is minted - the leaked secret never
reaches disk. The control that protects the app's output doubles as a
guard on flowproof's own artifact.

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
  - id: sec.portal.no-db-password-leak
    title: The DB password never surfaces in portal output
    flow: flows/no-secret-leak.flow.yaml
    verdict: pass
    secrets_checked: ["${DB_PASSWORD}"]        # variable names, never values
    corpus:                                     # what was actually scanned
      - surface text at each step boundary
      - assert_api response bodies
    excluded:                                   # echoed like the OCR exclusion
      - transient text between step boundaries (capture is per-step, not continuous)
      - page source not read as text (hidden fields, HTML comments, data- attributes)
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

**Precondition: PR #126 merges first.** The egress containment work
(branch `claude/flowproof-egress-containment`, PR #126) edits the same
`spec.rs` spine this design extends - the `control:`, `identities:`, and
`assert_no_secret_leak` additions all land on top of its changes. v1 is
sequenced AFTER #126 merges, not concurrently, so the two do not fight
over the spec spine.

**v1 - the smallest useful slice.** Four pieces, all buildable on shipped
mechanism:

  a. The `control:` block with per-suite id uniqueness (a duplicated id is
     a suite-load error naming both flows).
  b. Suite `identities:` + `session: <name>`, with the load-time-copy
     dereference semantics above: the named identity is copied into the
     trace header at load, the `session` field is the untagged
     string-or-mapping form, and a bare name with no governing suite is a
     load-time error.
  c. A `flowproof audit` rendering (YAML + JSON, the three verdicts
     `pass`/`fail`/`capability-error`), folded from the suite runner's
     EXISTING run report - a rendering of results that already exist, not
     a new pipeline.
  d. `assert_no_secret_leak: ${VAR}`, the NAMED form only, scanning the
     corpus defined in the secret ruling (surface text at step boundaries,
     `assert_api` bodies, agent trajectory + MCP lanes), with the
     both-phase in-memory scan and the never-print-the-value failure shape.

The control grammar's rules are normative from v1: the family and its
admission test are the gate new proposals must pass, even though the only
new lane v1 ships is `secret_leak`.

**Deferred, in rough order:**

- The **bare `assert_no_secret_leak`** form, which needs a suite-level
  `secrets:` declaration to give it a defined domain before it is safe.
- **`surface.egress` on spawned-app flows** (v2), Linux-scoped as above,
  riding the egress containment mechanism and its containment-tier honesty
  unchanged.
- **Report diffing and removed-control detection** (later): the full
  coverage map across runs, once real audit tooling consumes the JSON and
  a persisted run record exists to diff against (see open questions).

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
- **What does `flowproof audit` render FROM?** It needs a persisted,
  structured run record, and today the suite runner's verdicts live
  in-process plus a junit XML export (`crates/flowproof-replay/src/report.rs`)
  - neither is the durable structured record the audit YAML/JSON and its
  `evidence:` paths imply. The artifact-store layout that record would live
  in (`.flowproof/artifacts/`) is itself still an open question upstream -
  [design.md](design.md) lists "artifact store layout and retention" as
  open - so v1's `flowproof audit` is gated on that record existing, and
  the exact on-disk shape is deferred to wherever the artifact store is
  settled.
- **Where is control-id uniqueness checkable?** Only at SUITE LOAD. A
  standalone flow run sees one flow, so it cannot detect a duplicate id in
  a sibling flow; uniqueness is a suite-level property enforced when the
  suite loads, and a lone `flowproof run` on a single flow neither checks
  nor needs it. This is accepted, not a gap: the id's job is to be the
  join key across a suite's coverage map, which only exists at suite scope.
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
