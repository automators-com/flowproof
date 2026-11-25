**`flowproof mcp install --claude-desktop` registers the server with the
Claude desktop app.** The app reads one `claude_desktop_config.json` per user,
not a project file, so `flowproof mcp install` could not set it up. The new
flag adds the `flowproof` entry there and keeps the app's other settings. The
app starts local servers in `/`, with no project, so a call with a relative
path and no `cwd` now comes back with a hint to pass the project's folder, and
a leading `~` in `cwd` or a flow's path now means the home folder: in a real
chat the agent's first call passed `cwd: "~/…"` and failed.
