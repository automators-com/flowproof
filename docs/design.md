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
- **Authoring backends** (`flowproof-agent`): pluggable, rules-first. The
  deterministic rules resolver handles known vocabularies; the LLM author
  handles arbitrary steps: the driver describes the live scene graph
  (interactable elements with real selectors), the model must choose its
  target FROM that list — it cannot invent selectors — and the chosen action
  is performed and verified like any other before being recorded. Backends:
  Anthropic Messages API and any OpenAI-compatible endpoint (e.g. vLLM),
  configured via `FLOWPROOF_AI_PROVIDER`, `FLOWPROOF_AI_BASE_URL`,
  `FLOWPROOF_AI_API_KEY` (falls back to `ANTHROPIC_API_KEY`/`OPENAI_API_KEY`),
  `FLOWPROOF_AI_MODEL` (mirrors the `AI_PROVIDER`-style convention used
  across Automators products). Scene-graph grounding is deliberate: it keeps
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
