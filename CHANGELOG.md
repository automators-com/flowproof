# Changelog

All notable changes to flowproof are recorded here. Versions follow the
workspace version (Rust crates, the Python wheel, and the npm package move
together).

## Unreleased

### Changed

- **A version tag now publishes.** Both publish workflows were
  `workflow_dispatch` only, so pushing `v0.15.0` did nothing and the release
  still had to be triggered by hand — a step easy to forget after tagging, and
  easy to mistake for having released.

  They now also fire on a tag matching `v[0-9]+.[0-9]+.[0-9]+`. The pattern is
  exact rather than `v*` so an experimental tag cannot ship a release, and a
  new guard refuses a tag whose name disagrees with the version it would
  build. That guard is not hypothetical: a `v0.14.0` tag was created on the
  commit that bumps to 0.15.0. While nothing consumed tags it was merely wrong
  in the history; with tags publishing, the same slip would ship 0.15.0
  labelled 0.14.0, and registry versions are immutable.

### Fixed

- **Recording refused a click replay would have waited for.** The occlusion
  probe's own comment said it used "the same predicate as replay's
  actionability gate, so the two agree by construction". The predicate was the
  same. The waiting was not: replay polls the gate to the step's deadline,
  recording asked once and aborted. A toast on its way out, a modal backdrop
  mid-fade, a carousel between slides — each reports "something else would
  receive this click" the instant it is asked, and each is gone a moment
  later. Recording refused pages it could have recorded, and the trace it
  declined to write would have replayed.

  Recording now polls the same gate for the same budget, measured as wall
  clock exactly as replay measures it — a count of polls would omit the probe
  round-trips between them and let recording out-wait replay on a slow
  transport. Not a new constant:
  recording bakes `step_timeout_ms()` into every targeted step's existence
  precondition and replay waits exactly that recorded value, so both are
  derived from the one number and follow its `FLOWPROOF_STEP_TIMEOUT_MS`
  override together. The direction is the part that matters — recording must
  never accept a click replay would reject, so out-waiting replay is the one
  error that cannot be tolerated: it mints a trace whose first replay fails.

  An occluder that outlasts the budget is still refused, with the message it
  always had. This buys the transient cases only, which is all a wait can buy.

- **The two E2E jobs had been red on `main` for a day, and nothing on a pull
  request could see it.** `web E2E` and `windows build + E2E` are off the PR
  path, so they run only on push to main — every PR involved went green, and
  main went red on merge, repeatedly, for two unrelated reasons.

  `notepad_author_e2e` builds a `FlowSpec` literally and the struct had grown
  three fields it did not list (`apps`, `exports`, `login`). The file is
  `#![cfg(windows)]`, so no amount of `cargo clippy --all-targets` on Linux or
  macOS ever compiled it.

  `web_e2e` asserted the whole-run GIF renders, but GIF assembly became opt-in
  when `--video` landed and the test kept running with defaults — it expected
  an artifact nothing had been told to produce. It now asks for one, so the
  assertion tests what it claims to.

## 0.15.0

A maintenance release with one theme: the things that were true in writing but
not in the code.

A SAP flow's `login:` block, which 0.14.0 shipped, was documented alongside
examples the grammar refuses — `Remember the "…" matching /\d+/ as order`
appeared in two more places while the same document listed that exact form
under "Refused on purpose". The examples are fixed, and a test now reads the
prose pages so the next drift fails a test rather than a reader's first
recording. `documented_grammar_examples_all_resolve` was a hand-kept list of
strings; nothing had ever read the docs.

Two advisories left the tree: PyO3 to 0.29, and `cryptography` to 50.0.0 in the
Python SDK's lock file. Neither reached a user — the second is dev-only — but
the lock file is what every contributor and every CI run installs.

And a keystroke stopped costing 213 milliseconds.

### Changed

- **A keystroke cost 213 milliseconds, and almost none of it was work.** The
  CDP transport gives the socket reader thread and every sender one shared
  mutex, and the reader holds it across a blocking read — so a send did not
  race the reader, it waited out a read already in progress. Profiling a form
  flow found the reader holding that lock for 94% of the run's wall-clock,
  sends waiting a mean of 10.7ms each, and the writes themselves costing
  0.07ms. Typing is two calls per character, so a form paid for it by the
  letter: 38.8 seconds for a seven-step, 78-character flow.

  A second cost sat on top. Responses were awaited by polling a channel every
  5ms rather than blocking on it, and every tab-level call paid that poll
  twice — once for the `Target.sendMessageToTarget` ack, once for the real
  response. The ack wait was 5.25ms while the response it was waiting behind
  had already arrived, at 0.03ms.

  The workspace now pins a patched `headless_chrome`: the reader's read
  timeout drops from 100ms to 1ms, and responses block on the channel instead
  of polling it. The same flow runs in about 5 seconds, typing at roughly
  20ms per character and a click at 0.2s. A git patch is viable because
  nothing here goes to crates.io — the wheel and the npm package are built
  from this workspace.

  This is a mitigation and the code says so. Reads and writes still share one
  lock, so a send still queues behind a read; the constant only bounds how
  long. The fix is to split the transport's halves, which belongs upstream.

### Fixed

- **The fix for "a step naming a whole form authored one field" could make it
  author none.** That fix let one step answer with a sequence, and grounded the
  sequence as a unit: if any single action failed, all of them were refused, so
  a half-filled form could never reach the trace. That part was right and
  stands. What was wrong was the correction. A refused sequence went back to
  the model as the original question with the failure appended and a closing
  instruction to reply with "ONLY the corrected JSON object" — singular. A
  model that complies returns one action. So a step meaning a whole form was
  answered with a sequence, refused over one bad target, and then re-answered
  with a single field: fewer fields than the one-action-per-step behaviour the
  sequence was introduced to replace, and on a form long enough, none at all.

  The odds were the mechanism. Each action grounds or does not roughly
  independently, so a one-action step landed at some rate and a twelve-action
  step landed at that rate to the twelfth — and the one retry had to win the
  same throw again, from scratch, having been told to come back with less. The
  longer the form, the likelier the step authored nothing, which is why this
  showed up on forms rather than on clicks.

  The retry is now a correction rather than a fresh question: it names which
  action was refused and why, states how many actions before it had already
  grounded and asks for those back unchanged, and asks for an array. Nothing
  about what is *accepted* moved — a sequence still lands whole or not at all.

- **A high-severity advisory sat in the Python SDK's lock file.**
  `cryptography` was pinned at 49.0.0, which GHSA-g6cj-pr64-35w5 names
  (fixed in 50.0.0). It arrives transitively — `mcp` pulls `pyjwt[crypto]`
  pulls `cryptography` — and `mcp` is in the `dev` dependency-group, so it
  never reached the published wheel and no user was exposed. A locked
  vulnerable version is still a locked vulnerable version: it is what every
  contributor and every CI run installs, and "not shipped" is a reason to fix
  it calmly rather than a reason to leave it. Now 50.0.0; ruff and the 25
  pytest tests pass unchanged.

- **The docs still demonstrated a step the grammar refuses, in two more
  places.** An earlier pass fixed the `exports:` walkthrough and one
  multi-surface example; it missed the same
  `Remember the "…" matching /\d+/ as order` in authoring.md's Phase 2 block
  and in multi-surface.md's agent-seam block, because the fix was by hand and
  by hand is how the first two got there. A reader following either one got a
  refusal from the tool that had just taught them the step.

  Both now read the number from the field that holds it. More to the point,
  a test reads the prose pages now: every step in every fenced yaml block of
  authoring.md, multi-surface.md and getting-started.md is checked against
  the set of shapes the grammar recognises in order to reject. It asserts
  only that — not that each step resolves — because a plain step is
  natural-language intent for the model author by design, and demanding it
  parse deterministically would fail on examples that are perfectly correct.
  The extraction also fails if it stops finding steps, so it cannot quietly
  degrade into a test that passes by reading nothing.

- **The published wheel carried two known PyO3 advisories.** `flowproof-python`
  was pinned to PyO3 0.26, which RUSTSEC-2026-0176 (out-of-bounds read in
  `nth` / `nth_back` for `PyList` and `PyTuple` iterators) and RUSTSEC-2026-0177
  (missing `Sync` bound on `PyCFunction::new_closure` closures) both name, and
  both are fixed only in 0.29. This is the one dependency in the workspace that
  ships to users rather than staying in a build: the extension module is the
  wheel.

  Now on 0.29, which required no source change - the binding was already written
  against the `Bound` API the advisories left alone. `cargo audit` reports no
  vulnerabilities. The `abi3-py39` floor is unchanged, so the wheel still
  targets Python 3.9 and older interpreters keep working.

  Two unmaintained-crate warnings remain, `paste` (RUSTSEC-2024-0436) and
  `ttf-parser` (RUSTSEC-2026-0192). Both arrive through `imageproc`, a
  dev-dependency of `flowproof-cli`, so neither is in a shipped artifact and
  neither is a vulnerability.

## 0.14.0

This release is about the test case that does not fit in one flow. Real work
crosses seams — SAP GUI creates the order, the web portal must show it — and
until now the only bridge between two flows was a hand-written script, exactly
the glue flowproof exists to replace. A flow can now declare `exports:`, handing
named values from its own captures to the flows after it, resolved at replay
time from replay-time captures.

The seam *inside* one flow closed in the same release. `apps:` declares named
surfaces, steps live in `in:` blocks, and a flow written that way records,
replays and heals: one flow file, one trace, every step routed to the surface
that ran it, and replay from the trace alone with zero LLM calls, exactly as a
single-surface replay. Each surface carries its own config — `browser:` for a
web surface, `window:` for a desktop one, vision included — and a screenshot
baseline names its surface, so a `gui` baseline can never be compared against a
`portal` frame. Single-surface traces serialize byte-for-byte as before.

SAP flows also stopped borrowing their identity from the environment. A flow
names the user it logs in as, so the clerk-then-approver case is two flows with
one `login:` block each rather than two process-global variables that cannot
both be set at once — and the password stays in the spec, with no field in the
trace to leak it.

A step also stopped being one action. "Fill out all the vehicle data" authored
a single field and moved on, because the authoring protocol accepted only one
JSON object — so the form failed its own validation with a trace that looked
authored. A step is now a unit of intent: the model may answer with a sequence,
grounded as a whole, and the scene it reasons over finally reports what a
control holds rather than only what it is.

And runs got faster where the time actually was. Every web probe walked an
element-handle path costing four to six CDP calls, which is why turning video
off never helped: a click is ~1.3s now against ~3.9s, a short type ~2.5s
against ~5.6s, headed and headless alike.

### Added

- **A SAP flow can name the user it logs in as.** Credentials came from
  `SAP_USER`/`SAP_PASSWORD`, which are process-global — so a test case that
  acts as a clerk and then as an approver could not express its second user
  at all, and a run needed environment set up outside the spec before it
  would work. A flow now takes a `login:` block (`user`, `password`,
  optional `client` and `language`), and a suite of flows with one block
  each is the two-user test case. Values may be `${VAR}` references
  resolved at every launch, or literals, so a flow can run with nothing
  configured anywhere. The password never enters the trace: it has no
  header field, is read from the spec at each launch, and the header
  carries only `login_user`, because a recording that cannot say which
  identity produced it is not reviewable. `login:` without `connection:`
  is refused — it reads like it picks an identity, but with nothing to open
  it could only attach to whatever session was already there. And a session
  logged in as somebody else is no longer taken over: flowproof opens its
  own connection and logs in beside them, since driving the wrong identity
  passes while proving nothing. Flows with no `login:` block behave exactly
  as before. An `apps:` surface entry parses and validates the same block —
  it is the shape a same-system two-user case wants — but nothing stages a
  surface's credentials yet, so `record` and `heal` refuse such a flow by
  name at launch rather than driving whatever session was already open as
  whoever opened it. Until that lands, the same case is a suite of
  single-surface flows chained with `exports:`, one `login:` each.

- **`flowproof heal` works on multi-surface flows — Phase 2's list is
  empty.** Healing was refused on the belief it needed multi-surface
  replay; it never did. Heal is re-record-plus-diff, and multi-surface
  recording already ships — so heal now runs on the same surface
  registry, proposes a trace that keeps the multi header and per-step
  surface attribution, and flags a step that MOVED between surfaces as a
  `surface` change, because the same action against another app is not
  the same step. An unchanged flow still proposes nothing.

- **`assert_screenshot` works in multi-surface flows: a baseline's
  identity names its surface.** It was refused because the identity did
  not — two blocks reusing one name would have overwritten each other's
  baseline, and a `gui` baseline could quietly stand in for a `portal`
  frame. The recorder now qualifies each baseline once, at authoring:
  stored and compared as `<name>@<surface>.png`, carried in the trace, so
  minting and replay follow with no changes of their own. Two surfaces
  may reuse one spec name; `@` in a spec-chosen name is refused, naming
  the reservation, so no user name can collide with another surface's
  baseline.

- **A desktop surface carries its own `window:` — and vision becomes a
  surface.** A Citrix/RDP-published app could not join a multi-surface
  flow: vision attaches to a window by title, and there was nowhere
  per-surface to say which. `window: {title: …}` on a vision surface is
  that attach selector (required, stored raw in the surface entry so a
  `${VAR}` resolves at every replay), and `width`/`height` (optionally
  `x`/`y`) pin any desktop surface's shape at its FIRST activation — what
  was APPLIED is recorded per surface, position included even when the
  spec never asked for one, and replay reproduces it exactly. On a web
  surface `window:` is a parse error naming `browser: viewport`; a
  lopsided geometry (width without height) is refused like the
  single-surface rule before it.

- **A web surface of a multi-surface flow carries its own `browser:`.**
  A Fiori portal surface could not pin its viewport: `browser:` was
  flow-level config, refused on multi-surface flows, so a flow recorded
  on one window shape could replay on another — the exact drift the
  single-surface header field exists to prevent. The block now sits on
  the surface entry (same shape, one level deeper), travels on the
  surface's header entry, and both factories stage it through one shared
  helper in the only window staging can land: between the driver's
  construction and the launch its first activation performs. Record and
  every replay launch that surface the same shape; on a non-web surface
  the key is a parse error naming whose config it is.

- **A multi-surface trace REPLAYS — Phase 2 closes.** Each step recorded
  which surface ran it, and replay now dispatches on exactly that: a
  surface change is an activation (lazy launch on the first visit,
  re-foreground after), performed by a registry rebuilt from the header's
  own surface map — so a replay needs the trace and nothing else, makes
  zero LLM calls, and resolves `${VAR}` urls and connections fresh from
  this run's environment. A capture minted on one surface is re-read LIVE
  and typed into another, which the round-trip test proves by replaying
  against a different order number than record saw. On a plain
  single-surface driver the first attributed step refuses by name — a
  multi trace can never quietly replay against one app. `run` and suites
  lost their refusal, and `heal` lost its own a slice later (above). The
  test case that started all this — SAP GUI
  creates the order, the portal must show it, SAP confirms — is now one
  flow file, one trace, free on every CI run.

- **A multi-surface flow RECORDS.** The recorder meets an `in:` block by
  activating its surface on the registry — lazy launch on the first
  visit, re-foreground after — and every recorded step carries the
  surface that ran it, authored against that surface's own grammar (what
  a SAP adapter performs differs from what a browser does). Captures
  cross blocks with no new machinery: the namespace was always
  flow-scoped, only the surfaces changed underneath it — an order number
  remembered on SAP GUI types into the portal as `${captured.order}`,
  and the trace stores the NAME, never the value, exactly as before. The
  header is the `multi` sentinel plus the surface map with config stored
  as written, so a `${VAR}` connection resolves fresh at every replay.
  Replay was the next slice and landed in this same release (above);
  `assert_no_secret_leak` on a multi-surface flow is still refused until
  its corpus is defined rather than scanned vacuously.

- **The driver layer can now hold several surfaces and drive exactly one.**
  A `SurfaceRegistry` stands where a single driver otherwise would,
  routing every call to the ACTIVE surface of a multi-surface flow:
  launch is lazy, a launched surface is kept alive (a return visit
  resumes the same browser, the same SAP login), and everything fails
  closed by name — a call before any activation, an undeclared surface,
  and the new `activate_surface` trait hook, whose DEFAULT refuses on
  every ordinary driver so a mis-wired run says it is single-surface
  instead of silently driving the wrong app. The recorder starts using
  this in the next slice.

- **The trace format can now say which surface a step ran on.** A
  multi-surface trace (docs/multi-surface.md) carries its named surfaces
  in an additive header `apps` map and attributes each step to one via an
  optional `surface` field — both absent on single-surface traces, which
  serialize byte-for-byte as before (a conformance test now holds that
  line). The header's `app` then carries the reserved name `multi` with
  adapter `multi`, deliberately not a copy of any one surface: an engine
  predating multi-surface fails loudly at load instead of replaying every
  step against whichever surface happened to be first.

  Writing the fixture surfaced two places the schema disagreed with the
  engine it describes: the `app` object refused `command` and `geometry`
  (so every `windows`-flow header failed validation), and the adapter
  enum was missing `api` (so an out-of-band flow's header did too). Both
  had been wrong for as long as no fixture carried one — the same lesson
  as the action-enum gap before it, and the reason the new fixture
  exercises a `command`+`geometry` surface explicitly.

- **The multi-surface vocabulary, one slice ahead of its engine.** A flow
  can now declare `apps:` — named surfaces (`gui: {app: sap}`, `portal:
  {app: web, url: …}`) — with its steps in `in: <surface>` blocks, exactly
  one surface active at a time. The vocabulary parses and validates for
  real: every wrong combination is a parse error naming the right spelling
  — a block naming an undeclared surface lists the declared ones, a bare
  step at top level says where steps live, `agent` and `api` surfaces are
  refused each with its reason, `url:`/`connection:` sit on the surface of
  their kind, and flow-level surface config says where it moved. The
  engine followed within this release, slice by slice (the entries above),
  and until each slice landed `record` and `run` refused by name and
  pointed at the shipped alternative — a suite of single-surface flows
  chained with `exports:` — rather than driving the wrong surface. The
  vocabulary first meant specs could be written and reviewed while the
  engine was built, and every refusal was the documentation of its own gap
  (docs/multi-surface.md has the plan).

- **A test case that crossed technologies had no way to carry a value
  across the seam.** A flow drives exactly one surface — that is by design,
  one driver per flow — but the real-world test case is "SAP GUI creates
  the order, the web portal must show it", and the order number exists
  nowhere except on SAP's status bar at run time. Captures are flow-scoped
  and suite `env` is fixed before any flow runs, so the only bridge was a
  hand-written script between two flowproof invocations: exactly the
  harness glue flowproof exists to replace.

  A flow can now declare `exports:` — `ENV_NAME: template`, where the
  template resolves `${captured.<name>}` from the flow's own captures once
  its last step has passed. The pairs become environment variables for the
  suite's remaining flows, which reference them as ordinary `${VAR}`s: the
  downstream trace stores only the reference, and the handoff happens at
  replay time from replay-time captures, so flow B replays against the
  value flow A's replay just read. Nothing is persisted — the trace holds
  the capture name, the report and the `[EXPORT]` line hold the export
  name, and the value lives only in the memory of the run. An export whose
  capture was never remembered fails the flow that owns the captures,
  naming what was in scope, rather than a downstream flow failing over a
  variable nobody visibly set — and a failed flow exports nothing.

### Changed

- **A web step spent most of its time asking the page questions the slow
  way.** Every probe — exists, enabled, stable, would a click reach it —
  walked an element-handle path costing four to six CDP calls, and the
  transport pays up to ~100ms per call (its sender serializes behind the
  reader's blocking socket read). A click cost ~3.9s and a short type ~5.6s
  with recording OFF — which is why turning video and keyframes off never
  made runs faster: the time was not in the artifacts. The probes now ask
  the page once: existence is one `evaluate` for css and text-anchor
  targets, and the whole actionability gate is one round trip through the
  new `AppDriver::actionability_gate`, whose default composes the three
  probes so every other driver behaves as before. Cell, scoped, and framed
  targets stay on the element-handle path, where their resolution semantics
  live. Same questions, same order, same answers: a click is ~1.3s, the
  type ~2.5s, headed and headless alike. What remains is the transport's
  per-call latency — visible in typing, two calls per character — which is
  headless_chrome's to fix. `docs/authoring.md` is re-measured to match.

- **The docs on automators.ai were a release behind, and nothing said so.**
  The workflow that republishes them skipped when its deploy-hook secret was
  missing and exited 0, so every run since the workflow was added reported
  success while publishing nothing. The secret had never been set. The visible
  result was a getting-started page still promising an automatic
  `recording.gif` after `--video` had made it opt-in — documentation that
  contradicted the tool, with a green tick next to it.

  A missing secret now fails the job with the two commands needed to fix it,
  and a rejected hook fails it too rather than printing an error body and
  exiting 0. `workflow_dispatch` was added so a human can republish without
  an empty docs commit. The deploy that does not happen no longer looks like
  the deploy that did.

- **The recording page answered a question nobody was asking.** `/docs/recording`
  opened as a design specification — status line, numbered sections, scope
  notes — so someone arriving to find out how to turn the video off met §1's
  principles instead. The controls were on the page, buried in §3 between the
  `FrameSource` abstraction and the artifact bundle layout. A reader who gave
  up before finding them concluded the flags did not exist.

  The page now opens with what a reader came for: a table of the three
  recording controls with their defaults, the CLI and SDK forms, and how to
  review what was captured. The design record follows under *Design notes*,
  unchanged in substance and still numbered, because it is worth keeping — it
  just is not the first thing a person needs.

  Three pieces of prose had also outlived their subject. The trace schema
  section was still labelled `PROPOSAL` though `recording` and `redaction`
  ship in `trace-v1.schema.json` today; the redaction layer was still
  described as something "this PR creates"; and the healing diff view was
  listed as out of scope in the same document whose §8 describes it as built.

### Fixed

- **Two documented examples showed grammar the engine refuses.** The
  `exports:` walkthrough and the multi-surface example both captured an
  order number with `Remember the "…" matching /\d+/ as order` — a regex
  form the rules grammar declines by name, so a reader following either page
  got a refusal from the tool that had just taught them the step. Both now
  read the number from the field that holds it, which is what the refusal
  tells you to do. The examples are strings in
  `documented_grammar_examples_all_resolve` now, so the next drift fails
  a test instead of a user's first recording.

- **The report attached ten screenshots when eight were captured.** Step
  time ranges are contiguous — one step ends on the millisecond the next
  begins — and the report viewer bracketed frames inclusively on both ends,
  so a frame captured on the shared boundary rendered under BOTH steps. A
  reviewer counting evidence saw captures that never happened. Each frame
  now renders under exactly one step, the earliest whose range brackets it,
  with a test that puts frames on the boundary and counts what the HTML
  shows.

- **"Fill out all the vehicle data" authored one field and moved on.** A plain
  step could only ever become ONE action, because that is what the authoring
  protocol accepted: one JSON object, one target, one verb. So a step naming a
  whole form filled its first control, the next step pressed Next, and the form
  failed its own validation — with a trace that looked authored. The tool
  answered a different question from the one asked, and said nothing about the
  difference. A person driving the same page does not stop after one field;
  neither should the step that says "all".

  A step is now a unit of intent. The model may answer with a SEQUENCE of
  actions, and the contract asks it to carry out the whole step rather than a
  first instalment. It is still one model call per step, still grounded to the
  same listed inventory — the sequence is rejected as a whole if any one action
  in it is not grounded, so a half-filled form cannot reach the trace — and the
  recorder performs and verifies each action exactly as before. The trace lists
  every action individually, so replay is no less deterministic than a flow
  spelled out field by field. A bound of sixty actions per step distinguishes a
  large form from a model that has started looping.

  The scene was the other half of it. It described what a control *was* and
  never what it *held*, and a `<select>` was represented by its text content
  truncated at eighty characters — on the Tricentis sample form, that is
  "– please select –, Audi", with the remaining fifteen makes invisible. A
  model asked to fill that dropdown could only guess, and a `<select>` refuses
  a name it does not have. Entries now carry `value`, `checked`, `required`,
  and, for a dropdown, its exact `options`. A password's value is never
  reported: the scene travels to a model.

- **Three flags shipped without ever being written down.** `run --trace`,
  `heal --trace` and `doctor --prompt` existed, worked, and appeared in
  `--help`, but no page a user reads mentioned them. That is a quiet way to
  not ship a feature: an adopter who cannot find a flag does not have it. The
  trace-path overrides now sit in `docs/getting-started.md` next to the
  convention they override — including the part that bites, that a suite run
  resolves traces by name and ignores `--trace`, so a relocated trace reads as
  a flow that was never recorded. `--prompt` is documented in
  `docs/agent-testing.md` for the case that motivates it: an agent that routes
  on the task can answer `Say hello.` without calling a model at all, and
  report a zero that says nothing about the wiring.

  A test now holds the line. `documented_flags.rs` walks the clap definition
  and fails when a visible flag or subcommand appears in no `docs/*.md` and
  not in `README.md` — mechanically, the way the trace schema is checked
  against its docs, because "remember to write the prose" has now failed
  several times. It only asks that a flag be *mentioned*; a bad explanation is
  still a human's job to catch.

## 0.13.0

This release is about what a run leaves behind, and what a person is allowed to
watch. Visual evidence used to be a fixed tax: every run captured a frame around
every step and assembled a GIF, whether or not anyone would ever open it. It is
now a choice made per run — screenshots by default because the report is built
from them, animation on request, and a sparse or silent mode for suites that
only need a verdict. That choice belongs to every caller, not just the shell:
the SDK and the MCP tools take the same controls with the same defaults, so an
agent no longer has to shell out to the CLI to make it. When a person does want
to look, `--keep-open` holds one visible browser at the final page instead of
forcing a rerun to see what happened.

SAP flows also start. A named SAP flow can now launch SAP Logon, pick the right
connection out of several, and treat a transaction SAP refused as the failure it
is rather than a step that passed.

### Added

- **The recording controls stopped at the command line.** `--recording-detail`,
  `--video` and `--highlight-cursor` reached only shell callers, so the SDK and
  the MCP tools — the surfaces agents actually drive — were stuck at full
  density with no way to ask for a GIF. An agent replaying a suite paid for
  screenshots it had no use for, and an agent that wanted an animation to hand
  a reviewer had to shell out to the CLI to get one.

  `record()` and `run()` now take `recording_detail`, `video` and
  `highlight_cursor` — on `Flow`, on the module-level functions, and on the
  `flowproof_record` / `flowproof_run` MCP tools. The defaults match the CLI's,
  so existing callers see no change, and an unknown density is rejected before
  the engine drives anything rather than after.

- **A visible browser can stay open for inspection without changing the safe
  default.** `flowproof record <spec> --keep-open` and single-flow
  `flowproof run <spec> --keep-open` imply headed Chromium, wait after the flow
  completes, and exit when its window is closed. The steered window is
  maximized and brought to the foreground before navigation begins. Ordinary
  runs still close their browser automatically. Suites, retries, and JSON mode
  reject the flag rather than turning unattended automation into a hidden wait.

### Changed

- **Every run paid for a GIF almost nobody opened.** Visual capture had one
  setting: a frame before and after every single step, assembled into
  `recording.gif` at the end. That is evidence a reviewer wants when a flow
  drifts, and dead weight on the hundred green suite runs between those
  moments — paid on the path that is supposed to be the cheap, repeatable one.

  Capture density and animation are now separate, execution-time choices on
  both `record` and `run`. Screenshot checkpoints remain the default, because
  the report's step viewer is built from them; GIF assembly is opt-in with
  `--video`. `--recording-detail low` keeps the initial state, every fifth
  completed step, and the final state; `--recording-detail off` stops capture
  entirely. `--highlight-cursor` draws a visible cursor and a bright click halo
  into the frames, rendered after redaction so it cannot expose masked pixels.
  All of it changes artifacts only: the same actions, assertions, redaction,
  and verdict execute either way.

### Fixed

- **A SAP flow could neither start SAP Logon nor reliably select its requested
  connection.** `connection:` was passed to `OpenConnection`, but only after a
  `SAPGUI` COM object already existed, so a closed SAP Logon could never reach
  that call. When SAP was open, the adapter inspected only the first connection
  and could attach to the wrong system or wait forever behind an unrelated
  half-open connection.

  A named SAP flow now starts `saplogon.exe` when needed, discovers default
  installs or `SAP_LOGON_EXE`, selects an existing matching connection or opens
  the requested one, scans all candidate sessions, and completes the standard
  login from `SAP_USER`/`SAP_PASSWORD` (plus optional client/language). Startup
  gets a practical 60-second default, and failures distinguish missing paired
  credentials, non-standard login screens, post-login dialogs, and disabled
  scripting without exposing secrets.

- **SAP could reject a transaction while Flowproof treated the navigation as
  successful.** The COM call itself succeeds when SAP reports an application
  error only in `wnd[0]/sbar`. Flowproof now promotes SAP status types `E` and
  `A` to failures with the original status text, accepts a plain transaction
  code such as `VA01` and records the deterministic `/nVA01` OK code, and uses
  portable transaction `SMEN` rather than the internal `SESSION_MANAGER` name
  in its Easy Access smoke flow.

- **Sonnet 5 could spend Flowproof's entire 1,024-token authoring allowance on
  reasoning and never reach its answer.** Anthropic returned a valid signed
  thinking block with `stop_reason: max_tokens`; Flowproof called that an
  unexpected response shape and printed the enormous opaque signature into
  the terminal. Anthropic authoring now has an 8,192-token ceiling, leaving
  room for reasoning and the small structured answer, while the
  OpenAI-compatible path keeps its existing 1,024-token bound. If a model
  still reaches the ceiling before emitting text, the error now names token
  exhaustion and the configured budget instead of misdiagnosing the response.

## 0.12.2

Flowproof now completes the natural-language authoring path for the browser
actions used throughout the obstacle-course material. Authors describe what a
person does—drag a task, click part of a control, count rows, choose several
options, work inside a frame—and the recording model grounds that intent to the
live page. The resulting trace remains deterministic and replay makes no model
calls.

Headed Chrome runs also have a clean lifecycle: one maximized private window is
owned and closed by each flow, with no oddly sized keep-alive window or empty
tabs left behind.

### Fixed

- **Watching a web flow left a blank Chrome window behind.** The shared-browser
  optimization kept a blank tab alive in Chrome's default window, then opened
  the actual flow in an isolated-context window. In headed mode the useful
  window closed with the flow and exposed the oddly-sized keep-alive window,
  sometimes with two empty tabs; repeated executions made the leftovers look
  like a leak.

  Headed runs now own one private browser process, maximize its visible window,
  and close it with the flow. Headless suites still reuse one warm browser.

- **Natural-language capture could not read an unlabelled value in a
  div-based row.** Web scene extraction only listed readable leaves with a
  globally unique id, attribute, or class. A value such as the generated order
  id in Tricentis obstacle 64161 was visible and stable relative to its `order
  id` label, but absent from the model's allowed target inventory; a model that
  correctly inferred a scoped selector was then rejected for inventing it.

  The web scene now exposes such values as grounded scoped targets: a stable
  container selector, neighbouring text anchor, and non-positional inner
  selector. Model authoring copies the opaque target token, while the trace
  persists the ordinary deterministic scoped selector and reads the value
  fresh on every replay.

- **Several ordinary human instructions still required rules syntax.** The
  authoring model could only emit a small set of direct actions, and its live
  scene omitted useful off-screen controls, table collections, same-origin
  frame contents, drag targets, and small visual targets. Instructions such as
  “drag task 1 into todo”, “click the right half”, “remember the number of
  rows”, “select these four options”, or “enter Tosca in the embedded form”
  could therefore fail even though the intended control was visible to a
  person.

  Model authoring now grounds drag, point click, count capture, single and
  multi-select, exact scrolling, frame-scoped input, and key presses directly
  from natural language. The expanded page inventory includes rendered
  off-screen elements and stable collection/frame targets; the model still
  cannot invent selectors, and replay still uses only the persisted
  deterministic trace.

## 0.12.1

Flowproof's default authoring path now treats a human sentence as intent, not
as a partly structured command. An author can write “type other into the name
field” without knowing that the application calls the element `name01`:
Flowproof gives the model the live scene, lets it ground the request to that
element, and records the resolved action deterministically. Replay still makes
no model calls.

Authors who need exact control can opt into the existing rules syntax with a
`rules:` prefix or `--author rules`. The same `auto`, `llm`, and `rules` modes
now behave consistently in the CLI, suite recording, Python SDK, MCP server,
and healing flows.

### Fixed

- **Natural-language steps could silently take the deterministic rules path.**
  Small wording changes could therefore reinterpret ordinary prose as an ID or
  selector and fail to find an element. Plain steps are now model-grounded by
  default, while rules parsing is explicit and visible.

- **The model did not have a usable view of the application to ground intent.**
  Authoring now distinguishes readable scene context from actionable targets,
  resolves labels and human descriptions to live element identities, and
  returns structured ambiguity when more than one target remains plausible.

- **Authoring mode differed between entry points.** CLI authoring, missing-suite
  recording, Python, MCP, and healing now share the same routing contract and
  report whether a step used the model, explicit rules, a fallback, or a reused
  resolution.

- **Some model expansions could produce unsafe or unverifiable actions.**
  Targetless typing and drags without an assertion are rejected, multi-action
  expansions are validated, and every accepted result is compiled into an
  ordinary deterministic trace before replay.

## 0.12.0

An application that must be asked twice, a page that faults on the way to the
goal, a list that has to be put in order — none of these could be written
down, because the grammar had no way to say "keep going until" or "only if".
`repeat:` and `when:` say it, and they expand while RECORDING: what lands in
the trace is the passes that actually ran, as ordinary steps. A recording
stays a recording, and replay still decides nothing.

`Drag` arrives on the terms it was twice refused on. It was deferred for
landing 4 drops in 8, and a mechanism that flaky is worse than none — it
teaches the reader to re-run instead of investigate. Two structural defects
were costing it, and neither was the pacing these notes twice blamed: the two
midpoints were read in different layouts, so scrolling the target into view
moved the source out from under the press, and the intermediate moves named
no held button, which a mouse-family library reads as the button having come
up. It lands 20 in 20 now, and it ships with the rule that a drop nothing
asserts is a drop nobody proved.

Underneath both is a click that reported a success it never had. Replay has
always refused to click an element another element would receive the click
for; recording did not check at all. So the click went to the occluder, the
page did nothing, and the step was written into the trace as though it had
worked — a recording that cannot replay, made by the tool whose job is to
notice exactly that. The two now run the same predicate. This makes recording
stricter, which is the point: a flow that clicked a covered element and
appeared to work was never working, it was recording.

### Fixed

- **A click that landed on an occluder was recorded as a success.** Replay
  refuses to click an element another element would receive the click for.
  Recording did not check at all, and that asymmetry is the worst way round:
  the click went to the occluder, the page did not react, and the step was
  written into the trace as though it had worked. The result is a recording
  that cannot replay - produced by the tool whose entire job is to notice
  that.

  Found on a page whose second control sits at the overflow edge of a
  fixed-height container: at some window sizes it is genuinely covered.
  `Press` reported success sixty times while the page did nothing, and the
  same element under `Hover` - which has always run the hit test - said
  plainly that it was obscured.

  Recording now runs replay's own predicate before dispatching, so the two
  agree by construction. This makes recording stricter: a flow that clicked
  a technically-covered element and appeared to work will now fail where it
  used to pass. That is the point. It was never working; it was recording.

- **A test that refused to pass vacuously failed instead, and it was
  red-lighting every non-Linux machine.**
  `a_run_that_was_not_contained_keeps_no_evidence_from_an_optimistic_probe`
  asserts that a control record carries what the RUN achieved rather than
  what the host probe optimistically reported. To stop that passing for the
  wrong reason, it required the probe and the run to disagree - by asserting
  the probe says `Enforced`.

  That is only true on Linux. Everywhere else the assertion fired and the
  test failed outright, so `cargo test --workspace` was red before anyone
  had touched anything. The instinct was right and the mechanism was wrong:
  a test that cannot be meaningful on this host should say so, not fail.

  The disagreement is what the test needs, not which side of it the machine
  is on. Off Linux the probe still says "not contained" - but for a
  DIFFERENT reason - so comparing the two rendered lines keeps the test
  honest everywhere and green everywhere it is honest.

  A gate that is red by default is one people learn to read past, which is
  the whole reason it matters that this one was.

### Added

- **`Drag [the [2nd ]]"<source>" onto [the [2nd ]]"<target>"`.** The driver
  could drag; now a flow can say so. Both ends resolve through the ordinary
  selector ladder — the drop target rides in the trace as its own ladder, not
  as a bare string, because it has to survive the same drift the source does
  — and both wait to be actionable before the press.

  **The next step must be an assertion, or the spec does not compile.** Every
  other action has something intrinsic to verify: a click's element was hit, a
  scroll's offset took, a checkbox is checked. A drop has nothing. Its effect
  is app-defined and may be a reorder, a mutation that re-renders identically,
  or nothing whatsoever, and "the events were dispatched" is not a
  verification. Left alone, a drag would record green whether or not anything
  moved. Requiring the author to say what the drop did turns a silent no-op
  red at the assert.

  A drag inside a `repeat:` or `when:` needs its assertion inside the block,
  where the drop happened — an assertion after the whole loop could not say
  which pass moved anything.

- **A drag the driver can perform, measured rather than asserted once.**
  `AppDriver::drag` presses at a source, moves across with the button held,
  and releases on a target - the MOUSE family, which is what jQuery UI,
  SortableJS and react-dnd's mouse backend listen to. Not yet reachable from
  a flow; the grammar is the next step.

  It is here because the number finally justifies it. The same drag landed 4
  drops in 8 before, which is worse than none - a flaky mechanism teaches the
  reader to re-run instead of investigate. It now lands 20 in 20 against a
  live jQuery UI sortable, and the two defects that were costing it were both
  structural: the source and target midpoints were read in different layouts,
  so scrolling the second into view moved the first out from under the press;
  and the intermediate moves named no held button, which a mouse-family
  library reads as the button having come up.

  The measurement is a test rather than a note, because "it worked when I ran
  it" is not a claim this repository accepts about a mechanism.

- **A condition could compare a reading to a literal, but not to another
  reading.** Anything that sorts, ranks or pairs values branches on the
  ORDER of two things on the page, and there was no way to write that.
  `when: the "<a>" is greater than the "<b>"` (and `is less than`) reads
  both sides and compares them **numerically**.

  Numerically, and not as text, because `"9"` beats `"10"` as a string and
  loses as a number. Both readings look plausible in a diff and only one is
  what anybody means by "greater", so a side that does not parse as a number
  fails the recording and is quoted back rather than silently compared the
  other way.

- **Some applications will not settle without being asked twice, and the
  grammar had no way to ask.** Repetition and branching were refused on the
  grounds that a trace must be a recording of what happened, not a program
  that decides what to do. That reasoning holds. What it did not follow from
  is that the loop had to be refused - only that it must not survive into
  the trace.

  `repeat:` (with `until:` and a required `max:`) and `when:` are blocks,
  expanded while RECORDING. The condition is read against the live app, and
  what lands in the trace is the passes that actually ran: concrete steps,
  no loop and no branch. Replay reads a flat list and still decides nothing.
  `foreach` already made this bargain at parse time, which it can only do
  because its list is known before anything runs.

  If the condition never holds, recording fails and names the bound rather
  than pressing forever. The refusals stay, and now name the block that does
  the job - a refusal whose alternative is real is the one worth keeping.

- **A contained run could delete your files and the report would not mention
  it.** `assert_no_egress` can say an agent did not *send* the thing it must
  not; nothing said anything about *delete*. A run that unlinked the customer
  and renamed an export over the top of it finished green and silent —
  flowproof was watching one boundary and there were two.

  The seccomp supervisor that enforces egress now also traps the destructive
  filesystem syscalls — `unlink`, `unlinkat` (including `AT_REMOVEDIR`),
  `rmdir`, `rename`/`renameat`/`renameat2`, `truncate`, `ftruncate`, `creat`,
  `openat2`, and the open family when the flags carry `O_TRUNC` — and any flow
  already engaging containment prints what it destroyed:

  ```
  filesystem observation: observed (linux seccomp); 2 destructive syscall(s)
    unlinkat /home/u/exports/2025.csv at 412ms
    openat [O_WRONLY|O_CREAT|O_TRUNC] /home/u/db.sqlite at 899ms
  ```

  It **observes**. No new step, no new spec key, nothing new to assert on —
  the honest edge of the mechanism, not a first instalment. Every trap replies
  `SECCOMP_USER_NOTIF_FLAG_CONTINUE`, so the kernel runs the syscall and
  re-reads child memory *after* the supervisor decided, which a sibling thread
  can race. Fatal for a verdict, acceptable for a report: the trap fires on
  syscall NUMBER, so a path can be stale but a destructive syscall cannot hide.

  The open family is gated in-kernel on the `O_TRUNC` bit, so an ordinary read
  or append never wakes the supervisor and a contained run keeps its speed.
  Said out loud rather than left to be discovered: an `open` for writing
  *without* `O_TRUNC` followed by a write at offset 0 corrupts a file and fires
  nothing.

- **An action inside an iframe was refused, and the reason had stopped being
  the whole truth.** v1 refused every framed action on the grounds that
  actions act at composited coordinates resolved against the main document,
  so one could "succeed" without touching the frame. That is right — about
  COORDINATES.

  A same-origin frame does not need them. The parent's own scripts can
  reach `iframe.contentDocument`, so a value action is driven through the
  frame's DOM, the same mechanism `Select` already uses in the main
  document, and nothing is dispatched at a point.

  ```yaml
  - Scroll the "css:body" in the iframe "container" to 147px
  - Type Ada into the "css:#coupon" in the iframe "container"
  ```

  `Type`, `Replace`, `Clear`, `Check`/`Uncheck`, `Remember` and `Scroll`
  work inside a frame now. **Pointer actions stay refused** — `Click`,
  `Press … button`, `Hover`, `Double-click`, `Right-click`, `Upload` — and
  the error says why: a framed pointer action could only arrive as an
  untrusted event, which an application may ignore while the step still
  passes. That is release-without-effect, the shape `docs/design.md`
  already refuses for drag.

  **A framed `Type` is not the main-document `Type`, and the docs now say
  so.** In the main document it is real keystrokes; in a frame it is a value
  assignment plus `input`/`change`, so an app filtering on `keydown` will
  not see it. Two guards keep that honest rather than silent: the target
  must not be `disabled` or read-only — a value assignment succeeds on a
  disabled control where real typing would be ignored, so it is refused by
  name — and the value is read back from the element afterwards, so a
  control that rejected it fails the step.

  Resolve, guard, act and verify happen in ONE round trip, because a frame
  can navigate between calls and a write into a detached document succeeds
  and reads back correctly while touching nothing.

- **A container could only be scrolled to its edges.** `Scroll … to the
  top|bottom` had two positions, and an application that keys behaviour to
  an exact `scrollTop` had no step at all.

  `Scroll "<target>" to 147px` scrolls to an exact offset. Pixels, not a
  percentage, and this is the one place pixels are right: `scrollTop` is a
  real DOM unit applications assert on, while a percentage of `scrollHeight`
  is a unit nothing reads. (The click offset takes percentages for the
  opposite reason.)

  The unit is required — `to 147` is a parse error, so a second unit could
  never change what an old flow meant — and the offset is verified after the
  write with a ±1 tolerance, because `scrollTop` reads back fractionally
  under a non-integer device pixel ratio. A container that stops short
  FAILS rather than clamping silently, and a target whose content fits is
  refused, since scrolling it would pass without moving anything.

- **A control that acts on WHERE it was hit could only be hit in the
  middle.** `Click` goes to the element's midpoint, which is the right
  default and the only thing there was. A split button, a slider track, a
  canvas region - anything reading `offsetX`/`offsetY` - had exactly one
  reachable point, and for a control whose two halves do different things
  that point is arbitrary.

  ```yaml
  - Click "Click into my right half" at 75%,50%
  ```

  **Percentages of the element's own box, never pixels.** An element's size
  depends on the viewport and the font, so a pixel offset recorded on one
  machine addresses a different part of the control on another - the same
  reason the ordinal exists instead of coordinates.

  Out of range is a parse error, not a clamp: a clamped `120%` becomes an
  edge click that looks deliberate and is not what the author wrote.

  And the point is verified before the click dispatches, with the hit test
  `Hover` already uses. An offset can leave the element entirely - a
  rounded corner, an overlapping sibling - and a click that lands on the
  occluder while reporting success is the false green this repository keeps
  finding. It fails instead, naming the offset.

- **A dropdown inside a table row had no spelling at all.** Every other
  action learned the scope suffix - `Click`, `Type`, `Clear`, `Check`,
  `Press … button`, `Right-click`, `Remember` - and `Select` was simply
  never wired to it.

  That is the one control where it hurts most. A page that puts a `<select>`
  in each row gives them all the same label, so the only way to reach the
  third one was `the 3rd "Value"`: counting, which is exactly the
  positional addressing the scoped targets exist to remove. Change the row
  order and the flow selects in the wrong row, quietly.

  ```yaml
  - Select Approved from the "Value" column of the row containing "Invoice 4711"
  - Select "A", "B" from the "Tags" field in the item containing "Invoice 4711"
  ```

  It is the shared suffix parse, so everything it already understood comes
  along: the cell and container forms, the multi-option list, and
  conjunctive anchors. The role noun becomes optional once a scope has
  named the control - `"Value" column of the row containing "X"` is already
  unambiguous - but an unscoped quoted label still requires `field` or
  `dropdown`, so nothing that used to be a parse error quietly becomes a
  page-wide guess.

- **A page that rolls a dice could not be tested, only watched.**
  `browser.clock` has always pinned what a page reads as "now", on the
  argument that a flow whose meaning changes daily is not a test. The other
  half of that argument was never built: a page that mints a value from
  `Math.random` shows something different on every single run.

  For a value the flow only has to *compare*, a second read gets you by. For
  one it has to **enter** — a generated order id typed into the next field,
  a sum of two drawn numbers — there is nothing to read and no literal to
  write. The flow could not be authored at all.

  ```yaml
  browser:
    random:
      seed: 1234
  ```

  A seeded PRNG replaces `Math.random`, injected before any page script for
  the same reason the clock's shim is: a page that has already drawn cannot
  be un-randomised afterwards. Pinned, the value is a constant you can write
  by hand, and record and every replay see the same one.

  `seed` is a literal, never a `${VAR}` — a seed resolved from the
  environment would make one trace mean different things on different
  machines, which is the drift pinning exists to remove. Web-only, refused
  by name elsewhere rather than silently doing nothing, because there is no
  `Math.random` to pin on a desktop window or an OCR frame.

  Deliberately narrow, and stated rather than left to be discovered:
  `crypto.getRandomValues` is untouched (a security primitive, not a
  convenience), workers keep their own real `Math.random`, and server-side
  randomness remains `mock:`'s job.

  The test is the CONTRAST, because a pinned run agreeing with itself proves
  nothing if the page was going to agree anyway: unpinned, three loads draw
  three different values; pinned, three loads draw one; and a *different*
  seed draws something else again — without that last check, a shim that
  returned a constant would pass.

  **What it pins is the sequence, not the position**, and that distinction
  is worth stating before somebody meets it. The page draws the same series
  every run; which member of it reaches the value you care about depends on
  how many draws the page made first. A page whose earlier scripts draw a
  variable number of times can still hand you a different value — the same
  series, one place along. Seen once against a page that generates on
  focus: stable across six runs, shifted by exactly one draw on a seventh.
  So a flow that types a constant derived from a draw should assert the
  draw first, and the docs now say so.

- **A row that needs two columns to name it could not be named.** The row
  target identifies by content instead of position, which is the whole
  reason it exists — but it took one anchor, and one column is often not
  unique. A table with two people called John and two called Doe has no
  single anchor that finds John Doe's row. `containing "Doe"` failed with
  `matches 2 rows`, correctly and unhelpfully, and the only thing left to
  write was `tr:nth-child(2)`: the positional selector this target was built
  to remove, reintroduced by hand.

  ```yaml
  - Click the "Edit" in the item containing "John" and "Doe"
  - assert: the "Email" column of the row containing "John" and "Doe" shows john@example.com
  ```

  Every anchor must be in the SAME row, so the conjunction narrows rather
  than widens. Anchors that sit in different rows match nothing rather than
  picking one of them, and a conjunction that is still ambiguous gives the
  same `matches N rows` error a single anchor does — the diagnostic is not
  weakened by having more ways to be specific.

  The quotes delimit and `and` is a separator, the same rule the multi-option
  `Select` above uses, for the same reason: an anchor is arbitrary page text.

  This is not the selector growth the charter declines. That clause is about
  adopting another framework's idioms; this is flowproof's own
  identity-not-position principle reaching a table where one column does not
  identify anything.

- **Selecting four options selected one, and said nothing.** `Select` commits
  through the control's value setter, which is correct and is also a
  *replacement*. So the obvious spelling — four `Select` steps at one
  `<select multiple>` — left the last option standing, fired four `change`
  events at an app expecting one, and reported success.

  Nothing was available to write instead. The grammar had one option per
  step, and a multi-selection is not a sequence of selections; it is one
  state the control ends up in.

  ```yaml
  - Select "Functional testing", "GUI testing" and "End2End testing" from the "Methods" field
  ```

  Set-a-state, like `Check`: what is named becomes selected, what is not
  named does not, and the step means the same thing however the environment
  arrived. One `input`+`change` for the final state, because what the app's
  handler expects to see is a user finishing a selection rather than three
  of them.

  **Every item is quoted**, and that is the design rather than decoration.
  Option text is arbitrary application text: `"Rock, Paper and Scissors"` is
  one option on somebody's page, and a list split on commas and the word
  "and" would read it as three — silently, by selecting the wrong set. Only
  the quotes delimit.

  Names are resolved before anything is selected, so a typo in the third
  option leaves the control untouched rather than half-applied, and the step
  then reads the selection back to verify it took. Found while writing that
  check: a JS exception inside the driver does **not** reach Rust as an
  error, so the first version of this reported success on an option that
  does not exist. The outcome is now a value that comes back and is
  inspected, not an exception nobody catches.

- **A count could be asserted but never used.** `the "css:.row" appears 5
  times` has always worked, and it is the right step when you know the
  answer. When the app decides the answer — a table built from whatever the
  backend returned — there was nothing to write. The number existed, the
  grammar could compare it to a literal, and no literal was available.

  `Remember how many "<target>" appear as <name>` reads it into the same
  flow-scoped name a text capture uses, so everything captures already do
  keeps working:

  ```yaml
  - Remember how many "css:.order-row" appear as rows
  - Type ${captured.rows} into the "Rowcount" field
  - assert: the "Total" shows ${captured.rows} + 1
  ```

  Counting rides the ordinal every adapter already implements, so it means
  on each adapter exactly what `the 2nd "Row"` means there — no adapter
  changed to gain it. The number is read at execution time on record and on
  every replay, so only the reference enters the trace and a page that grew
  a row does not need it rewritten.

  **Zero fails.** A selector typo matches nothing, and so does an empty
  table — and `0` is a confident, plausible number to type into an
  application. A capture that answered `0` to both would be the silent
  wrong-value class again, so the step refuses and names the one that means
  zero: `assert: the "<target>" appears 0 times`.

  There is no second definition of what a number is: the computed
  comparison's normalisation is reused as it stands, which is what lets
  `shows ${captured.rows} + 1` compose without either side knowing about
  the other.

### Fixed

- **The containment tier was reported by a guess taken before the run.**
  A flow that engages egress prints a tier line, stores one in the trace,
  and keeps one in the run record. Only the trace was reading the tier the
  run *achieved*; the printed line, the `--json` payload and the run record
  all called the pre-run probe and reported that instead.

  On Linux the two cannot disagree — the seccomp filter installs in the
  child's `pre_exec`, so reaching a finished run means it installed. The
  probe was therefore not wrong, it was *load-bearing for a reason that
  had quietly stopped being true*: containment on other platforms has
  steps that fail after a probe says yes.

  Two artifacts describing one run differently is worse than either being
  wrong on its own, because an auditor has no way to tell which is lying.
  And in the run record it cost evidence rather than a label: the same
  field decides whether the blocked lane is evidence at all, so a
  mismatched tier either presents destinations this run never blocked or
  discards the ones it did.

  There is now one definition, `achieved_tier`, used by the certifying
  path and the reporting path alike — those computing it separately is how
  they came to disagree. The corrected line prints only when the run's
  answer differs from the prediction, so nothing about Linux output
  changes.

- **`Select` typed the option name when the option did not exist.** A JS
  exception inside the driver does not reach Rust as an `Err`, so the throw
  on a missing option looked exactly like "this element is not a
  `<select>`" — the one case the code below it exists to handle — and it
  fell through to typing the option's name into the dropdown.

  Typing into a `<select>` is itself a prefix search, so the step landed on
  whatever option starts with the same letters and reported success. Not a
  failure and not the option that was asked for: a plausible wrong answer,
  chosen quietly.

  The two cases are now different answers rather than the same one. A status
  value comes back and is inspected: `not_select` keeps the fall-through to
  typing, which is what it was always for, and a name matching nothing fails
  naming it. Prefix matching is untouched — it is the documented ladder
  (value, then exact visible text, then prefix), and `Audit` selecting
  `Auditor` is correct.

  Found while building the multi-option form, whose first version had the
  same defect and was caught by its own red-path test.

- **The docs never said what a typed capture does with the text around
  it.** `authoring.md` showed one form, `Type ${captured.oid} into the …`,
  and said the value is read fresh on every replay. It did not say that
  several references resolve in one step, that literal text between them is
  typed as written — or, the part that matters, that none of it is
  evaluated.

  So `Type ${captured.a} + ${captured.b} into the "Sum" field` types
  `12 + 30`. That is interpolation working exactly as designed. It is also
  close enough to an answer that a flow can go green on it while asserting
  nothing anybody meant, and the page that reads like the complete account
  of captures did not mention it.

  Documented now, with the non-arithmetic stated outright and the reason:
  a capture is text the app displayed, and handing it back is data entry;
  deriving a new value from two of them is a computation, and a trace
  carrying a computation has stopped being a recording. The one exception
  stays where it was — `shows ${captured.x} + <number>` on the assertion
  side, which answers "did this change by the right amount?", a question no
  literal can express.

  Pinned by a test that asserts `12 + 30`, so if arithmetic is ever added it
  has to arrive as a spelling that cannot be mistaken for this one.

- **A step the grammar had decided not to have was built by the model
  instead.** `record` resolves with rules first and hands anything they
  cannot parse to the LLM author. That is right for a step nobody has taught
  it yet. It is exactly wrong for a shape somebody deliberately decided
  should not exist: a loop, a conditional, a regex capture, a date
  expression. Those do not parse either, so they went to the model, and the
  model does what it is asked — it grounds the step onto something.

  `Click "Next" until the "Status" shows Done` became a single click. It
  recorded green. Then it failed roughly one replay in five, because the
  page needed between three and nine clicks, and nothing anywhere said the
  word "until" had been ignored.

  Every one of these declines already existed — in the charter, in
  `docs/design.md`, in this repository's own comments. What did not exist
  was anything that made one hold. A decline written only in prose is a
  decline the engine does not implement.

  So refusal is now a state the parser can be in, distinct from failure.
  `RulesError::Refused` means *recognised and declined*, it names what to
  write instead, and — the half that matters — it never reaches the model
  author. `Unresolvable` still falls back, because a step the rules do not
  understand is a question and the model is a reasonable answer to a
  question. A refused step is not a question.

  The test that pins it asserts `calls == 0` against a counting model
  client, in `Auto` mode: the mode that *would* fall back. A refusal proven
  only by its error message would be a refusal the fallback could still walk
  around.

- **The suite was red off Linux, for a wording.** `cargo test --workspace`
  failed on macOS and Windows on `a_control_record_states_the_containment_tier_it_ran_under`,
  which asserted the tier string contained the literal `Linux-only`. When
  Windows containment started, the message became "enforced on Linux
  (seccomp) and in progress on Windows" — more accurate, and no longer
  carrying that phrase.

  Linux CI stayed green, so nothing caught it: the assertion is inside the
  `else` branch that only non-Linux hosts take. What it cost was every
  local verification off Linux, where the required `cargo test --workspace`
  is red before you have touched anything — and a gate that is red by
  default teaches people to read past it.

  The test now asserts the substance, that the reason names where
  containment IS enforced, rather than one wording of it.

- **The published schema rejected traces flowproof itself writes.** The
  action `type` enum in `trace-v1.schema.json` listed thirteen of the fifteen
  actions the engine emits. `capture` and `set_checked` were missing, so any
  trace containing a `Remember …` or a `Check …` step failed validation
  against the schema this repository publishes as the contract.

  Nothing caught it, and the reason is the interesting part: the conformance
  test validates one fixture, and that fixture contained neither action. The
  schema was wrong for exactly as long as nothing exercised it — a gap that
  fails *green*, and on the artifact external consumers validate against
  rather than on anything our own code reads.

  Both are in the enum now, with their param shapes, and the fixture carries
  one of each — including the counted `capture` reading — so the enum cannot
  drift from the emitted actions again without a red test. Verified the
  other way too: with the old enum restored, the extended fixture fails.

- **Our own suite had a flaky test, and it was red-lighting `main`.**
  `seeded_fixture_mutation_survives_navigation` wrote two pages to disk and
  navigated between `file://` urls, asserting that a cart mutation survived
  the navigation. Chrome does not reliably share localStorage between two
  file documents — it partitions them — so the mutation could vanish for a
  reason with nothing to do with seeding, and the run failed with
  `cart: MISSING` after three green steps.

  It is off the pull-request path, running only on `main` and nightly, so it
  failed after merge rather than before it — three times across the day,
  interleaved with passes, on commits that had nothing in common.

  The fix was already in the file. `serve_site` serves several pages from one
  loopback port, which is one origin, and its doc comment says in as many
  words that `file://` is not a usable substrate for storage tests. This test
  predates its adoption and never moved over. Now it does.

  A flaky test in a testing tool is worse than elsewhere: the product's whole
  claim is that a green run means something.

- **A column was addressed by counting `th`s against `td`s.** The column
  index came from the table's header elements; the cell came from the data
  row's `td`s. A grid whose header row opens with a stub above the row-label
  column (`<tr><td></td><th>Monday</th>…`, every schedule ever built) shifts
  every data cell one place right of its header, so `the "Thursday" column of
  the row containing "11:00 - 13:00"` read Wednesday.

  It returned a real cell with real text, so the assertion passed — against
  the wrong day. Silently answering about the neighbouring column is the
  failure this target exists to remove; a header rename is loud, and this was
  not. Both sides now count header and data cells together and index the
  header within its own row.

- **`is visible` passed on a `display:none` element.** The second of two
  assertions that could not fail, found alongside the column bug above and
  sharing its shape: it answered a narrower question than the one the author
  asked, and the narrower answer was always yes.

  Presence was the whole check, and a hidden element is still in the DOM and
  still answers every selector — so the assertion could not fail on the one
  condition anybody writes it for. It reported an input visible while
  `Scroll … into view` refused the same element as unreachable, which is the
  contradiction that gave it away.

  Rendered-ness is now the browser's own answer (`display:none` anywhere up
  the tree, `visibility:hidden`, `content-visibility`, `hidden`), and
  `is not visible` is satisfied by absent OR hidden, because the user cannot
  see either. A failure distinguishes *present and not rendered* from *never
  appeared* — different bugs in the app under test, and the old message sent
  the reader after a selector that was already correct. Surfaces with no
  notion of rendered-ness beyond resolution (UIA, SAP, vision) keep the
  presence reading rather than inventing a second opinion.

## 0.11.0

Three things the driver could already do, and the surface would not let you
ask for: press a function key, type a value the application invented, and
watch the browser while you record. Each obstacle was one layer thick —
`virtual_key` has always mapped `F<n>` to `0x6F + n`, the field-entry path
never cared where its text came from, and Chromium takes a headed flag. The
grammar was the whole of what was missing, which is why the shipped SAP
adapter could not express F8.

The fix is that story inside out. A capture in a `Type` step was neither
refused nor resolved — it was entered as the eleven literal characters of its
own reference. A surface that does neither is worse than one that refuses,
because the refusal is loud and the flow that types the wrong value goes
green.

### Fixed

- **A capture in a `Type` step entered the reference as literal text.** The
  check that was supposed to refuse captures in actions sat below the branches
  that parse first, so `Type ${captured.oid} into the "Order id" field`
  recorded happily — and typed the eleven characters `${captured.oid}` into
  the field. `${VAR}` resolution leaves it alone because a secret name may not
  contain a dot, and nothing else looked. The docs said it was a parse error;
  it was neither an error nor correct.

  `Go to ${captured.oid}` slipped past the same way, for the same reason.

  Entering the wrong value silently is the worst outcome available to a
  testing tool: the flow goes green having done something nobody asked for.

### Added

- **A remembered value can now be typed.** `Remember the "id:oid" as oid`
  then `Type ${captured.oid} into the "Order id" field`. The capture resolves
  at execution time on record and on every replay, so — exactly like a
  `${VAR}` secret — only the reference ever enters the trace.

  This is the only way a value the application generates per run can be
  entered at all. There is no literal for a trace to record, so a recording
  of one would be wrong the moment it replayed. Proven with a fixture that
  mints a random id per run: three replays, three different ids, green each
  time, and the trace holds `"text":"${captured.oid}"` and no number.

  **Typing is where it stops.** A capture still may not choose an element or a
  destination — `Click "${captured.x}"`, `Go to ${captured.x}`, or one inside
  a target label are parse errors naming the reason. Supplying text the app
  just displayed is data entry, which is what a user does with a generated id;
  picking the next element is control flow decided by the system under test.
  A name that was never remembered fails closed, listing what was in scope.

- **Function keys could not be expressed at all, on any app.** `Press F8`,
  `Press Alt+F4`, `Press Ctrl+F2` — none of them parsed. The key list held
  `Enter`, `Escape`, `Tab`, the arrows and a handful more, and a key outside
  it was accepted only as a single character with a modifier, which is why
  `Control+V` worked and `Alt+F4` could not.

  That list gates every app, and SAP is largely *driven* by function keys —
  F3 back, F4 value help, F8 execute — frequently the only way to reach an
  action with no clickable equivalent. A shipped adapter could not express
  its most common interaction.

  The keystroke half was never missing: `virtual_key` has always mapped
  `F<n>` to `0x6F + n`, in the table every SendInput-based driver shares.
  Only the grammar refused to author it, so this reaches through to the
  driver that could already do the work.

  Found the hard way. A flow ending in "close the app" recorded fine — the
  authored step grounded onto Calculator's real title-bar control — and then
  failed every replay with `element did not appear within 5000ms`, because
  Windows renders that title bar through XAML and its caption buttons are not
  dependably in the UIA tree. All three rungs of that step's selector ladder
  described the same control, so the ladder that normally absorbs drift had
  nothing to fall back to. `Alt+F4` never touches the tree at all, and was
  the one thing that would have worked.

  **Web refuses them at authoring time rather than at replay.** The browser
  has no key definition for `F1`–`F12`, so a step that recorded happily would
  die a run later, far from the spec line that caused it. The error names the
  platform and points at `Press the "<label>" button`.

- **You can watch a web recording now.** `app: web` ran headless with no way
  to turn it off — the flag was a literal `true` in `launch_browser`, and the
  one thing that looked like an escape hatch, `browser: { args: [...] }` in the
  spec, could not help: Chrome has no flag that undoes `--headless`, and by the
  time a spec's args are appended it is already in the argv.

  That is the wrong default for exactly one step. Replay should be headless —
  it runs in CI, unattended, often with no display. But **recording is the
  human-in-the-loop step**, the one where you are watching to see whether the
  thing you described is the thing that happens, and doing it blind means a
  misread selector looks identical to a page that never loaded.

  `FLOWPROOF_HEADED=1` shows the window. An environment variable rather than a
  spec field, because watching is a property of the run you are supervising and
  not of the flow: a committed `headed: true` would follow the flow into CI,
  where nobody is watching. Presence-based like `FLOWPROOF_NO_SHARED_BROWSER`,
  so `FLOWPROOF_HEADED=0` still shows the window — a variable you bothered to
  set is one you meant.

  One caveat, documented beside the feature: headed Chromium sizes its window
  from the desktop and headless uses a fixed default, so a flow with visual
  assertions should pin `browser.viewport` or its baselines get recorded at one
  size and replayed at another.

## 0.10.1

Two defects that 0.10.0 shipped with, both found in the field on a
colleague's machine within an hour of each other.

### Fixed

- **LLM authoring could not work at all on the default model, and said
  nothing useful about why.** flowproof sent `temperature: 0` on every
  Anthropic request. The API has deprecated `temperature` on its current
  models, so the default — `claude-sonnet-5` — rejected every call with a
  400, as did Opus 5. Following `docs/getting-started.md` exactly, with a
  valid key and a funded account, produced `model call failed (anthropic):
  http status: 400` and nowhere to go.

  Nowhere to go is the real defect. The response body said `` `temperature`
  is deprecated for this model `` — one sentence, naming the field, and it
  was thrown away because only the status reached the error. Found in the
  field, on a colleague's machine, after the setup path had been assumed
  working; the diagnosis needed a hand-built `curl` reproducing flowproof's
  own request, which is not something a user should have to construct.

  Three things now hold. `temperature` is gone from the Anthropic request —
  authoring never needed it, since the model must copy a target token from
  the live scene verbatim and the result is recorded for review. A non-2xx
  quotes the provider's own explanation, with the API key redacted from it,
  because that text is what people paste into bug reports. And the answer is
  read from the first **text** block rather than the first block: a reasoning
  model emits `thinking` ahead of its answer, which would have been the next
  wall, reported as an unexpected response shape.

  `temperature: 0` stays on the OpenAI-compatible path, where it is accepted
  and free.

### Changed

- **The run wrote a visual report and never said so.** Every replay produces
  `report.html` — the step table, the per-step frames, the recording as one
  animation — and the verdict line pointed at `result.json` instead. On macOS
  and Linux the only adapter that drives a UI is `web`, and it is headless, so
  a first run shows no window and ends by naming a JSON file. The reasonable
  conclusion is that nothing visual was captured.

  It was, in a directory Finder hides by default. Found by watching someone
  reach a green first run on a Mac and ask how they were supposed to know it
  had done anything — the failure mode the charter ranks above the current
  milestone, arriving one step later than expected: not "cannot get to a first
  green run", but cannot tell that they did.

  The verdict line now names `report.html`. `result.json` is unchanged, still
  written to the same bundle, and still what `--json` reports as `report_path`
  — the machine surface stays the machine surface. **This changes stdout**: a
  script scraping the path off the human line now gets the HTML rendering.
  Read `--json` instead, which is what it is for.

## 0.10.0

### Fixed

- **A tool call the agent really made could be missing from the evidence.**
  0.8.0 fixed the MCP stand-in losing its whole recording when an agent killed
  it, by persisting the lane after every captured call. It was not enough. The
  stand-in forwarded the server's response to the agent BEFORE persisting it,
  and the moment the agent has a response it may kill the stand-in — so the
  final call of a session was lost while `record` still exited 0.

  Found by the falsifiability suite, with the real server's own request log as
  the oracle: the server was asked for `tools/call get_weather`, the lane held
  only `initialize` and `tools/list`, and nothing said so. A lane that silently
  runs short understates what the agent did, and every assertion evaluated over
  it inherits the omission — including a guard claim that a tool was never
  called.

  Capture and persistence now happen BEFORE the response is forwarded. That
  ordering is the fix: by the time the agent can act on a response, the evidence
  for it is already on disk.

  And a failed flush is no longer swallowed. With the write at stdin EOF no
  longer load-bearing, an incremental flush that fails means evidence was lost,
  so it now fails the record by name instead of minting a partial lane. Evidence
  lost silently is the one outcome this tool cannot ship.

### Changed

- **A control that stopped being certifiable passed the gate.**
  `audit --since` fired on a removed control or one that turned `fail`. A
  control going `pass` -> `capability-error` exited zero — so a runner without
  seccomp, a driver that lost a capability, or a flow that quietly stopped
  running took coverage with it and failed nothing.

  `capability-error` exists precisely so that "we could not check this" never
  reads as "this is fine". `assert_no_egress` on a host that cannot enforce it
  fails as a capability error rather than passing vacuously, and the docs are
  emphatic about why. At the diff layer that distinction had been lost, in the
  one artifact the evidence claim rests on.

  It now counts as a regression, with a red-path proof pinning it. **This will
  fail builds that used to pass**: an unchanged suite run on a host that cannot
  enforce a control now goes red. That is intended — the remedy is to fix the
  host or narrow the control, never to soften the rule.

### Added

- **The HTTP MCP boundary had never been driven through the CLI.** The coverage
  table said it: exercised over real HTTP, but driven directly rather than
  through `record`/`run` with a real agent. Only the round trip can prove the
  things that matter about this transport — that the `FLOWPROOF_MCP_URL_<NAME>`
  flowproof injects is what a real agent actually reaches, that the lane is
  captured through `record`, and that `run` serves it back.

  A real agent subprocess now speaks JSON-RPC over HTTP to flowproof's listener,
  against a real HTTP MCP server, and the flow then replays with that server
  stopped and deleted from disk. The real server logs every request it receives,
  so the test has an independent oracle: "the lane is complete" and "the real
  server was never contacted at replay" are checked against the server's own
  account rather than flowproof's.

  With this the coverage table has no remaining gaps — every row is a CLI round
  trip with a real agent rather than an assertion about one.

- **The `agent.url` driver had never been recorded.** The coverage table said
  so plainly — "replay covered; the RECORD path has no test" — and `proxy_port`
  appeared nowhere in the test tree, only in `src/`. That is the half where the
  driver's distinctive risk lives: flowproof cannot inject environment into a
  service it did not start, so the service must already be pointed at the proxy
  by whoever launched it, and getting that wrong is a RECORD-time failure a
  replay test can never reach, because by then the cassette exists.

  A service flowproof does not start is now launched independently, pointed at
  the fixed `proxy_port`, triggered, recorded, and replayed with no model
  reachable. The trace is checked for the prompt and the reply rather than for
  its own existence — on a driver whose verdict must never come from the
  trigger's HTTP status, a file proves nothing.

  Needs no model credential: the upstream is a loopback fake and replay makes
  zero model calls.
- **Neither half of the call-order rule was tested.** 0.8.0 dropped positional
  cassette matching, because goose issues its task call and a session-title call
  concurrently without waiting: whichever landed first at record decided the
  order, and a positional matcher reported a divergence when nothing about the
  agent had changed. Nothing exercised the new behaviour.

  Two proofs, and the second is what keeps the first honest. Two independent
  calls recorded in one order replay in the other and still pass — the
  tolerance. An agent that changes what it SENDS still diverges — the
  discrimination. Without the second, "order-tolerant matching" would be
  indistinguishable from "the request is not checked at all", and a matcher that
  accepted anything would satisfy the first proof perfectly.
- **The argument matchers had no red path.** `assert_tool_call` could be proven
  to fail on the tool NAME — a flow demanding a tool the agent never calls
  refuses the trace — but nothing proved the `where <path> <matcher>` clauses
  can fail at all. Every `where` clause in the suite asserts an argument the
  model was always going to produce, so each passes whether the matcher works or
  not.

  That is the layer carrying the most weight and the least evidence.
  `docs/agent-testing.md` calls argument assertions "usually where the bugs
  are", and names chained arguments — threading one tool's result into the next
  call — as the behaviour multi-step agents actually get wrong.

  One committed guilty call covers both halves of the vocabulary: the right tool
  with the wrong city violates a value matcher (`where city equals Nairobi`) and
  a presence matcher (`where city is absent`) at once. Both refuse the record,
  and neither mints a trace.

- **The guard assertion had no end-to-end test that could fail.**
  `assert_no_tool_call` is the one the security story leans on — the model
  asked, and the code refused — and `docs/agent-testing.md` calls it arguably
  the highest-value assertion in the feature. Every end-to-end use of it in the
  suite sat in a flow where the forbidden tool was never going to be called, and
  such a flow passes whether the assertion works or not. Replace the assertion
  with an unconditional PASS and nothing end-to-end would have noticed, while
  every guard flow an adopter wrote stayed green and proved nothing.

  The violating input has two halves, both deliberate: a model that ASKS for the
  forbidden tool, and an obedient agent with no guard of its own that calls it.
  A guard flow over that trajectory must refuse the trace, name the tool, and
  mint nothing — the last of those because a record that failed loudly while
  still writing a trace would enshrine a guard that never held as a cassette
  replaying green forever.

  The assertion behaved. The proof is what stops it silently ceasing to.

- **`assert: reply contains` had no failing direction anywhere.** Every
  end-to-end use of it asserts text the model was always going to produce, so
  those flows pass whether the assertion works or not — and this is not a
  hypothetical shape of hole. It is the assertion that already hosted a false
  green: in 0.9.0 a streaming client handed one buffered body assembled the
  identical final text, so `assert: reply contains` stayed green for exactly the
  defect it existed to catch.

  The violating input is the model's own final answer, committed as a fixture so
  a reviewer can see what makes it guilty without reading the harness: the flow
  asserts "sunny", the reply says it is raining. The record must refuse, and mint
  no trace — a refused record that still wrote one would leave a cassette whose
  reply assertion never held, replaying green from then on.

## 0.9.1

### Security

- **The recommended way to supply the record credential also handed it to the
  agent.** flowproof holds the upstream model key and attaches it only on the
  outbound hop, so the agent under test gets a placeholder — that was the
  design, and for `OPENAI_API_KEY` and `ANTHROPIC_API_KEY` it was what happened.
  Overwriting two names is not the same as masking a credential. `Command`
  inherits the parent environment, the spawn path only ever *set* variables, and
  the first name `flowproof record` looks in is `FLOWPROOF_AGENT_KEY` — which
  nothing overwrote. So the documented, first-precedence way to supply the key
  was also the one way it reached third-party code verbatim, and
  `FLOWPROOF_AI_API_KEY` passed through the same way. Nothing asserted any of
  it: grepping for the placeholder returned two production lines and no test, so
  the masking that did work was working by inspection rather than by
  measurement. Every variable that can carry a real key is now named in one
  place and removed before the placeholders are handed out, with the spec's own
  `agent.env` still able to pass one through deliberately. **If you record with
  `FLOWPROOF_AGENT_KEY` or `FLOWPROOF_AI_API_KEY` set, assume the agent could
  read it on 0.9.0 and earlier.**

### Added


- **Nothing proved that a failing control is recorded as failing.**
  `audit_record_e2e.rs` covers a great deal — that audit reads a run record
  rather than re-replaying it, and that `--since` exits non-zero on a
  regression — but every verdict it asserts is either `capability-error` from a
  flow an env gate skipped, or hand-written into a record fixture. No test ran a
  control-bearing flow that genuinely failed and checked what verdict the record
  received. That is the highest-consequence gap the control map can have: the
  map is the artifact a reader trusts precisely when they cannot read the trace
  themselves, so a control reporting `pass` over a failed flow would misreport
  the one thing it exists to say.

  The first entry in a new falsifiability suite closes it. A committed fixture
  is recorded honestly against a live loopback endpoint, then run with the port
  dead so the assertion fails for real at replay, and both layers are checked:
  the verdict in `report.json` (re-rendered unchanged by `audit`), and the exit
  code a CI run would see. Two layers deliberately — 0.9.0's streaming false
  green survived because equivalence was checked at the wrong one.

  See [docs/how-flowproof-tests-flowproof.md](docs/how-flowproof-tests-flowproof.md)
  for what a red-path proof is and the rules for adding one. The verdict path
  behaved correctly; the proof is what keeps it that way.

- **Nothing proved `audit --since` can decline to fire.** The existing tests
  prove the gate goes red on a regression — a removed control, or one that
  turned failing. A gate wired to exit non-zero unconditionally would have
  satisfied every one of them, and CI would have gone red on every clean run
  until somebody stopped believing it. That is the shape a false signal takes in
  a gate: not a missed regression, but a verdict so frequent it stops meaning
  anything.

  Two proofs now cover the discriminating half: an unchanged control set exits
  zero with an empty diff, and an ADDED control is reported but does not fail
  the gate — a suite that could not grow without going red would teach its
  owners to stop adding controls. Both check the diff content and the exit code,
  because a gate reporting an empty diff while still exiting non-zero is the
  same defect wearing the other face.

- **An unattended SAP run had nobody to log back in.** If the session expired
  mid-job there was no way to recover it, which is exactly what a nightly run
  hits. With `SAP_USER` and `SAP_PASSWORD` set, `connect()` now recognises SAP's
  own logon screen — an empty `Info.User` sitting on transaction `S000`, rather
  than an authenticated session — types the standard client/user/password
  fields, and keeps polling until `Info.User` is actually populated before
  calling the session ready. A no-op when the variables are unset: attach-only
  and an explicit `OpenConnection`-without-login behave exactly as before.
  Values are read fresh for a single property-put each and are never logged,
  traced, or stored.

### Changed

- **PyPI and npm described the product flowproof used to be.** Both carried the
  old model-boundary framing while the repository had moved on, so the package
  pages and the repository disagreed about what flowproof is. Both now read
  "Deterministic evidence and control-regression gating for AI agents and the
  SAP, web and Citrix systems they drive." Metadata only; no behaviour changes.

### Fixed

- **A recorded precondition could not wait long enough for a real network.**
  The step timeout was fixed at 5000 ms, which is not enough for SAP flows
  reaching a live system over a SAProuter hop rather than localhost — and
  `navigate()` was never the bottleneck, since it returns in single-digit
  milliseconds whatever it checks. The delay is in the precondition polling
  afterwards. `FLOWPROOF_STEP_TIMEOUT_MS` now overrides the value written at
  record time. Unset, nothing changes: the default stays 5000 ms, and replay is
  unaffected either way because it reads `timeout_ms` from the trace rather than
  from a live constant.

## 0.9.0

### Changed

- **Streaming replay had no test that could fail for its own bug.** A client
  that asks for `stream: true` is meant to be served the recorded turn as SSE,
  frame by frame, in both phases — but nothing exercised that through
  `flowproof record` and `flowproof run`, and the record-mode synthesis had no
  test at all. The trap is that a streaming client handed one buffered JSON
  body assembles the identical final text, so `assert: reply contains ...`
  still passes and the run still reports PASS: any test asserting on the reply
  would have been green for exactly the defect it existed to catch. Two flows
  now record a `stream: true` agent subprocess and replay it with no model
  reachable, asserting the CHUNK BOUNDARIES the agent observed — the content
  type, the role or `message_start` frame, the whole arguments in one delta,
  the finish frame, the terminator — in both the chat-completions and the
  Messages dialect. Proven non-vacuous by mutation: collapsing either dialect's
  stream, at either record or replay, leaves the flow passing and fails these
  tests on the frames. The cassette is unchanged and holds no transport at all;
  chunk boundaries stay synthesized rather than recorded, which is what lets one
  recording serve a streaming and a non-streaming client alike.

- **The Anthropic Messages dialect is now proven end to end, record leg
  included.** It shipped in v2 and `docs/agent-testing.md` said so, but every
  test behind that claim handed the proxy a cassette written by hand — nothing
  ever recorded one. So the record path (the upstream URL the Messages API
  actually wants, the block-shaped request parser, the captured `stop_reason`)
  was asserted about rather than tested, on the one boundary the product's
  headline claim rests on. A flow now records against a Messages-dialect
  upstream with a real agent subprocess, replays it with no model reachable at
  all, and `assert_tool_call` holds across both. The README and the
  per-capability coverage tables move the dialect out of "thinner coverage",
  which is now a shorter and truer list.

### Fixed

- **`flowproof run` could not replay an agent cassette at all.** Pointed at a
  directory, the suite runner fed every `app: agent` trace to the UI-trace line
  parser and reported `invalid trace line`; pointed at a single file it loaded
  the cassette, spawned the agent correctly, and then never terminated. So the
  half of the product that records an agent's model traffic could record and
  never replay, which is the one thing a record/replay tool exists to do. Both
  halves are fixed: directory mode dispatches on the trace's own `app`, and the
  pipe drain is bounded so a subprocess that outlives its output cannot hang the
  run. Adopters holding agent cassettes written by 0.7.0 or 0.8.0 will find they
  now replay; a cassette whose recording embedded a per-run value (a timestamped
  working directory, an absolute path) will diverge on message content, which is
  the matcher working rather than a regression.

- **An upstream that rejected the credential looked like a hang.** Any failure
  from the real model became a `502` to the agent, and `502` means "bad gateway,
  try again", so a well-behaved agent retried with backoff and a record run whose
  key was simply wrong printed nothing for over ten minutes. Two things made it
  worse: ureq's default turned a non-2xx into an opaque `http status: 401` and
  discarded the body, so the one sentence naming the cause never reached anyone,
  and the failure was recorded in the proxy log but never announced. Now a client
  error is passed through unchanged - `401` and `403` are verdicts about the
  credential, and no amount of retrying makes an unauthorized key authorized, so
  the agent is allowed to give up - while `5xx` and transport failures stay `502`
  because those may genuinely succeed on a retry. The upstream's own words
  survive into the error, and the first failure prints. A record run with a bad
  credential now fails in about six seconds saying
  `upstream returned 401: {"error":"unauthorized"}`.

  **Behaviour change worth noting before you upgrade:** anything that treated a
  proxy `502` as "retry" will now see the upstream's `4xx` and stop. That is the
  point, but it is observable.

- **The key's outbound header is dialect-dependent, and the docs said otherwise.**
  `UPSTREAM_KEY_VARS` claimed the key "passes straight into the outbound
  `Authorization` header and never anywhere else". The behaviour was always
  correct - OpenAI-compatible upstreams get `Authorization`, Anthropic gets
  `x-api-key`, because that is what the Messages API reads - but the doc sent
  readers looking in the wrong place. Stated plainly now, including the
  consequence: an upstream that speaks the Anthropic dialect while authenticating
  with Bearer (an in-house gateway, say) rejects every call, and nothing about
  the error told you why.

- **A SAP session was attached without checking it was logged in.** `sap-connect`
  now verifies before attaching, so a dead session fails at connect time with a
  reason rather than at the first operation with something unrelated.


- **An agent that never started was reported as a replay that made no model
  calls, and its stderr was thrown away.** The README offers
  `flowproof run scripts/demo/order-status.flow.yaml` as the frictionless first
  green run — no key needed — so on a machine missing the demo agent's own
  `openai` package it was many adopters' first contact with the tool, and it
  said "the agent made 0 model calls, the recording has 2". That reads as
  *flowproof could not replay*; the truth was *your agent died before it spoke
  to anything*, and the traceback saying exactly why had been captured and then
  discarded. A zero-call run is now its own failure mode: it names the process
  and its exit code, prints the agent's stderr under the run, and points at the
  command flowproof actually ran. An agent that exits cleanly without calling a
  model is still the wiring failure it always was, and an `agent.url` service is
  never told it "exited 200" — its exit code is an HTTP status, not a process's.

## 0.8.0

Everything here was found by pointing flowproof at **goose** — a third-party
agent nobody here wrote. None of it was reachable from flowproof's own
examples: `weather-node` records and replays cleanly through all of it. Each
defect needed an agent doing something the harness did not anticipate, and every
one of them first presented as the ADOPTER's bug.

### Fixed

- **`record` dropped a model call the agent fired but did not wait for.** The
  cassette was read the moment the agent process exited, while the proxy pushes
  a turn only after the upstream answers, so a call still in flight was
  forwarded, answered, and silently missing from the trace — and `record` exited
  0 while doing it. Latency-driven, so a fast fake upstream hid it entirely:
  measured 3 drops in 8 runs against a 2.5s upstream, 0 in 8 after the fix.

- **Cassette matching no longer asserts the ORDER of concurrent calls.** An agent
  that issues two model calls concurrently sends them in one order at record and
  the other at replay, and a positional matcher called that a divergence when
  nothing about the agent had changed. Matching is now by BODY, byte-for-byte,
  with every recorded turn consumed exactly once — an extra call still fails, an
  edited prompt template still fails, and a sequential trajectory still matches
  turn-for-turn. This retires the "reordering tolerance… nothing has [demanded
  it]" note in the design: the first third-party agent demanded it.

- **`reply` no longer picks up an agent's side conversation.** An agent may talk
  to the model about something other than the task — goose asks it to name the
  session, concurrently and without waiting — and its answer is an assistant
  message, so "the last one" was a coin flip. Turns are now grouped by system
  prompt and the reply comes from the thread doing the work. Record went from 2
  successes in 3 to 6 in 6. A heuristic, with its limit stated in the code: an
  agent whose side conversation is larger than its real one would defeat it.

- **Egress containment denied every MULTI-THREADED agent.** `dup_child_fd` called
  `pidfd_open` on the seccomp notification's pid, which is a TID, and
  `pidfd_open` only accepts a thread-group leader — so any socket opened from a
  worker thread was refused, against an ALLOWED destination. It presented as a
  network error, not a containment bug. Python and Node agents do their
  networking on the main thread, which is why nothing caught it. Now resolves the
  leader from `/proc/<tid>/status` and retries.

- **The MCP stand-in no longer loses its recording when the agent kills it.** The
  lane was written only at stdin EOF, which needs the agent to close the
  stand-in's stdin; an agent that terminates its MCP subprocess abruptly never
  got there, and the whole recording was lost. The lane is now persisted after
  every captured call, atomically.

- **The unprotected-tool warning understands MCP tool namespacing.** A client
  normally namespaces a server's tools, so a tool intercepted under `mcp:` as
  `delete_all` reaches the model as `files__delete_all`, and comparing the names
  literally warned that a CORRECTLY intercepted tool was unprotected. Matching is
  now on a namespace separator only, never a bare substring, so a genuinely
  unprotected tool is still named.

### Documentation

- `docs/adopting.md` gains two limits the goose campaign measured: an agent that
  stamps wall-clock time into its prompt cannot replay under byte-exact matching
  (with the libfaketime workaround and its mandatory monotonic exemption), and
  the tool name an assertion matches is the model-boundary name, not the MCP
  name.


### Changed

- **The README's demo GIF shows an agent test, not a Windows Calculator
  flow.** The README opens on testing an AI agent at the model boundary and
  then illustrated it with `app: calc` — an example that needs a Windows
  desktop, so the one moving picture on the front page was of the one thing
  most readers cannot run. It now shows an `app: agent` flow: the spec, one
  `record` against a model, and a `run` that replays it with zero model calls.
  The same change of direction as 0.6.1's quickstart, applied to the hero
  image.
- **The README's Quick start leads with an agent test, not Windows
  Calculator.** The one worked example on the front page was `app: calc`,
  which needs a Windows desktop — so the section that exists to get a reader
  to a first green run dead-ended for most of them, and the paragraph under it
  had to apologise for the example above it. It now quotes the shipped
  `examples/agent-demo/weather-node.flow.yaml` (the npm path, no Python),
  records once with a key and replays for ever without one, and points at
  `app: web` / `app: api` for UI and no-UI flows; the Calculator walkthrough
  is where it already lived, in `docs/getting-started.md`. This finishes the
  move 0.6.1 started in the quickstart doc. `flowproof run
  scripts/demo/order-status.flow.yaml` is offered as a green run needing no
  key at all, since that cassette is committed. The
  `the_quickstart_quotes_the_shipped_agent_example_verbatim` test now holds
  the README's block to the shipped file too, not just the doc's.
- **The README's Status section stopped claiming v0.2.** It named a release
  four minor versions behind the wheel it describes, and undercounted the
  adapters ("all five" against six shipped). It now points at the badges for
  the current release rather than hardcoding a number that can go stale again,
  separates what is tested in CI from what is built with thinner coverage
  (naming the agent-boundary paths whose record legs have no test), and states
  the two limits a reader should know up front: an agent flow is one turn, and
  egress containment is Linux-only.
- **The README's Roadmap section is gone.** It listed shipped work as
  "Planned, not yet shipped" — npm distribution of the CLI, and incremental
  re-record, which is `record --reuse` today — so the one section whose job was
  to separate plans from reality had stopped doing it. What works is covered by
  "What works today"; the design notes it linked are now linked from
  Contributing.
- **The GIF is reproducible.** `scripts/demo/` holds the flow, the agent under
  test, a local OpenAI-compatible upstream so `record` needs no API key, and
  `make_readme_gif.py`, which captures the three commands and renders the
  frames from their real output — so the asset cannot drift from what the CLI
  actually prints. The previous GIF had no source in the repository. The demo
  cassette is committed and deterministic, so
  `flowproof run scripts/demo/order-status.flow.yaml` passes on a fresh clone
  with no key, and a `readme_demo_spec_resolves` test keeps the spec parsing.

## 0.7.0

### Added

- **The proxy and the MCP stand-ins are addressable from a spec.** The proxy
  binds an ephemeral port, so its URL could not be written into a spec ahead
  of time: an agent whose client reads a non-standard variable could not be
  pointed at it at all, and every such adopter wrote the same wrapper script.
  `agent.env` values now substitute runtime handles at spawn:

  ```yaml
  agent:
    command: ./start-agent
    env:
      AI_GATEWAY_URL: "${flowproof.proxy_url}"        # includes /v1
      OTHER_GATEWAY:  "${flowproof.proxy_url_no_v1}"  # client appends its own
      EXEC_MCP_BASE:  "${flowproof.mcp_url.my_server}"
  ```

  `agent.env` is applied last, so a mapping here overrides the standard
  variables flowproof injects. An unknown `${flowproof.*}` handle passes
  through untouched rather than failing the run.

### Changed

- **The agent runtime contract is documented in one place**: every variable
  flowproof injects, that `agent.env` overrides them, the record-time
  upstream (`FLOWPROOF_AGENT_UPSTREAM` then `OPENAI_BASE_URL`, keys from
  `FLOWPROOF_AGENT_KEY` / `ANTHROPIC_API_KEY` / `OPENAI_API_KEY`), and that
  the HTTP MCP stand-in matches ANY path containing `/mcp` - so a client
  deriving `<base>/mcp-exec/sap` from one base needs no per-path plumbing.
  All were true before and undocumented, which cost an adopter real time.
- **`assert: reply contains` is documented as reading the model boundary**,
  not the agent's stdout: it is the last assistant message in the
  trajectory. An agent that returns its answer over SSE, polling or a queue
  is unaffected.
- **The unprotected-tool warning points at a URL** rather than
  `docs/agent-testing.md`, which an npm installer does not have on disk.
  The npm README links the docs directly.

## 0.6.1

### Fixed

- **A drifted tool-call argument now names the path that moved.** Replay
  compared arguments byte-exactly but reported only the tool NAMES, so an
  argument-only change printed two identical lists and told the reader
  nothing on the exact failure the comparison exists to catch. It now reads
  `create_booking.flight.id: recorded KQ311, replayed KQ999`. A different
  tool still reports the names, and arguments that are not valid JSON report
  the whole payload rather than a precise-looking half-answer.
- **The npm darwin-x64 binary is cross-compiled from Apple Silicon.** It
  previously needed the `macos-13` Intel runner, which stopped being
  schedulable: a publish sat queued on it for over two hours, and because the
  launcher job waits for every platform leg, `flowproof` itself never
  published. The assembled binary's architecture is now checked at publish
  time, so a build that silently came out native is visible in the log rather
  than at a user's shell.

### Changed

- **The quickstart leads with an agent test, and installs with npm.**
  `docs/getting-started.md` opened with a Windows Calculator flow, required
  Python, offered pip only, and referenced a version five releases old.
  It now shows npx/npm/pip side by side and reaches a green agent test
  first; the UI walkthrough is kept, one section down.
- **`examples/agent-demo/` ships a Node agent** (`weather_agent.mjs` and
  `weather-node.flow.yaml`) alongside the Python one. The only agent example
  used to be Python, so the npm install path dead-ended at a second language
  runtime.
- **`docs/agent-testing.md` answers "how does this run with no model" on its
  first screen**, and says plainly what is and is not under test: the agent's
  own code and configuration, not whether the model is any good. The
  mechanism was previously explained 40% down the page.

### Added

- **A daily npx smoke test** installs the PUBLISHED package from npm on
  Linux, macOS and Windows and runs it. It asserts that a missing platform
  binary fails with a non-zero exit, keeps stdout clean, and says how to fix
  itself - a green CI run with flowproof never executing was the failure
  worth guarding against. The launcher previously had no test at all.

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
