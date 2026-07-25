# Changelog

All notable changes to flowproof are recorded here. Versions follow the
workspace version (Rust crates, the Python wheel, and the npm package move
together).

## 0.6.0

### Added

- **Web action and assertion grammar**, each one a gap found by migrating a
  real OSS suite rather than picked from a list:
  - `Double-click [the [2nd ]]"<text>"` and `Hover over [the [2nd ]]"<text>"`.
    Hover verifies the element actually matches `:hover` after the move, so a
    move onto an occluded element fails instead of passing.
  - **Native browser dialogs** (`alert`/`confirm`/`prompt`) handled by a
    suffix on the action that triggers them, since a JS dialog blocks
    synchronously and cannot be a step of its own.
  - `page title is|contains <title>` - the url pair's missing sibling. Web
    only, and auto-waiting, because an SPA sets `document.title` after the
    route commits.
  - **iframe-scoped assertions**: `the "<inner>" in the iframe "<frame>"`,
    same-origin only. The frame is a FENCE, not a hint: a miss inside it
    never falls back to a same-named element on the page outside, a
    cross-origin frame ERRORS rather than reading as absent, and an ACTION
    inside a frame is a parse error, because it would resolve against the
    main document and could pass without acting.
  - **Cookie security controls**: `cookie "<name>" exists | is httpOnly |
    is secure | is persistent`. A cookie's VALUE cannot be asserted, by
    design: it is a credential, so no value can reach a trace or a failure
    message. `is secure` passes over plain http but warns, because browsers
    exempt localhost and the run would otherwise certify nothing.
- **`assert_api` response assertions**: `body_json <dotted.path>` with
  `equals`, response `header` assertions, and array `count` /
  `count_at_least`.
- **Agent tool-boundary warning.** A flow that mocks or forbids a tool
  nothing intercepts is told so at runtime, at record AND at replay: the
  model boundary does not stop a tool executing, and replay re-serves the
  recorded tool calls to a live agent on every run.

### Fixed

- **`session:` seeding no longer overwrites what a flow changed.** The
  seed script is dropped once the first document has run it, instead of
  guarding itself with a `sessionStorage` sentinel. The sentinel was scoped
  per ORIGIN, so any navigation crossing one (a login host to an app host)
  re-seeded and silently reset the flow's own mutations.
- **The run record states the containment tier it actually ran under.** A
  flow declaring `allow_egress` runs uncontained off Linux and still passes;
  the record now distinguishes that from a genuinely certified run, and
  blocked-destination evidence is only claimed when this run was contained.
- **npm packaging.** The platform binaries are published under the
  `@automators` scope: the unscoped names were rejected by npm's
  spam heuristic, which is why npm sat at a 0.0.1 placeholder while PyPI
  shipped. A `versions agree` CI job now checks every version location on
  every PR, so the registries cannot drift again.
- Assertions that own their waiting (`checkbox is checked`,
  `shows ${captured.x}`) wait for a late-rendering target instead of failing
  on the up-front probe, and an absent target is told apart from a
  wrong-kind one.
- `assert_api` sends a failing write once instead of re-firing it.

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

### Added

- **`assert_api` counts array elements: `count` and `count_at_least`.** Pair
  either with `body_json` to assert how many elements are in the collection at
  that path (`body_json: results` + `count: 5`, or `count_at_least: 2` for a
  minimum). Previously the only way to ask "how many rows came back" was to
  assert that some index exists (`results.1.id`), which cannot express
  "exactly N" and forces you to name a leaf key that element happens to carry;
  11 of ~30 assertions in a migrated real-world API suite are of this shape.
  A non-array at the path fails naming the actual kind, and a wrong count
  reports both found and wanted.

### Changed

- **Breaking: a failing `assert_api` no longer re-sends a write.** Auto-wait
  polls a failing probe until its bound expires, which is correct for a read
  and dangerous for a write: the probe IS the mutation, so a failing `POST`
  was delivered once per tick (41 deliveries measured against a counting
  server inside the default 10s bound), and only ever when a test FAILED.
  `GET`, `HEAD` and `assert_sql` still poll; `POST`, `PUT`, `PATCH` and
  `DELETE` are sent exactly once and their failure names the opt-in. A flow
  that relied on polling a write now fails loudly instead of silently
  duplicating writes: add `retry: true` to the step to restore it (or
  `retry: false` to send a read once). On older releases, `timeout_seconds: 0`
  is the mitigation.

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
- `the "<target>" checkbox is [not] checked` and `the "<target>" shows
  ${captured.<name>}` now wait for a target that renders late, like every other
  targeted assertion. Both were missing from the assertions-do-their-own-waiting
  gate, so a single non-waiting probe failed the record with ElementNotFound
  before the assertion's own poll loop could run. The `--reuse` drift check had
  the same omission (a late target forced a spurious re-author).
- `session:` localStorage seeding runs once, on the flow's first document,
  instead of on every document: the init script (CDP re-runs it on each
  navigation) is now DROPPED once that document has run it, so fixture state
  a flow mutates through the UI (an item added to a seeded cart) survives
  mid-flow navigation and reload instead of being silently reset to the
  fixture. This holds across a navigation that changes origin too: the first
  cut of this fix guarded on a sessionStorage sentinel, which is per origin,
  so a cross-origin navigation could not see it and re-seeded over the
  mutation.

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
