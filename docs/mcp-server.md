---
title: "flowproof as an MCP server"
description: "Let a coding agent replay your flows itself: Claude Code, Codex CLI, Cursor and VS Code start `flowproof mcp` as one of their tool servers."
---

`flowproof mcp` is flowproof as **your coding agent's tool server**: Claude
Code, Codex CLI, Cursor, VS Code or any MCP client starts it over stdio and
replays flows itself. It is not a flow's `mcp:` block, where flowproof
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

There are no file tools, because your agent has better ones and traces are
[plain JSONL](trace-format.md), and no configuration tools: credentials and the
authoring model are set by a person with `flowproof config`, so they never
reach a model through a tool call.
