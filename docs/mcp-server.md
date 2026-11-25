---
title: "flowproof as an MCP server"
description: "Let a coding agent replay your flows itself: Claude Code, Codex CLI, Cursor and VS Code start `flowproof mcp` as one of their tool servers."
---

`flowproof mcp` is flowproof as **your coding agent's tool server**: Claude
Code, Codex CLI, Cursor, VS Code or any MCP client starts it over stdio and
records, replays and repairs flows itself, and can check its own setup. It is not a flow's `mcp:` block, where flowproof
**stands in for the tool server of an agent you are testing** (see
[agent testing](agent-testing/index.md)).

Register it with your client. Most take an entry like the one below, but the
file and key differ, so check the client's documentation. The server runs in
the folder the client was started in.

```json
{ "mcpServers": { "flowproof": { "command": "flowproof", "args": ["mcp"] } } }
```

Under the Python package, `flowproof` runs the engine inside Python and the
server cannot spawn it. Set `FLOWPROOF_MCP_EXE` to a `flowproof` binary from
npm or `cargo install` instead.

## Tools

**`flowproof_run`** replays a flow, or every flow under a directory, as
`flowproof run --json` does, with no model calls. It takes `spec` (required),
`trace`, `vars`, `var` (an object of business-data `KEY: VALUE` overrides,
never secrets), `from`, `until`, `retries`, `headed` (headless otherwise,
whatever `FLOWPROOF_HEADED` says) and `cwd`, the folder to run in for this
call only. Every result echoes the `cwd` it used. A failing flow is a result
with `passed: false` and its report; only an engine error, such as a spec that
does not parse, is marked as an error.

**`flowproof_record`** performs a flow once against the live app and writes its
trace, as `flowproof record --json` does, so it does whatever the flow does: use
test accounts. It takes `spec`, `out`, `vars`, `var`, `author`, `reuse`,
`resume`, `no_repair`, `headed`, `cwd` and `allow_exec`. Plain steps are authored by the model
set with `flowproof config ai`, falling back to grammar without one. A step
that cannot be authored comes back with `needs_clarification`, the payload
described in [the outside-in loop](self-help.md). That is a result, not an
error: the agent rewords the step and records again. Its scene inventory is
screen content, and it goes to whichever model your MCP client uses.

**`flowproof_heal`** re-drives a flow whose app has drifted and writes a
`.proposed.jsonl` beside the trace, as `flowproof heal --json` does; the trace
itself is left alone. It takes `spec`, `trace`, `vars`, `var`, `author`,
`headed`, `from_run` and `step`, plus `apply` and `apply_proposal`, which
overwrite the trace. Those two are refused unless the call also passes
`reviewed: true`, which the agent should send only after you have reviewed the
proposal and said to apply it.

**`flowproof_doctor`** takes `check`: `ai` (is an authoring model configured)
or `sap` (can SAP GUI Scripting be reached). A failed check is a result with the
fix in its message. `fiori` and `agent` are refused with the reason: `doctor
--fiori` logs in to a live system for real, and `doctor --agent` runs a command
the caller supplies, so a person runs those.

**`flowproof_audit`** returns the control-coverage report for a suite
directory, `dir`, from the run record `flowproof_run` wrote, with `run` or
`since` as on the CLI.

`flowproof_run` and `flowproof_record` take `allow_exec`, off by default. Without it, a flow whose
`suite.yaml` hooks, `agent.command` or Windows launch command would run a
program is refused, and the result names that command, the file it came from
and a `consent` note. The agent should show you the command and pass `allow_exec:
true` only if you agree. An ambient `FLOWPROOF_ALLOW_EXEC` does not count: the
server removes it from every child, so consent is always per call.

## Progress and cancelling

A recording can take minutes. A client that sends a progress token with a call
gets the engine's progress lines as `notifications/progress`, as they are
printed: the same lines `flowproof record` shows in a terminal.

Cancelling a call stops it. The engine is asked to close its browser and exit,
then the server stops everything it started, such as a browser or a suite
shell. On Windows only the engine itself is stopped. A cancelled call gets no
result. When the client closes the server, calls still running are cancelled
the same way.

## What there is no tool for

There are no file tools, because your agent has better ones and traces are
[plain JSONL](trace-format.md), and no configuration tools: credentials and the
authoring model are set by a person with `flowproof config`, so they never
reach a model through a tool call.
