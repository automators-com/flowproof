# Test cases that span technologies

> Status: Phases 1 and 2 are **shipped**. `exports:` chains
> single-surface flows through a suite
> ([authoring.md](authoring.md#handing-a-value-to-the-next-flow-exports)),
> and multi-surface flows record AND replay
> ([authoring.md](authoring.md#multi-surface-flows-apps-and-in-blocks)):
> `apps:` + `in:` blocks, one surface active at a time, captures crossing
> blocks, per-step surface attribution, replay from the trace alone with
> zero LLM calls. Still open from Phase 2: per-surface `window:`/`browser:`
> config, surface-aware `assert_screenshot` baselines, and `heal` on
> multi-surface flows (re-record instead). Phase 3 remains a proposal.

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
      # ...
      - Remember the "id:wnd[0]/sbar" matching /\d+/ as order
  - in: portal
    steps:
      - Type ${captured.order} into the "Search" field
      - assert: page shows ${captured.order}
```

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
- `flowproof heal` must know which surface a failed step belonged to (the
  selector ladder itself is per-adapter already and needs no change).
- `assert_screenshot` baselines need a surface in their identity, or a
  `gui` baseline could be compared against a `portal` frame.

Delivery is several small PRs (the ratchets refuse large ones): spec
parsing and refusals first, trace format with schema and docs second, the
record path third, the replay path fourth — each with its tests.

## Phase 3 — agent segments in a multi-surface flow (deliberately deferred)

`app: agent` records a different boundary: a model cassette, not UI steps.
Embedding an agent block inside a UI flow means composing two trace kinds
in one file while keeping "replay makes zero LLM calls" true across the
seam. Until Phase 2 has settled, an agent flow chains as a *consumer* via
Phase 1 — its spec references `${ORDER_NO}` like any other flow. What an
agent flow could itself export (a conclusion, a tool result) is part of
this phase's design, not Phase 1's.

The arbitrary-Windows-app case needs no phase of its own: `app:
{command, window_title}` is one more entry in the Phase 2 `apps:` map.
