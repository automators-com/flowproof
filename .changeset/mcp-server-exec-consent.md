**`flowproof mcp` never runs a flow's own commands without the call asking.**
A coding agent pointed at a repository it just cloned is not an embedder
running its own trusted suite, but the Python `flowproof-mcp` server treated it
as one and allowed suite shell, a spec's `agent.command` and a Windows launch
command unconditionally. The native server refuses them by default, as the CLI
does, and returns a result that names the command and the file it came from.
A call runs them only with `allow_exec: true`. A `FLOWPROOF_ALLOW_EXEC` the
client inherited is removed, so consent is always given per call.
