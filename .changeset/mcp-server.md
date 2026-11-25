**A coding agent can replay your flows itself with `flowproof mcp`.** The only
way for Claude Code, Codex CLI, Cursor or VS Code to drive flowproof was the
Python `flowproof-mcp` server, behind a pip extra that most installs never had.
`flowproof mcp` is a stdio MCP server in the binary you already have. Its first
tool, `flowproof_run`, replays a flow or a suite and gives back a failing flow
as a result to read, not as an error. Each call can name the folder to run in
without moving the server. Every value is passed as `--flag=value` and the spec
comes after `--`, so a tool argument can never turn into a flag such as
`--allow-exec`. Record, heal, doctor and audit follow.
