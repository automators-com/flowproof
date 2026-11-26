**The MCP server's "pass `cwd`" hint now works on Windows.** A client that
starts `flowproof mcp` in a filesystem root, with no project, gets a hint to
pass the project's folder as `cwd` when a call fails. The check only knew the
Unix root `/`, so a server started in `C:\` never gave the hint. The hint now
names the folder the server is running in.
