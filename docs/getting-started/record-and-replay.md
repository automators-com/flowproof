---
title: "Record and replay"
description: "Write a spec, record it once against a real model, replay it deterministically, and run a whole suite."
---

The thing flowproof is for: an agent calls tools, and you want a test that
fails when it calls the wrong one - without paying a model on every CI run.

[`examples/agent-demo/`](../examples/agent-demo/) has a real agent built on
the official OpenAI SDK, in two languages. Use the Node one if you installed
from npm; nothing here needs Python.

```yaml
# examples/agent-demo/weather-node.flow.yaml
name: Weather assistant answers with the forecast (Node)
app: agent
agent:
  command: node examples/agent-demo/weather_agent.mjs
tools:
  - name: get_weather
    result: { city: Nairobi, sky: sunny, temp_c: 26 }
steps:
  - prompt: What is the weather in Nairobi right now? Use your tools.
  - assert_tool_call: get_weather where city contains Nairobi
  - assert: reply contains sunny
```

**Record once.** This is the only step that calls a real model, so it is the
only step that needs a key:

```bash
npm install openai
npx flowproof config ai             # stores the model API key with a masked prompt
npx flowproof record examples/agent-demo/weather-node.flow.yaml
```

**Replay for ever.** No key, no model, no network to the provider:

```bash
npx flowproof run examples/agent-demo/weather-node.flow.yaml
```

```text
  [PASS] s0001 prompt
  [PASS] s0002 get_weather where city contains Nairobi
  [PASS] s0003 reply contains sunny
PASS: Weather assistant answers with the forecast (Node)
```

What just happened, and why it is worth having:

- The agent ran **for real** both times - same client, same tool loop.
- At record, flowproof sat at the model boundary and captured the exchange.
  At replay it served that recording back, so the trajectory is fixed and
  **no model was called**. A CI run costs nothing and cannot flake on
  sampling.
- `assert_tool_call: get_weather where city contains Nairobi` is the part
  that fails when the agent regresses: wrong tool, wrong argument, or a
  tool called out of order.
- `get_weather` returns a live timestamp. Replay is deterministic anyway,
  because the spec's `result:` is substituted at the model boundary.

Two limits worth knowing before you build on this, rather than discovering
them later:

- **A flow is one turn, not a conversation.** Every `prompt:` step is joined
  into a single task delivered up front; there is no follow-up user turn.
  See [agent-testing.md](../agent-testing/index.md).
- **The model boundary is not the tool boundary.** A `tools:` mock changes
  what the model is TOLD a tool returned; the agent still ran its own tool.
  Only the `mcp:` boundary stops a tool executing. flowproof warns at
  runtime when a flow relies on this.

Python instead of Node? Same flow, same assertions:
[`weather.flow.yaml`](../examples/agent-demo/weather.flow.yaml) runs
`python3 examples/agent-demo/weather_agent.py` (`pip install openai`).

**Adding flowproof to an existing agent?** [adopting.md](adopting.md) is
written to be handed to a coding agent: the audit to run first, the three
questions that decide everything, and the order to do it in.

Next: [agent-testing.md](../agent-testing/index.md) for the full assertion grammar,
the MCP tool boundary, and egress containment.

## Walkthrough: a UI flow (Windows Calculator)

The same record-once/replay-deterministically idea, applied to a desktop
app. This drives Windows Calculator to compute **5 + 3 = 8**.

Requirements: **Windows 10/11** with the Calculator app.

## 1. Write a spec

`calc.flow.yaml` (also in [`examples/calc.flow.yaml`](../examples/calc.flow.yaml)):

```yaml
name: Add two numbers
app: calc
steps:
  - Type 5
  - Press plus
  - Type 3
  - Press equals
  - assert: display shows 8
```

## 2. Record

```powershell
flowproof record calc.flow.yaml
```

flowproof launches Calculator, resolves every step to a real UI Automation
element, **actually performs the flow** (you'll see the buttons pressed),
verifies the assertion against the live display, and writes
`calc.trace.jsonl`: one JSON step per line, human-diffable:

```text
Recorded 'Add two numbers': 5 steps -> calc.trace.jsonl
```

That filename is a convention, not a lookup: `record` derives
`<name>.trace.jsonl` from the spec, and `run` and `heal` derive the same one
when you do not say otherwise. Override it when a trace should not sit next
to its spec: `--out` chooses where `record` writes, `--trace` tells `run`
and `heal` where to read:

```powershell
flowproof record calc.flow.yaml --out traces/calc.trace.jsonl
flowproof run   calc.flow.yaml --trace traces/calc.trace.jsonl
flowproof heal  calc.flow.yaml --trace traces/calc.trace.jsonl
```

Keep the pair together unless you have a reason not to. A suite run resolves
every spec's trace by the convention and **ignores `--trace`**, so a
relocated trace reads to `run <dir>` as a flow that was never recorded,
skipped by default, or a hard error under `--strict`.

## 3. Replay

```powershell
flowproof run calc.flow.yaml
```

Replay is deterministic: it re-resolves the recorded selectors, presses the
same buttons, and evaluates the assertion by reading the display.

```text
  [PASS] s0001 Type 5
  [PASS] s0002 Press plus
  [PASS] s0003 Type 3
  [PASS] s0004 Press equals
  [PASS] s0005 display shows 8
PASS: Add two numbers (2154 ms) -> .flowproof\runs\20260718T120000.000Z\report.html
```

The path is the one worth opening: `report.html` is the human rendering, and
on a headless adapter it is the only way to *see* what the run did. Exit
codes: `0` pass, `1` test failure, `2` error. Each run writes a
self-contained bundle under `.flowproof/runs/<timestamp>/`: `result.json`
(the machine surface, including the step→time mapping), `report.html`
(with a step-synchronized frame viewer: click any step to see exactly
what happened), `junit.xml` (one testcase per step, for Jenkins / GitLab /
Azure DevOps / any CI that ingests JUnit; point your test-report collector
at `.flowproof/runs/*/junit.xml`), and `recording/` with the captured
keyframes. Pass `--video` to additionally create `recording.gif`, the whole
run as one animation, paced like the real execution, embedded at the top of
the report. Sensitive
regions are masked before frames are written: declare `redact:` rules in
the spec, and password fields are always masked automatically
(see [docs/recording.md](recording.md)).

For faster runs, GIF/video assembly is off by default while screenshots remain
available. Use `--video` to opt in. `--recording-detail low` captures only the
initial state, every fifth step, and the final state; `--recording-detail off`
disables screenshots and video entirely. The flags work on both `record` and
`run`, and affect artifacts rather than execution or verdicts. Add
`--highlight-cursor` when reviewers should see a synthetic cursor and bright
click halo at each pointer action; drag steps highlight both their source and
destination.

**When a step fails**, the bundle additionally answers the first two
questions a human asks. `debug/dom.html` is the full DOM at the moment of
failure and `debug/console.log` the page's recent console/exception tail
(web flows; captured best-effort). And when an anchored element wasn't
found, the failure detail suggests the nearest visible text anchors:
`element … not found, did you mean 'Save changes'?`, usually the whole
diagnosis for drifted labels, with `flowproof heal` as the fix.

**Verify the recording reproduces itself.** A recording is a claim that
the flow can be performed again from the trace alone, and nothing checks
that claim: authoring succeeds when the live app happens to cooperate, so
a target that was merely reachable at that moment (a button under a
rotating carousel, a field beneath a datepicker that hadn't opened yet)
gets written down as though it always would be. The first person to learn
otherwise is whoever runs the suite.

```bash
flowproof record shop.flow.yaml --verify
```

`--verify` replays the new trace once, immediately, and refuses the
recording if it cannot reproduce itself: the trace is kept as evidence for
`flowproof heal`, and the command exits non-zero saying so. It is opt-in
rather than the default because it **performs the flow a second time**
against the live application, repeating whatever that flow does: orders,
e-mails, payments. Turn it on for a flow whose steps are safe to repeat,
and leave it off for one that isn't.

**Incremental re-record.** When the app changes, don't re-record the
flow, re-record the step: `flowproof record calc.flow.yaml --reuse`
walks the spec against the existing trace and reuses every old step
whose intent still matches and whose target still resolves on the live
app, verbatim (same selectors, zero rules/model work). Only drifted or
new steps are authored fresh; for model-authored steps that means the
model is consulted **only** for the drift. The summary reports the
split: `Recorded 'Flow': 12 steps (11 reused)`.

**Autonomous repair.** When a `record` step fails and an authoring model is
configured (`flowproof config ai`), `record` does not just stop and report
the failure. It diagnoses the failure, asks the model for a minimal edit to
the failing step in the `.flow.yaml`, applies that edit directly, and reruns,
up to 3 attempts, or fewer if the same failure recurs with no progress
(treated as a Flowproof limitation rather than a fixable flow, not
something more patching can solve). The only file this ever touches is the
`.flow.yaml` being recorded; it never edits Flowproof's own code. A
`<flow>.repair.json` report next to the trace records every attempt, what
changed, and why. Pass `--no-repair` to disable this and get the original
behavior: stop and report the first failure immediately.

When the model concludes a failure is a Flowproof limitation rather than a
fixable flow (an "engine gap"), that verdict is treated as provisional, not
final: live evidence at the moment of failure can be ambiguous (a target
that briefly reads as empty text, for example), so `record` gives the whole
flow one independent, fresh attempt (a new driver session, from the top)
before reporting a hard failure. If the fresh attempt passes outright, the
first failure was a one-off; if it fails again and repair finds a real fix,
that's used; if it fails the same way again, both failures are recorded in
`<flow>.repair.json` as agreeing evidence of a genuine problem. A
budget-exhausted verdict (repair genuinely tried several real fixes) does
not get this free retry, only a verdict that never really tried a fix at
all.

```bash
flowproof record shop.flow.yaml --no-repair
```

**Actionability.** Element actions don't fire on an element that merely
exists: replay gates every click/type on **enabled** (not
`disabled`/`aria-disabled`), **stable** (bounding box settled, no
mid-animation clicks), and **receives events** (a click at its center
actually reaches it, not a toast or modal backdrop), polling within the
step's auto-wait bound. A gate that never clears fails with its name:
`element exists but is disabled after 5000ms`, so a flake is a
diagnosis, not a mystery.

### Running a whole suite

Point `run` at a **directory** and every `*.flow.yaml` under it (recursive,
sorted, `.flowproof` artifact dirs skipped) replays as one suite: a failing
flow doesn't stop the rest, each flow keeps its own run bundle, and a merged
`<dir>/.flowproof/suite-junit.xml` (one `<testsuite>` per flow) is what CI
ingests. Exit code is non-zero if ANY flow failed:

```bash
flowproof run specs/
flowproof run specs/ --retries 2      # re-run a flow that fails, up to twice
```

**Dev servers with file watchers.** flowproof writes each run bundle to
`.flowproof/runs/…` inside the project, next to the spec it came from. A dev
server watching that tree (vite, webpack-dev-server, nodemon) sees those
files appear and reloads the app **mid-run**, which can fail a flow for
reasons that have nothing to do with the app. Exclude the artifacts from the
watcher: in vite that is `server.watch.ignored: ["**/.flowproof/**"]`, plus
any directory your app writes to during a test (a JSON-file database, an
upload folder).

Deterministic replay is stable, but the infrastructure under it (a dropped
CDP frame, a momentarily slow backend) is not; `--retries N` re-runs a
failed flow up to N more times with a fresh driver before calling it
failed. The web adapter reuses **one headless browser** across the whole suite
(an isolated context per flow), so the cold start is paid once, not per flow;
set `FLOWPROOF_NO_SHARED_BROWSER=1` to force a browser per flow. A headed run
is private automatically: its visible browser is maximized and closes with the
flow instead of leaving the shared keep-alive window on the desktop.

**Suite manifest.** A suite usually needs sequencing a bespoke harness
would otherwise provide: shared env, seed before each flow, cleanup after.
Declare it in an optional `suite.yaml` next to the specs instead:

```yaml
# specs/suite.yaml
env:
  DM_BASE_URL: http://localhost:3000
  DM_SESSION_COOKIE: ${DM_SESSION_COOKIE}   # re-map / compose ambient vars
before_each: pnpm --filter app exec tsx seed.ts   # $FLOWPROOF_SPEC = the spec path
after_each: pnpm --filter app exec tsx cleanup.ts
order:                                       # optional; unlisted specs run after, sorted
  - smoke/login.flow.yaml
```

`env` is exported to every flow and hook; `before_each`/`after_each` run
via `sh -c` with the current spec path in `$FLOWPROOF_SPEC`. A hook that
exits non-zero aborts the suite; silent seed/cleanup failure is exactly
the fragility to avoid.

**Minted test data: `env_from`.** Hooks are for *effects*; their stdout
is not captured. When flows need values an external CLI mints (DataMaker
picking a valid Material/Supplier/Plant out of SAP), declare a data
command instead:

```yaml
env_from: datamaker sap info-record pick --plant 1010 --format env
```

It runs once before any flow (via `sh -c`, from the suite directory); its
stdout must be `KEY=VALUE` lines (`#` comments and blank lines allowed)
which become env vars for every flow and hook, reachable from specs as
`${VAR}`. It fails closed: a non-zero exit or a malformed line aborts the
run, and the command's stderr is echoed either way, so a mint script that
explains itself is heard.

The command **runs with the suite's `env:` visible**: minting test data
almost always needs the suite's own base URL and credentials. Each `env:`
entry is resolved against the process environment for this purpose; an
entry that cannot resolve yet is simply not passed (it may reference this
command's own output, and it gets its turn afterwards). Two orderings are
easy to conflate and only the first changed: what the *command sees* now
includes `env:`, while `${VAR}` precedence *in flows* is unchanged:
process env, then `env_from` output, then `env:`.

Suite context follows single flows too: `record` and single-spec `run`
discover the nearest `suite.yaml` walking up from the spec (nearest wins;
the chosen manifest is named on stderr), so a flow behaves the same alone
as inside its suite, including at record time, when `${VAR}`s must
already resolve. Note the trust model: running a spec executes the
`env_from`/hooks of the suite it belongs to, same as running the suite.
See [self-help.md](self-help.md) for the authoring loop this enables.

More suite machinery, all from the first external adoption:

- **`min_version: "X.Y.Z"`** in `suite.yaml`: the engine refuses to run
  when older than the suite demands, naming both versions. Set it when
  specs use vocabulary an older flowproof would have mishandled (before
  0.2.2, unknown spec fields were silently ignored; now they are parse
  errors).
- **Missing traces skip, not abort**: a committed spec whose trace was
  never recorded reports as junit `skipped` with the reason, instead of
  hard-failing everyone's suite run. `--record-missing` records it in
  place first; `--strict` restores the hard error for CI that must not
  let coverage silently shrink. `run`'s own `--author <auto|rules|llm>`
  only has an effect together with `--record-missing`: it picks the
  authoring backend for whatever gets recorded, same flag and default as
  `record`'s; on a spec that already has a trace, `run --author` is a
  no-op.
- **Suite `env:` is lazy per entry**: an unresolvable value warns and is
  skipped instead of blocking flows that never reference it. A flow that
  DOES reference it still fails at the moment of use, naming the
  variable. (`env_from` stays fail-closed: a data command failing is
  never ignorable.)
- **`skip_unless_env: [FLAG]`** on a spec: first-class env-flag gating,
  reported as junit `skipped` with the reason instead of an invisible
  bash guard. Checked after suite env applies, so `suite.yaml` can
  satisfy the gate; a gated flow skips even under `--strict`.

Programmatic callers invoking the CLI should pass `--json`: the full
structured report prints to stdout instead of the human-readable lines;
never parse the prose output.

```powershell
flowproof run calc.flow.yaml --json
```
