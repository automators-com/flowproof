---
title: "Installation"
description: "Install flowproof and run the agent quickstart: record a flow once, then replay it with zero LLM calls."
---

flowproof records a flow once from a natural-language YAML spec, then replays
it deterministically - **zero LLM calls at replay time**.

Start with the agent quickstart below. The UI walkthrough after it is the
same idea applied to a desktop app, and needs Windows.

## Install

Either package ships the same engine as a native binary. Pick whichever
matches the project you are testing:

```bash
npx flowproof --version      # no install, no Python
```

```bash
npm install --save-dev flowproof
```

```bash
pip install flowproof
```

The npm package resolves a platform binary for linux-x64, darwin-x64,
darwin-arm64 and win32-x64. On any other platform install from PyPI instead;
`npx flowproof` will say so rather than fail obscurely.

Building from source instead? You need Rust and maturin: `pip install .`
from `sdk/python` compiles the engine automatically.

**Staying current.** Every command checks (at most once a day, cached) whether
a newer release exists, and prints a one-line notice to stderr if so, never
to stdout, so `--json` output and scripted use stay clean. Set
`FLOWPROOF_NO_UPDATE_CHECK` (any value) to disable it entirely, e.g. for CI or
an air-gapped machine.
