**The Python MCP server is archived, and a native one is coming to replace
it.** `flowproof-mcp` was the only way to drive flowproof from a coding agent,
and reaching it meant a Python install plus `pip install flowproof[mcp]` — for
four tools over a binary that every other surface gets at directly. Nothing was
using it. It still works, unchanged, and its stdio test still runs in CI; the
module is now named `_archived_mcp_server.py` so nobody builds on it while a
`flowproof mcp` subcommand is built into the engine itself. If you are using
`flowproof-mcp`, say so before it is removed.
