# flowproof design

> Status: skeleton. The full design doc will be pasted in here; the sections
> below capture the decisions already fixed so the scaffold has a home for
> them.

## Core principle

**AI authors, deterministic engine executes.** A computer-use agent performs a
flow once from a natural-language YAML spec and records a trace; the trace
compiles to a deterministic script replayed in CI with **zero LLM calls**.
Self-healing on failure proposes a reviewable diff — never a silent mutation.

## Architecture

- **Rust driver** (`flowproof-driver`): DXGI capture, SendInput, UIA client.
  Native adapters over pixels wherever possible: SAP GUI Scripting COM,
  WebDriver/CDP, Java Access Bridge later (`flowproof-adapters`).
- **Perception**: scene graph = UIA tree + screenshot + OCR + local grounding
  model. Citrix/RDP mode is vision-only.
- **Selector ladder** per step (deterministic first): 1) native ID
  2) structural 3) OCR/text anchor + spatial relation 4) visual template
  5) AI relocation. See `docs/trace-format.md`.
- **Authoring backends** (`flowproof-agent`): pluggable and explicit. Plain
  scalar steps are model-grounded human intent; `rules:` and global
  `--author rules` select the deterministic grammar. The driver describes
  the live scene graph (interactable elements with real selectors), the model must choose its
  target FROM that list — it cannot invent selectors — and the chosen action
  is performed and verified like any other before being recorded. Backends:
  Anthropic Messages API and OpenAI's API, configured via `flowproof config ai`
  or `FLOWPROOF_AI_PROVIDER`, `FLOWPROOF_AI_API_KEY` (falls back to
  `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`), and `FLOWPROOF_AI_MODEL` as an
  advanced override. Custom OpenAI-compatible endpoints remain available via
  `FLOWPROOF_AI_PROVIDER=openai-compatible` plus `FLOWPROOF_AI_BASE_URL`.
  Scene-graph grounding is deliberate: it keeps
  authored traces selector-based and replayable; screenshot/vision
  observation joins later (required for Citrix mode).
- **Assertions**: element state, OCR, visual diff, out-of-band SQL/API.
- **SDKs**: Python-first (`sdk/python`, later PyO3/maturin bindings to the
  engine); YAML specs with natural-language steps.

## Relationship to DataMaker

flowproof is a sibling of DataMaker, not a component of it.

- **No opencode dependency.** DataMaker's agent runtime wraps
  `@opencode-ai/sdk` (`apps/datamaker-opencode`) — the right harness for a
  chat/codegen agent. flowproof's recording agent is a *computer-use* loop
  (screenshot → ground → act via driver → record step); it talks to model
  APIs directly through its pluggable backends and stays a single Rust
  binary with no Node sidecar.
- **MCP surface** (shipped: `flowproof-mcp` in the Python SDK, `pip
  install flowproof[mcp]`): `record` / `run` / `get_trace` / `heal` as MCP
  tools, following the `datamaker-mcp` / `datamaker-api` MCP patterns.
  This is the integration path by which DataMaker's agent — or any coding
  agent — drives flowproof. Large tool results (screenshots, traces)
  should follow the spill-to-object-storage + presigned URL + summary
  idiom used by datamaker-mcp.
- **Outside-in self-help** (see [self-help.md](self-help.md)): when a
  step is too ambiguous to author ("make required field changes"), the
  record tools return a structured clarification payload — the stuck step
  plus the live screen's field inventory — and the *driving* agent
  resolves it: query the system of record (e.g. the DataMaker CLI against
  SAP) for the domain answer, rewrite the step into concrete grammar,
  re-record. Externally-minted test data flows in through `suite.yaml`'s
  `env_from` → `${VAR}`. flowproof deliberately has no in-loop tool use;
  ambiguity resolution belongs to the agent with the richer context.
- **Shared philosophy with `datamaker-sap-cli`'s AI inference:** static,
  deterministic resolution first; call a model only when genuinely
  ambiguous. In flowproof this is the selector ladder; healing is the only
  place a model re-enters after recording, and it always outputs a diff for
  human review.
- **Possible future reuse:** DataMaker's eval-harness pattern
  (`packages/evals`) for grading the AI author, and the
  YAML-spec-drives-artifacts pattern (`packages/content`) for docs generated
  from flow specs.

## Trace format

See [`trace-format.md`](trace-format.md) and the JSON Schema in
`crates/flowproof-trace/schema/trace-v1.schema.json`.

## Open questions

- Grounding model choice and packaging for the local perception stack.
- Heal-diff UX: trace-line diff vs. side-by-side screenshot review.

### Answered

- **Artifact store layout and retention.** Each `flowproof run` writes one
  structured record at `.flowproof/runs/<run-id>/report.json`, beside the
  merged `junit.xml`; large blobs (screenshots, GIFs) stay in the
  content-addressed `.flowproof/artifacts/<sha256>` store the record points at.
  A `run-id` leads with a filesystem-safe RFC3339 stamp so a plain sort is
  chronological. Retention keeps the most recent 10 records per suite, pruning
  older ones after each run (logged, never silent); `.flowproof/` is gitignored.
  `flowproof audit` reads the latest record, and `audit --since <run-id>` diffs
  two of them by `control.id`.

## Agent-boundary testing

Deterministic testing of AI-based systems — assert a prompt's tool-call
trajectory against a mocked model boundary, record→replay applied to
the model API instead of the UI. Full design in
[agent-testing.md](agent-testing.md).

## Design notes from the Actual migration (round 2, P2)

Three capability questions surfaced by migrating actualbudget/actual
that are worth designing deliberately rather than shipping fast. The
first two are tracked as issues; the third is a decision, recorded here.

### Computed assertions (`expect.poll`-style)

**Shipped:** the named-capture form landed; see [authoring.md](authoring.md)
("Computed assertions"). The design reasoning below is kept for context.

Playwright suites often read a value, act, then assert the NEW value
relative to the old one (`balance == old_balance - 100`). Before it, a flow
could only assert against literals or `${VAR}` refs fixed before the run.
The deterministic-replay-compatible shape is a **named capture**: a step
that reads an element's text into a run-scoped variable, plus assertion
grammar that can reference it with simple arithmetic
(`assert: the "Balance" shows ${captured.balance} - 100`). Capture and
comparison both happen at execution time on both record and replay, so
the trace stays value-free (same property the `${VAR}` secret
indirection has). What needs design care: the expression grammar's size
(keep it to `+`/`-` and numeric normalization, or it becomes a
language), and how a captured value interacts with healing.

### Table-cell addressing

**Shipped:** cells are addressed by column-header text and a row anchor;
see [authoring.md](authoring.md#scoped-targets-table-cells-and-list-items-by-identity). The shipped
locator reads `the "<column>" column of the row containing "<anchor>"`
(the sketch below said `of the "<row>" row`). The design reasoning is kept
for context.

"The cell in column X of the row containing Y is empty" — row/column
coordinates, not flat text anchors. The scene() inventory needed
table structure (headers + row anchors), the grammar a
locator suffix, and the
selector ladder a structural tier that survives column reordering.
Worth doing as one coherent piece; half of it (row-anchored text) was
already expressible via `nth` ordinals, which was the workaround before.

### `page.evaluate` escape hatch: rejected

A free-form JavaScript step will not be added. It would puncture every
invariant the engine is built on: the trace stops being reviewable
(arbitrary code instead of declarative steps), replay stops being
deterministic (script results feed back into control flow), redaction
cannot see what the script touches, and healing cannot reason about it.
Every concrete case the migration hit has a first-class answer instead:
seeding state → `session:`; network shaping → `mock:`; environment
shaping → `browser:`; reading values → assertions (and, when designed,
named captures above). If a flow genuinely needs custom code, that code
belongs in the app under test or in a suite hook (`before_each`), where
it is visible, versioned, and outside the deterministic replay path.

### Generic `style <prop>` / `has css` assertions: rejected

By the same principle, the `style` assertion is a CLOSED allowlist -
`color`, `background-color`, `text-transform` - not a generic
`getComputedStyle` reader, and there is no `has css` form. A generic
computed-style assertion invites the used-value flakiness a screenshot
diff already handles better: computed geometry (`width`, `height`, `top`)
resolves to px values that shift with the viewport, fonts, and zoom, so an
equality on them is a test that fails for reasons unrelated to intent.
Geometry belongs in `assert_screenshot` (pixel-exact against a pinned
viewport), visibility in `is visible`, and layout regressions in a visual
baseline. The allowlist is the small set of computed values that read like
semantic state (is this amount red? is this heading uppercased?) rather
than like layout arithmetic. A property outside it is a parse error that
names the allowed set and points at those alternatives.

### Drag-and-drop: deferred, not rejected

`Drag "<source>" onto "<target>"` is a real gap with unmet prerequisites, not
a puncture like `page.evaluate`. Two house rules block the naive form. First,
there is no single honest MECHANISM: native HTML5 drag-and-drop responds to
synthetic `dragstart`/`drop` events, mouse-based libraries (SortableJS,
react-dnd's HTML5 backend) listen to `mousedown`/`mousemove`/`mouseup` with
movement thresholds and rAF timing, and CDP's own drag interception is the
historically flaky path in headless Chrome. Whichever family an implementation
picks, it silently no-ops on the others - a release-without-effect false green,
the exact failure the eval-rejection philosophy above exists to name and
refuse. Second, there is no intrinsic VERIFY: a drop's effect is app-defined (a
DOM reorder, a data mutation that re-renders identically, a file drop with no
visible move), so nothing app-independent proves the drag was not a no-op, and
"events dispatched" is not a verification (`Check` and `Scroll` both verify the
state actually took).

What would have to exist first: (1) the grammar `Drag [the [2nd ]]"<source>"
onto [the [2nd ]]"<target>"` is a COMPILE error unless the following step
asserts the drop's outcome, so a silent no-op turns red at the assert instead
of green; (2) one mechanism proven deterministic in headless CI against all
three families (a trusted CDP input stream with drag interception, Playwright's
approach), or a per-flow declared `browser: dnd: html5|mouse` if that flakes,
never a guess; (3) same-frame only at first, since cross-frame drag is where
CDP interception breaks. Until those hold, a Drag step would be worse than none.

**Measured, 2026-08-01.** (1) and (3) were implemented and hold: a `Drag` not
followed by an assertion is a compile error naming the reason, and the
same-frame limit costs nothing today. **(2) does not hold**, and the numbers
are the point:

- A **mouse** dispatch - press, interpolated moves, release - against a real
  jQuery UI `sortable` landed the drop **2 runs in 5**. Adding a priming move
  clear of the distance threshold and pacing the moves slower than one CDP
  round trip took it to **4 in 8**. The failures are real drops that do not
  land, not assertion timing: the row simply stays where it was.
- The **HTML5** path could not be measured against a live page at all, so its
  determinism is unknown rather than good.
- **CDP drag interception is unavailable**: `headless_chrome` 1.0.22 exposes
  neither `Input.dispatchDragEvent` nor `Input.setInterceptDrags`, so
  Playwright's approach - the one this section names - cannot be reached
  without hand-rolled raw CDP methods.

The `browser: dnd:` escape hatch does not rescue it. That exists for when
DETECTION picks the wrong family; detection was correct here (it chose
`mouse` from the source's markup and recorded it). The flakiness is one layer
below, in the dispatch.

**Corrected, 2026-08-02.** The sentence that used to close this section named
`Input.dispatchDragEvent` as the way out. It is not, for the case in front of
us. That API synthesises HTML5 drag events, and the corpus's one drag obstacle
uses jQuery UI `draggable` with `connectToSortable` - the MOUSE family, bound
on `mousedown`/`mousemove`/`mouseup`. Its rows carry no `draggable="true"`, so
native drag-and-drop is not involved at any point. Adding the missing CDP
methods by hand would leave the measured failure exactly where it is.

That matters because the doc was pointing future work at the wrong layer: the
route it named would have been implemented, would have worked, and would still
not have moved the number.

**Resolved, 2026-08-02: 20/20.** It was not the shape of the sequence either.
Two defects in the dispatch, both structural, and neither about pacing:

1. **The two midpoints were read in different layouts.** Scrolling the target
   into view after computing the source's point moves the source out from
   under the press about to be dispatched at it, so the drag begins on
   whatever now occupies those coordinates. On a page where both fit on
   screen this is invisible; on one where they do not, it is most of the
   failure.
2. **The intermediate moves named no held button.** A mouse-family library
   reads a move whose `which` is 0 as the button having come up and abandons
   the drag. CDP reports none held unless told to, so every move looked like
   a release. Measured in isolation against the fixture: with the held button
   10/10, without it 0/10.

With both fixed the drag landed **20 times in 20** against the live jQuery UI
`sortable`, and 10/10 against the committed fixture on every run since.

The lesson is the one the correction above already paid for once: this section
twice named a cause - first a missing API, then the pacing - and was twice
wrong, because neither had been isolated by measurement. The number moved when
the dispatch was instrumented rather than theorised about.
