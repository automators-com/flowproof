---
title: "Install Flowproof"
description: "Install Flowproof, verify the CLI, and choose the guide for the system you need to test."
---

Flowproof records a natural-language YAML flow once, then replays the resulting
trace deterministically with zero LLM calls.

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

Confirm the installation:

```bash
flowproof --version
```

## Choose what to test

| Goal | Start here |
| --- | --- |
| Record and replay a first agent or UI flow | [Record and replay](record-and-replay.md) |
| Test an AI agent's model and tool behavior | [Agent flows](agent-flows.md) |
| Drive a browser | [Web flows](web-flows.md) |
| Test HTTP or SQL without a UI | [API-only flows](api-flows.md) |
| Drive Windows, SAP GUI, or Citrix | [Live application tests](live-app-tests.md) |
| Run several flows with shared setup | [Run a suite](suite-runs.md) |
| Configure credentials and model access | [Secrets and configuration](secrets-and-config.md) |

## Build from source

You need Rust and maturin. Run `pip install .` from `sdk/python` to compile
and install the engine.

## Control update notices

Every command checks at most once a day (cached) whether
a newer release exists, and prints a one-line notice to stderr if so, never
to stdout, so `--json` output and scripted use stay clean. Set
`FLOWPROOF_NO_UPDATE_CHECK` (any value) to disable it entirely, e.g. for CI or
an air-gapped machine.
