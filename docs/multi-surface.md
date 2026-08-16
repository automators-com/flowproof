# Test cases that span technologies

> Status: Phases 1 and 2 are **shipped**. `exports:` chains
> single-surface flows through a suite
> ([authoring.md](authoring.md#handing-a-value-to-the-next-flow-exports)),
> and multi-surface flows record AND replay
> ([authoring.md](authoring.md#multi-surface-flows-apps-and-in-blocks)):
> `apps:` + `in:` blocks, one surface active at a time, captures crossing
> blocks, per-step surface attribution, replay from the trace alone with
> zero LLM calls. Per-surface `browser:` and `window:` are shipped —
> vision surfaces included — `assert_screenshot` baselines are
> surface-qualified (`<name>@<surface>.png`), and `heal` runs on the same
> surface registry recording uses. **Nothing from Phase 2 remains open.**
> The one multi-surface shape still refused is a surface that names a
> `login:` — nothing stages a surface's credentials yet ([below](#phase-2--multi-surface-flows-shipped)).
> Phase 3 remains a proposal.

## The problem

A real end-to-end test case often crosses surfaces: SAP GUI creates the
order, the web portal must list it, and perhaps an agent or a native
Windows app acts on it after that. A flow drives exactly one surface — one
`app:`, one driver, chosen at launch — and that is a fact worth keeping
(one driver per flow is what makes record and replay simple to reason
about). But the *test case* must not be capped at one technology, or the
gap gets filled by hand-written harness scripts between flowproof
invocations: exactly the glue flowproof exists to replace.

The enabler is already in the architecture: every adapter implements the
one `AppDriver` trait, and the step grammar is adapter-agnostic — a step
does not know which surface executes it. Spanning technologies is a
dispatch and attribution problem, not a grammar redesign.

## Phase 1 — the suite is the test case (`exports:`, shipped)

Suites already mix app kinds (one directory can hold `sap`, `web`, `api`
and `agent` flows), `order:` pins sequencing, `env_from` mints shared test
data, and out-of-band assertions (`assert_api`, `assert_sql`,
`assert_screenshot`) work in any flow regardless of `app:`. The missing
piece was carrying a value a flow *learned* to the flows after it —
captures are flow-scoped, and suite `env` is fixed before any flow runs.

`exports:` closes that gap: a flow resolves `ENV_NAME: template` pairs
from its own captures when its last step passes, and the pairs become
environment variables for the suite's remaining flows. The downstream flow
references them as ordinary `${VAR}`s, so its trace stores only the
reference and the handoff happens fresh on every replay. Nothing is
persisted; an unresolvable export fails the flow that owns the captures; a
failed flow exports nothing.

This covers the dominant shape of cross-technology test case — *do in
system A, prove in system B* — with one driver per flow and no new trace
format. When a case genuinely ping-pongs between surfaces mid-flow, Phase 2
is the answer.

## Phase 2 — multi-surface flows (shipped)

One flow file, one trace, several named surfaces, exactly one active at a
time:

```yaml
name: Order across GUI and portal
apps:
  gui:    { app: sap, connection: ${SAP_CONNECTION} }
  portal: { app: web, url: ${PORTAL_URL} }
steps:
  - in: gui
    steps:
      - Go to /nVA01
      # ... create and save the order, then read the number from its own
      # field rather than out of the status bar's sentence ...
      - Go to /nVA02
      - Remember the "id:wnd[0]/usr/ctxtVBAK-VBELN" as order
  - in: portal
    steps:
      - Type ${captured.order} into the "Search" field
      - assert: page shows ${captured.order}
```

A sap surface takes `login:` too, with the same shape and the same rules as
on a single-surface flow — which is what a *same-system, two-user* case
wants, one flow with a `clerk` and an `approver` surface:

```yaml
apps:
  clerk:    { app: sap, connection: TS3, login: { user: obeva,    password: ${CLERK_PW} } }
  approver: { app: sap, connection: TS3, login: { user: approver, password: ${APPROVER_PW} } }
```

That parses and validates today. It does not RUN today: nothing stages a
surface's credentials yet, so `record` and `heal` refuse a surface that names
a `login:` — launching anyway would drive whatever SAP session was already
open, as whoever opened it, which is the exact confusion `login:` exists to
prevent. Until it lands, the same case is a suite of single-surface flows with
one `login:` each, chained with `exports:` — Phase 1, which is shipped.

Design decisions, each with its reason:

- **Explicit `in:` blocks, not per-step surface prefixes.** The prose
  grammar stays untouched inside each block, transitions are visible in
  the spec, the trace, and failure output, and a reviewer can see the
  seams.
- **Sequential activation; exactly one active surface.** Not merely
  simpler — correct on Windows. SAP GUI scripting, UIA and vision all
  inject real input into the foreground window; two live input-injecting
  drivers would fight over focus. Only CDP is out-of-band. A block
  boundary parks the current driver and foregrounds the next; interleaving
  is refused at parse time with the reason named.
- **One capture namespace across blocks.** That is the point of the
  feature: `${captured.order}` minted in the `gui` block types into the
  `portal` block. Capture semantics are unchanged; only their reach grows.
- **Lazy launch, kept alive.** A named app launches at its first block and
  stays up, so returning to `gui` in a later block resumes the same
  session.
- **A `windows`-mapping surface's `command`/`window_title` may reference a
  capture minted by an earlier block** — `${captured.download_path}` types
  into a launch command the same way it types into a field, resolved at
  the surface's actual activation rather than before any step has run
  (a value that does not exist yet cannot resolve). This is what lets a
  block that downloads a file hand its path to a later block that opens it
  in a different application: `Wait until the download completes as
  export`, then an `excel:` surface launched with
  `EXCEL.EXE ${captured.export}`. An unresolved capture at activation time
  (the minting block never ran, or ran on the wrong surface) fails the run
  closed, naming what was missing — never a launch against the literal
  `${captured.x}` text. `web`/`sap`/`vision` surfaces resolve the same way
  but arrive already fully resolved in practice, since nothing downstream
  of Fiori/SAP GUI login needs a value a flow only learns mid-run.
- **Backward compatible.** A bare `app: web` remains the single-surface
  flow it always was — the multi-surface form is additive vocabulary.

Trace format: the header's single `app` grows an additive optional `apps`
map, and step records gain an optional surface attribution — additive
optional fields, which trace v1 permits without a version bump. The
ratchet still applies: `docs/trace-format.md` and
`crates/flowproof-trace/schema/` move in the same commit as the format
change. Replay routes each recorded step to its recorded surface through a
driver registry holding one driver per named app.

Open questions Phase 2 must answer before it ships:

- `window:` geometry and `browser:` config become per-surface.
- `flowproof heal` heals multi-surface flows: healing is re-record-plus-
  diff, so the registry does the surface work, and a step that moved
  between surfaces diffs as a `surface` change. Shipped.
- `assert_screenshot` baselines carry the surface in their identity
  (`<name>@<surface>.png`), so a `gui` baseline can never be compared
  against a `portal` frame. Shipped.

Delivery is several small PRs (the ratchets refuse large ones): spec
parsing and refusals first, trace format with schema and docs second, the
record path third, the replay path fourth — each with its tests.

## Phase 3 — agent segments in a multi-surface flow (design, not yet code)

> Status: **proposal.** Everything below is design for discussion; the
> parser accepts none of it. Today an agent flow chains through a suite
> via `exports:` — its spec consumes `${ORDER_NO}` like any other flow —
> which covers "UI produces, agent consumes" without any of this.

What Phase 2 cannot express: an agent acting IN THE MIDDLE of a UI flow,
on values captured moments earlier, with the flow continuing on what the
agent did. The shape:

```yaml
name: Order triage across surfaces
apps:
  gui: {app: sap, connection: "${SAP_CONNECTION}"}
  assistant:
    app: agent
    agent: {command: "python support_agent.py"}
    tools: [...]
steps:
  - in: gui
    steps:
      # ... create and save the order, then read the number from its own
      # field rather than out of the status bar's sentence ...
      - Go to /nVA02
      - Remember the "id:wnd[0]/usr/ctxtVBAK-VBELN" as order
  - in: assistant
    steps:
      - prompt: Investigate order ${captured.order} and set its priority.
      - assert_tool_call: set_priority with order ${captured.order}
  - in: gui
    steps:
      - Press F5
      - assert: page shows Priority updated
```

### The four design decisions

**1. An agent surface is a surface entry, not a flow field.** The
`agent:`/`tools:`/`mcp:`/`strict:` blocks (today refused on multi-surface
flows) move INTO the surface entry, exactly as `url:` and `browser:` did.
The `agent` kind-refusal lifts only when the entry carries its `agent:`
block; the surface's steps are the agent step forms (`prompt:`,
`assert_tool_call:`, `assert_no_tool_call:`, `assert_no_egress`) and
NOTHING else — a UI step inside an agent block is a parse error naming
the two grammars.

**2. The cassette is a sidecar, referenced from one step.** An agent
trace is a single JSON document; a multi-surface trace is JSON-lines. Do
not merge the shapes: the agent block records as ONE step in the step log
— `action: {type: "agent_run", params: {cassette: "<stem>.cassettes/
assistant-1.json", sha256: …}}` — whose cassette lives in a sibling
directory, exactly the relocatable-bundle pattern baselines already use.
The step log stays diffable line-by-line; the cassette stays reviewable
as the document it is; the trace directory stays self-contained. An
engine predating `agent_run` fails loudly on the unknown action type.

**3. Captures cross INTO the seam; what crosses back is named.**
`prompt:` text interpolates `${captured.<name>}` — resolved at execution
on record and every replay, stored raw, the discipline everything else
follows. The reverse direction gets ONE new step form:

```yaml
- remember_answer: {matching: "/ticket (\\d+)/", as: ticket}
```

reading the agent's FINAL answer (a value the cassette already stores),
so later UI blocks can type `${captured.ticket}`. Tool results and
intermediate turns are deliberately not capturable in v1 — the final
answer is the agent's contract; mining its internals would couple flows
to trajectory details healing is allowed to change.

**4. Replay stays zero-LLM by construction; containment scopes to the
block.** Replaying an `agent_run` step replays its cassette — the same
executor `app: agent` flows use, fed from the sidecar — while UI steps
replay as today. Egress containment (where enforced) arms when the block
starts and disarms when it ends; `assert_no_egress` and the tool-call
asserts judge THAT block's cassette only. `assert_no_secret_leak`
remains refused on multi-surface flows until its corpus question is
answered for mixed lanes.

### Slices, when this leaves proposal

1. Vocabulary: agent surface entries + agent-step grammar inside their
   blocks + `remember_answer` (parse + validation + refusals, engine
   refuses at record like Phase 2's slice 1 did).
2. Trace: the `agent_run` action + cassette sidecar in schema and docs,
   same commit.
3. Record: the recorder runs the agent segment through the existing
   agent runner, writes the sidecar, stamps the step.
4. Replay: cassette replay behind the step, captures rejoined.

The arbitrary-Windows-app case needs no phase of its own: `app:
{command, window_title}` is one more entry in the Phase 2 `apps:` map.
