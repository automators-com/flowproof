# Changelog

All notable changes to flowproof are recorded here. Versions follow the
workspace version (Rust crates, the Python wheel, and the npm package move
together).

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
