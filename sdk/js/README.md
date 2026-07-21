# flowproof

A generic open-source automation framework for the AI-agent era:
automated testing and agentic process automation across web, desktop,
and Citrix. Agents author flows from natural-language intent; a
deterministic engine executes them.

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
