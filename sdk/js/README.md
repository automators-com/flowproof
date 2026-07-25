# flowproof

Run your AI agent once, keep the recording, and assert against it from then
on. flowproof captures the run at the model boundary - every request and
every tool-call decision - and serves it back on later runs, so replay makes
**zero LLM calls**. You assert which tools were called, with which arguments,
in which order, and which were not. The same engine drives web, desktop and
Citrix.

This package ships the `flowproof` CLI as platform-native binaries
(linux-x64, darwin-x64/arm64, win32-x64) — no Python required:

```bash
npx flowproof --version
npx flowproof record my.flow.yaml
npx flowproof run specs/
```

The Python SDK (`pip install flowproof`) remains the primary SDK and
adds the programmatic API and MCP server. Docs and source:
[github.com/automators-com/flowproof](https://github.com/automators-com/flowproof)
