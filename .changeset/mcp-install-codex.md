**`flowproof mcp install --codex` registers the server with Codex.** Codex
takes its servers as TOML in `~/.codex/config.toml`, so `mcp install` only
printed an entry to paste. `--codex` now writes the `[mcp_servers.flowproof]`
table there itself, or in `$CODEX_HOME`. The edit keeps your comments, your
other servers and settings, and any keys you added to the table. It takes the
same `--command`, `--force` and `--remove` as `--claude-desktop`, so Flowproof
Desktop can connect Codex to the engine it bundles.
