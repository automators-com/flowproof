# Changelog

All notable changes to flowproof are recorded here. Versions follow the
workspace version (Rust crates, the Python wheel, and the npm package move
together).

## 0.5.0

### Added

- **Security controls: deterministic security regression.** Assert that a
  security control still holds on every replay, with recorded evidence.
  - `assert_no_secret_leak: ${VAR}` certifies a named secret never appears in a
    run's observable output: the agent model-boundary trajectory, a web flow's
    surface text captured at each step boundary, or an `assert_api` response
    body. Scanned identically at record and replay; only the variable NAME
    travels, never the value; a leak at record fails the run and mints no trace
    (a store-guard for the trace's own cassette). A flow kind with no readable
    corpus fails as a capability error, never a vacuous pass.
  - `control:` block names a flow's security control with a stable id, so a
    suite becomes a control-coverage map over time; per-suite id uniqueness is
    enforced at load.
  - Access-control regression as a composed pattern: perform the attempt as a
    declared identity, assert the denial (a 403, a UI block), and prove the
    identity was alive in the same run so a dead credential cannot read as a
    passing control.
  - Shared `identities:` in a suite (`session: <name>`), declared once and
    dereferenced into each flow at load so the trace stays self-contained.
- **`flowproof audit`.** Renders a control-coverage map (YAML or `--json`) from
  a persisted run record (`.flowproof/runs/<id>/report.json`) that
  `flowproof run` writes, with no re-replay. `--since <run-id>` diffs two runs
  and reports added, removed, and verdict-changed controls, exiting non-zero on
  a regression (a removed control, or one that turned failing).
- **Hover a web element.** `Hover over "<text>"` (plus the `the`, ordinal, and
  `in the item containing "<anchor>"` forms) moves the pointer onto the element
  with a single `mouseMoved`, no press/release. The step then self-checks that
  the element actually matches `:hover` (the hit test landed on it or a
  descendant), so a move that hit an occluder fails instead of passing. Hover
  state persists until the next explicit pointer action, so a following `Click`
  can hit a hover-revealed element. Web only. Additive trace v1 change: a new
  `hover` entry in the action-type enum of `trace-v1.schema.json`; traces not
  using it are byte-identical.

See [docs/authoring.md](docs/authoring.md#security-controls) and
[examples/access-control/](examples/access-control/).

### Changed

- **Text anchors now match button-type inputs by their `value` attribute.**
  `<input type="submit|button|reset" value="Login">` is a void element whose
  accessible name is its `value` (HTML-AAM), so every rung of the text-anchor
  XPath ladder now also matches these three input types by `@value` with the
  rung's own comparison (exact, prefix, case-insensitive). This is a minor
  matching-semantics change that affects replay-time resolution of EXISTING
  text anchors: the ladder is re-evaluated at replay, so a page where a legacy
  element and a button-type input share an accessible name at the same rung may
  now resolve differently, matching what Playwright and WebdriverIO consider
  the correct element. Only those three types: text-like inputs hold user data
  in `value`, not a name, and are still never matched by it.

### Fixed

- `the "<target>" appears 0 times` no longer fails recording with
  ElementNotFound before the count runs: AssertCount now sits in the
  assertions-do-their-own-waiting gate, so asserting absence passes when zero
  elements match and nonzero counts auto-wait like every other assertion.
- Role nouns compose with the state assert tails: `the "Username" field is
  visible` (and `button`/`link`/`dropdown`/`checkbox` before `is [not]
  visible`, `is enabled|disabled`, `is [not] empty`) now resolves exactly like
  the noun-less form. The noun is dropped, not enforced; `checkbox is [not]
  checked` keeps its required noun.

## 0.4.1

### Fixed

- egress containment deadlocked every command-agent flow on Linux (the
  notify-fd handoff used a syscall the filter traps); containment is now opt-in
  (only flows using allow_egress/assert_no_egress) and the handoff no longer
  deadlocks.

## 0.4.0 (2026-07-24)

### Added

- **Agent-boundary testing (`app: agent`).** Deterministic record/replay of an
  agent's model-call trajectory against a mocked model boundary. OpenAI-style
  and Anthropic Messages backends, streaming synthesized symmetrically at record
  and replay, and http-target agents (`agent.url` + `proxy_port`) alongside
  `command:` agents. Assertions: `assert_tool_call` / `assert_no_tool_call` with
  `where` matchers, and reply-content checks. See
  [docs/agent-testing.md](docs/agent-testing.md).
- **MCP tool-boundary testing.** The agent's Model Context Protocol traffic is
  recorded and replayed as additive trace lanes: stdio servers, streamable-HTTP
  servers, and server notifications over the GET SSE stream. A mocked tool result
  is answered locally and never forwarded.
- **`flowproof capture`.** A byte-fidelity HTTP capture endpoint for inspecting
  exactly what a tool under test sends. See [docs/capture.md](docs/capture.md).
- **Web grammar additions.** Attribute assertions (`attribute X is Y`),
  computed-style assertions over a closed property allowlist, a `Scroll` action,
  and scoped-container targets (`the "X" in the item containing "Y"`).
- **Egress containment for `app: agent` (Linux).** A `command:` agent flow
  can now declare the network it is allowed to reach and certify that it
  reached nothing else:
  - `agent.allow_egress`: a list of allowed destinations (`host:port`,
    `ip:port`, `cidr:port`, or a bare `host`/`ip` for any port). `${VAR}`
    references resolve at execution and are stored unresolved. Loopback is
    exempt wholesale, so the model proxy and local MCP servers need not be
    listed.
  - `assert_no_egress`: a step that certifies the set of undeclared
    destinations the agent attempted is empty. It is a capability claim - on
    any platform or driver where containment is not enforced it fails
    ("cannot certify") rather than passing vacuously.
  - On Linux, enforcement is a real, unprivileged, default-deny seccomp
    user-notification filter with a parent supervisor, live in both record
    and replay so the phases share a denial environment. The supervisor
    performs allowed connections itself over a `pidfd_getfd` dup of the
    child's socket and never uses `SECCOMP_USER_NOTIF_FLAG_CONTINUE` for
    address-bearing syscalls, closing the check-then-reuse race.
  - Every agent run prints its containment tier (enforced / not contained,
    with the reason) on every platform. macOS, Windows, `url:` services, and
    kernels older than 5.6 are reported "not contained".
  - The trace gains an additive egress audit lane (containment tier, the
    unresolved allow-list, and any denied attempts). A flow that does not use
    the feature serializes byte-identical to before.

See [docs/agent-testing.md](docs/agent-testing.md) for the grammar, the
per-platform honesty table, and the v1 limitations.

### Fixed

- **Test stability.** The agent-boundary end-to-end tests each mutated the
  process-global `FLOWPROOF_AGENT_UPSTREAM`; under parallel `cargo test` that
  raced and could flake or hang CI. They are now serialized so a run is
  deterministic.
- **npm publish pipeline.** The multi-platform publish workflow is idempotent
  and fails open, so a partially-published release can be re-run safely.
