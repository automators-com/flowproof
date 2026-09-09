---
status: done
---
# Plan 10 — post-publish registry smoke test for an exact release version

Issue: [#382](https://github.com/automators-com/flowproof/issues/382), split from #377.
`needs-human` throughout — this plan adds a workflow under `.github/workflows/`,
a constitution-protected path (`CLAUDE.md`, "The autonomous loops"). A human
opens and merges the PR; an agent may draft it under direct supervision.

## The gap this closes

`cargo test` and the existing CI prove the *engine*. Nothing in the repo today
proves that the two things a real user actually types —
`pip install flowproof==<version>` and `npx flowproof@<version>` — work
against what is *actually sitting on PyPI and npm* right now, on a machine
with no checkout and no local cache. A release can go out with a wheel that
fails to import on one platform, or an npm optional-dependency that silently
didn't resolve, and nothing today would catch it before the release is
announced.

Two things already exist and this plan must extend, not duplicate:

- **`.github/workflows/npx-smoke.yml`** — already installs the published npm
  package with no checkout, on `ubuntu-latest` / `macos-latest` /
  `windows-latest`, and already runs daily plus on manual dispatch with a
  free-text `version` input (defaulting to `latest`). `macos-latest` on
  GitHub-hosted runners is Apple Silicon today, so this already covers
  **macOS ARM64** for npm — the issue's macOS ARM64 requirement is not a new
  runner, it's reusing this one under a stricter contract (see below).
- **`.github/workflows/publish.yml`** — builds and uploads PyPI wheels for
  `ubuntu-latest` / `windows-latest` / `macos-latest` (also ARM64) at release
  time, and has a `guard` job that fails fast if the version isn't new on
  PyPI. It never installs the *published* wheel back down and runs it — that
  entire PyPI-side check is the actual gap.

## What this plan does NOT try to build fresh

- A new npm-install-and-run mechanism — reuse `npx-smoke.yml`'s steps (see
  "npm leg" below).
- A new macOS ARM64 runner — `macos-latest` already is one.
- Anything that fixes darwin-x64: it stays unexecuted in CI, as already noted
  in `npx-smoke.yml`'s own comment. This plan only makes that gap
  *documented* where a release-reader will see it, per the issue's explicit
  ask.
- Any change to which files own version numbers, or a docs-truth checker —
  out of scope per the issue.

## The new workflow: `release-smoke.yml`

One new `workflow_dispatch`-only workflow, human-triggered, never on a
schedule or a push (this runs against a specific past release, not "whatever
is latest").

### Input

```yaml
on:
  workflow_dispatch:
    inputs:
      version:
        description: "Exact released version to validate, e.g. 0.22.0 (no 'latest', no 'v' prefix)"
        required: true
```

First step of every job validates the input against `^[0-9]+\.[0-9]+\.[0-9]+$`
and fails immediately (before spinning up the matrix) if it's `latest`,
empty, or malformed. This is the acceptance criterion "`latest` is
insufficient as release evidence" — enforced, not just documented.

### Matrix and architecture assertion

```yaml
strategy:
  fail-fast: false
  matrix:
    include:
      - os: ubuntu-latest
        expect_arch: X64
      - os: windows-latest
        expect_arch: X64
      - os: macos-latest
        expect_arch: ARM64
```

Every job's first real step runs `uname -m` (bash) or
`[Environment]::Is64BitOperatingSystem`/`$env:PROCESSOR_ARCHITECTURE`
(pwsh on Windows) and asserts it matches `expect_arch`, failing loudly if
GitHub silently swaps the underlying image's CPU family. This satisfies "assert
runner architecture so image changes cannot silently weaken coverage."

Each matrix leg then runs **both** the PyPI leg and the npm leg (two channels
× three platforms = six results), so the final summary has one row per
channel per OS as the issue asks, not one per OS.

### PyPI leg (new)

Per platform, in a fresh venv, no repo checkout:

1. `python -m venv` + `pip install --no-cache-dir flowproof==<version>`.
   - `--no-cache-dir` and no `actions/checkout` step in this job are the
     "without a repository checkout or local package cache" requirement.
   - Wrap the install in a bounded retry (e.g. 3 attempts, short backoff)
     that treats "no matching distribution" as retryable only for a short
     window (PyPI index propagation lag), then fails non-zero — never falls
     back to an unpinned `pip install flowproof`. This satisfies "fail
     closed... bounded... non-zero rather than falling back to latest."
2. `python -c "from flowproof import _native; print(_native.__name__)"` —
   proves the native extension actually imports (`sdk/python/flowproof/cli.py:8`
   is the real import this mirrors).
3. `flowproof --version` — assert stdout matches exactly `flowproof <version>` (same assertion style `npx-smoke.yml` already uses for npm).
4. `flowproof --help` — assert the output names the expected top-level
   commands (`record`, `run`, at minimum; enumerate from
   `crates/flowproof-cli/src/lib.rs`'s `Command` enum at the time this is
   built, so the check doesn't silently drift from the real CLI surface).

### npm leg (reuse, not reimplement)

Factor `npx-smoke.yml`'s three existing steps ("npx --version from a clean
cache", "the resolved binary runs", "missing platform binary fails loudly")
into a composite action (e.g. `.github/actions/npm-smoke-steps/action.yml`)
parameterized on `version`. `npx-smoke.yml` calls it with `inputs.version ||
'latest'` exactly as today; `release-smoke.yml` calls the same composite
action with the required exact version. This is the "reuse or factor the
existing npm smoke behavior" criterion — one source of truth for what "npm
works" means, not two workflows that can drift apart.

Apply the same bounded-retry-then-fail-closed wrapper around the first `npx`
call here too, for npm's own propagation lag.

### Summary

Final job, `needs: [pypi, npm]` with `if: always()`, collects each matrix
leg's `outputs` (requested version, os, arch, channel, resolved version parsed
from the tool's own `--version` output, pass/fail) and writes a table to
`$GITHUB_STEP_SUMMARY`. Resolved version is deliberately re-parsed from the
tool's actual output rather than echoing the input, so a channel that silently
served a different version is visible in the table, not just in a failed
assertion.

## Documentation changes (not workflow code)

1. **Maintainer release checklist** — checked the repo: there is no existing
   release checklist anywhere (`docs/`, `CONTRIBUTING.md`, `.github/`, no
   release-issue template). This plan adds a new one:
   `internal/release-checklist.md` — `docs/` is the public-facing user docs
   site (see `docs/meta.json`'s nav), and this checklist is a maintainer-only
   operational doc, so it belongs with `internal/design.md` and the other
   contributor-facing docs instead. A short markdown list a maintainer works
   through before announcing. First item: "Run `release-smoke.yml` against
   the exact released version and confirm every leg is green." The
   darwin-x64 and website/install-path items below join it as items 2 and 3
   on the same list, so there is exactly one place a maintainer checks before
   announcing, not three scattered notes.
2. **darwin-x64 note** — one sentence, next to the existing comment in
   `npx-smoke.yml`, and mirrored in the release checklist: darwin-x64 is
   architecture-checked at publish time (`file` reports Mach-O x86_64) but
   cannot be *executed* in CI until GitHub offers a schedulable Intel macOS
   runner; verifying it today is a manual, human step.
3. **Website/GitHub install-path check** — a checklist line, not automation:
   "Confirm the website and GitHub README install commands still name a
   supported PyPI/npm invocation" — explicitly a human step per the issue's
   own scope boundary.

## Resolved questions

- **Maintainer checklist location** — resolved above: new file,
  `internal/release-checklist.md` (not `docs/`, which is the public docs
  site). No prior art existed in the repo to conflict with.
- **What "assert the expected command surface" means** — plain version: the
  workflow's `--help` check needs to know which words to `grep` for in the
  output, e.g. making sure `record` and `run` show up as commands. Rather
  than typing that list into this plan by hand (where it could go stale the
  next time a command is added or renamed), the implementer reads the actual
  list of commands straight from the source of truth —
  `crates/flowproof-cli/src/lib.rs`'s `Command` enum — at the point they
  write the workflow, and greps `--help`'s output for each one. That way the
  check is always testing against whatever commands genuinely exist at
  build time, not a list someone forgot to update.
- **Refactoring the working `npx-smoke.yml`** — confirmed okay to proceed:
  the extraction will be exercised by dispatching `release-smoke.yml` itself
  on the PR before merge, which proves the shared composite action still
  behaves exactly as `npx-smoke.yml` did, standalone, before it merges.

