# CLAUDE.md

Guidance for Claude Code (claude.ai/code) working in this repository.

**If you are an autonomous loop, read [`CHARTER.md`](CHARTER.md) first.** It is
the direction, the invariants, and the out-of-scope list, and it outranks
anything here. Your role prompt is in `scripts/loop/roles/`. This file is
context; the charter is authority.

## What flowproof is

Test an AI agent the way you test everything else: run it once, keep the
recording, assert against it from then on.

flowproof sits at the model boundary. It captures a real run — every model
request and every tool-call decision — into a **trace**, then serves that
recording back on later runs. **Replay makes zero LLM calls**, so a suite that
cost money per commit and flaked on sampling becomes free and repeatable.

What is asserted is behaviour, not prose: which tools were called, with which
arguments, in which order, and which were **not**.

## Repository layout

Rust workspace. All crates move together on one version.

| Crate | What it is |
|---|---|
| `flowproof-trace` | trace format, selector ladder, trace-to-script compiler |
| `flowproof-replay` | deterministic executor — replays traces with zero LLM calls |
| `flowproof-driver` | native driver: screen capture, input injection, UI Automation |
| `flowproof-agent` | recording agent: planner loop, pluggable model backends |
| `flowproof-adapters` | SAP GUI Scripting COM, browser via CDP (behind feature flags) |
| `flowproof-cli` | the `flowproof` binary: `record`, `run`, `heal` |
| `flowproof-python` | PyO3 bindings — the `flowproof._native` extension module |

Also: `sdk/python` (Python SDK, `uv`/ruff/pytest), `sdk/js` (placeholder),
`docs/` (design + format), `examples/`, `tests/flowproof/` (flows exercising
flowproof itself), `scripts/gate/` and `scripts/loop/` (the autonomous-loop
constitution — see below).

**The driver is Windows-native, but the workspace must always build on Linux and
macOS via the stub backend.** Assume your change will be compiled on Linux.

## Commands

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings   # -D warnings is the CI gate
cargo test --workspace
```

Python SDK (`sdk/python`, `uv` recommended): `ruff check` and `pytest`.

A green run needing no API key — the committed cassette replays for free:

```bash
flowproof run scripts/demo/order-status.flow.yaml
```

That demo's agent needs its own SDK (`pip install openai`); see
`scripts/demo/README.md`. If the agent process dies before its first model call,
flowproof now names that as its own failure and quotes the tail of the agent's
stderr — so a missing dependency reads as a missing dependency rather than as a
replay that could not reproduce.

**Never pipe a verification command.** `cargo build | tail` returns *tail's*
exit code, so a failed build reports success. This has produced several false
results here. Redirect to a file and read the status, or use `${PIPESTATUS[0]}`.

## Things that will fail CI if you get them wrong

- **Trace format changes must update `docs/trace-format.md` *and*
  `crates/flowproof-trace/schema/` in the same commit.** Enforced by the
  `adversary` job's ratchets, not just by convention.
- **Versions move together** — `Cargo.toml`, the Python wheel, the npm package,
  and three more locations. The `versions agree` job checks all six. Never bump
  one alone.
- **Don't delete a test, add `#[ignore]`, or add a `pytest` skip/xfail.** The
  ratchets refuse a change that leaves the suite smaller or quieter than it found
  it. Silencing a test is not fixing it.
- **Don't modify a committed `*.trace.jsonl`.** Adding a cassette is normal work;
  rewriting one silently redefines what correct means. It is human-only.

## Conventions

- **Conventional Commits** with crate scope where it helps: `fix(agent): …`,
  `feat(trace): …`, `docs: …`.
- **The CHANGELOG explains why, not what.** The voice is distinctive: it names
  what was wrong, why it mattered, and what holds now. Read the last few entries
  before writing one — do not produce a list of changes.
- **A fix ships with the test that proves it stays fixed.** This is a testing
  tool; a fix without a test is an assertion.
- **Prose describing code that no longer exists is a defect.** If you change
  behaviour, find the docs that described the old behaviour.
- Keep pull requests small. Over ~400 changed lines the ratchets refuse it, on
  the grounds that review stops working at that size.

## CI

Jobs are gated on a `what changed` filter, so a docs-only change costs one
six-second job instead of five. Windows and the E2E suites are off the pull-request
path (~50 minutes); add the `full-ci` label to run them, and a nightly scheduled
run is the backstop.

Seven required checks, including `constitution` and `adversary`.

## The autonomous loops

This repository is developed partly by autonomous loops. Files that *constrain*
them are the constitution and **cannot be modified by a loop**:

```
CHARTER.md  CODEOWNERS  scripts/gate/  scripts/loop/  .github/workflows/  CLAUDE.md
```

`scripts/gate/constitution-check.sh` refuses a pull request from a non-human
author that touches any of them, and it fails closed — an unrecognised identity
is treated as a loop. `CLAUDE.md` is in the set because it is loaded into every
session's context: a loop that could edit it would be rewriting the operating
context of every future loop.

If a change genuinely needs one of these paths, a human opens it.
