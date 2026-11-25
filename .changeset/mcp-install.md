**`flowproof mcp install` registers the server with your coding agent.** Run
it in a project and it adds `flowproof mcp` to `.mcp.json` (Claude Code and
GitHub Copilot), and to `.cursor/mcp.json` or `.gemini/settings.json` when the
project has those folders. Other servers in the file are kept, an existing
different entry is replaced only with `--force`, and `--print` shows the entry
for any other client.
