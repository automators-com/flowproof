**`flowproof mcp install` also installs a `flowproof-flows` skill.** The MCP
tools say what they do; the skill tells a coding agent when to use them:
record a new flow, replay to check one, heal when the app has changed. It
also says that a failing run is a report to read, and that two decisions are
yours: applying a heal proposal (`reviewed: true`) and letting a flow run its
own commands (`allow_exec: true`). It is written to `.claude/skills/` and
`.agents/skills/` beside the server's config entry.
