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
- Artifact store layout and retention (`.flowproof/artifacts/`).
- Heal-diff UX: trace-line diff vs. side-by-side screenshot review.
