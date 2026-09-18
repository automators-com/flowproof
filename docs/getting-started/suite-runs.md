---
title: "Run a suite"
description: "Run a directory of flows with shared configuration, dependencies, test data, retries, and resumable checkpoints."
---

Use a suite when several flows share configuration or must run in a controlled
order. A directory run executes every selected `*.flow.yaml`, keeps a run bundle
for each flow, and writes merged JUnit output for CI.

## Run every flow in a directory

```bash
flowproof run specs/
```

Flowproof discovers specs recursively, sorts them, and skips `.flowproof`
artifact directories. One failed flow does not stop the others by default. The
suite exits nonzero if any flow fails and writes
`specs/.flowproof/suite-junit.xml` with one `<testsuite>` per flow.

Missing traces are reported as skipped. Use `--strict` when CI must fail on a
missing trace, or `--record-missing` to record missing traces before running:

```bash
flowproof run specs/ --strict
flowproof run specs/ --record-missing --author auto
```

`--author` affects only traces created by `--record-missing`. It has no effect
on a flow that already has a trace.

## Configure retries and the browser

Retry infrastructure failures with a fresh driver:

```bash
flowproof run specs/ --retries 2
```

Retries can repeat the flow's external effects. Use them only where rerunning
the complete flow is safe.

Web suites reuse one headless browser with an isolated context for each flow.
Set `FLOWPROOF_NO_SHARED_BROWSER=1` to launch a browser per flow. Headed runs do
not share the browser.

Flowproof writes artifacts below `.flowproof/runs/`. Exclude that directory
from file watchers so a dev server does not reload during a flow. For Vite:

```js
server: {
  watch: { ignored: ["**/.flowproof/**"] }
}
```

Also exclude directories that the application itself writes during tests.

## Add shared environment and hooks

Place `suite.yaml` beside the suite's flow files:

```yaml
# specs/suite.yaml
env:
  DM_BASE_URL: http://localhost:3000
  DM_SESSION_COOKIE: ${DM_SESSION_COOKIE}
before_each: pnpm --filter app exec tsx seed.ts
after_each: pnpm --filter app exec tsx cleanup.ts
order:
  - smoke/login.flow.yaml
```

`env` is available to every flow and hook. Hooks run through `sh -c` from the
suite directory, with the current spec path in `$FLOWPROOF_SPEC`. A nonzero hook
exit errors that flow.

`order` places named flows first and then runs every unlisted flow in sorted
order. Use `flows` when unlisted operations must not run.

Running or recording one spec also discovers the nearest `suite.yaml` by
walking up from the spec. The selected manifest is named on stderr. Running a
single spec can therefore execute that suite's `env_from` command and hooks.
Treat a suite manifest as executable project configuration.

## Select flows and enforce dependencies

Use `flows` as an explicit allowlist and execution order:

```yaml
flows:
  - create-order.flow.yaml
  - receive-order.flow.yaml
  - invoice-order.flow.yaml
depends_on:
  receive-order.flow.yaml: [create-order.flow.yaml]
  invoice-order.flow.yaml: [receive-order.flow.yaml]
stop_on_failure: true
```

`flows` cannot be combined with `order`. Every path must name a unique file
inside the suite. A dependency must be selected and appear earlier in the
list. Invalid names, exclusions, duplicates, and cycles fail before hooks or
flows run.

A failed, errored, or skipped prerequisite skips its dependents and makes the
suite fail. `stop_on_failure: true` also skips independent remaining flows
after a failure or error. These policies apply only to directory runs; running
one spec directly remains an explicit standalone operation.

## Mint test data with `env_from`

Use `env_from` when an external command must select valid business data:

```yaml
env_from: datamaker sap info-record pick --plant 1010 --format env
```

The command runs once before the suite. Its stdout must contain `KEY=VALUE`
lines, blank lines, or `#` comments. A nonzero exit or malformed line aborts the
run. Stderr is always displayed so the command can explain a failure.

The command receives resolvable values from the suite's `env:` block. Values it
returns are then available to flows and hooks as `${VAR}`. Flow input precedence
remains process environment, then `env_from`, then suite `env:`.

Hooks are for effects; their stdout is not captured. Use `env_from` for values a
flow must consume. See [The outside-in loop](../self-help.md) for an example of
using live system data during authoring.

## Pause and resume a business workflow

For a selected workflow with `stop_on_failure: true`, create a new checkpoint
for each intended business run:

```bash
flowproof run specs --vars inputs.values.yaml \
  --checkpoint ./j45-progress.json --stop-after create-order.flow.yaml
flowproof run specs --vars inputs.values.yaml \
  --checkpoint ./j45-progress.json --resume
```

The first command runs through the named stage and pauses. Pending stages are
reported as skipped, and the incomplete suite exits nonzero. `--resume`
restores the exports from confirmed stages and runs only stages that have not
started. A completed checkpoint performs no target operations when resumed
again.

Before an operation starts, Flowproof writes and syncs its claim. Only a passing
report with all declared exports clears that claim. If a process stops during
an operation, resume remains blocked because external writes may already have
happened. Reconcile the business system and run evidence before preparing a new
reviewed continuation. Flowproof does not automatically retry or mark an
uncertain stage successful.

A crash can leave `<checkpoint>.lock`. Remove the lock only after verifying that
its process has stopped. Removing the lock does not clear the uncertain
operation.

Checkpoint recovery pins the suite directory, suite/spec/trace/value bytes,
parsed specs, engine binary, referenced inputs, and adapter environment. It
rechecks business inputs at every stage boundary. Exports need unique producers,
and consumers must declare the producer as a prerequisite. Suite environment
and value files cannot override exports.

Checkpoint mode rejects features that can make recovery ambiguous:

- retries and `--record-missing`;
- trace overrides;
- `env_from` and shell hooks;
- external launch commands;
- agent and multi-surface flows; and
- missing traces or disabled gates.

Prepare checkpoint inputs separately with `--vars`.

The checkpoint contains resolved business identifiers and previous reports.
Keep it outside version control and shared CI artifacts. Unix checkpoint files
use mode `0600`. The checksum detects corruption, not tampering by someone who
can rewrite the file. Keep credentials out of `exports`.

Checkpoints provide conservative orchestration, not business-level
idempotency. Recording, polling inside a step, another checkpoint, or a
standalone flow can still repeat an external operation.

## Other suite controls

| Control | Behavior |
| --- | --- |
| `min_version: "X.Y.Z"` | Refuses to run with an older Flowproof version |
| `skip_unless_env: [FLAG]` | Reports the flow as skipped unless the environment variable exists |
| Lazy suite `env:` | Warns and skips an unresolved entry until a flow actually needs it |

`env_from` is not lazy. A failed data command always aborts the suite.

## Consume suite output programmatically

Pass `--json` when another program invokes the CLI. The full structured report
goes to stdout. Completed-step progress continues on stderr, so callers can
display progress without parsing human-readable output.
