# Releasing flowproof

A release is three artifacts cut from one commit: the Rust workspace, the PyPI
wheel, and the npm launcher plus the four native binaries it resolves. They
carry the same version number because they are one product — the launcher and
the binary it starts are a single artifact, and a wheel whose embedded engine
disagreed with its own version would be undebuggable.

**Publishing is manual on purpose.** Both registries are immutable: a version
you upload can never be replaced, only superseded. Nothing publishes on a push
or a tag; you dispatch it, having decided the number is right.

## Before you start

Pick the version. The project is pre-1.0, so features move the minor and fixes
move the patch.

Then check the number is free on both registries. This takes ten seconds and
saves a burned release:

```bash
V=0.11.0
curl -sS -o /dev/null -w 'pypi %{http_code}\n' "https://pypi.org/pypi/flowproof/$V/json"
curl -sS -o /dev/null -w 'npm  %{http_code}\n' "https://registry.npmjs.org/flowproof/$V"
```

`404` from both means the number is yours. `200` means it is already published
and you need a different one. Anything else — stop, and re-run when the index
answers; the publish guards refuse to guess for the same reason.

## 1. The version bump PR

Seven locations hold the version. Six are checked by the `versions agree` job;
the seventh is not, which is why it has drifted before.

| Location | What holds the version |
|---|---|
| `Cargo.toml` | `[workspace.package]` → `version` |
| `Cargo.lock` | all seven `flowproof-*` workspace crates |
| `sdk/python/pyproject.toml` | `version` |
| `sdk/python/flowproof/__init__.py` | `__version__` |
| `sdk/js/package.json` | `version` |
| `sdk/js/package.json` | all four `optionalDependencies` |
| `sdk/python/uv.lock` | the `flowproof` package entry — **not gated** |

Edit the first five by hand, then regenerate the two lockfiles rather than
hand-editing them:

```bash
cargo update --workspace          # Cargo.lock
(cd sdk/python && uv lock)        # uv.lock
```

Each platform package in `optionalDependencies` is pinned to the launcher's
exact version, not a range. That is deliberate: a launcher that could resolve
a different binary than the one it shipped with would make a green run mean
nothing.

### The CHANGELOG

Rename `## Unreleased` to `## <version>`, and write a short lede under the
heading saying what the release is about — the entries themselves were written
by the PRs that landed them.

**Leave a fresh empty `## Unreleased` heading above it.** This is not
cosmetic. Renaming the section without leaving one is what put two 0.10.1
fixes inside an already-published 0.10.0: the release PR took the heading
away, so git merged the later entries into a version that was immutable and
already on PyPI. A released section is a historical record and must stop
changing the moment it is published.

### Verify before you open the PR

```bash
cargo fmt --all --check
cargo build --workspace
cargo test --workspace
(cd sdk/python && ruff check && uv run pytest -q)
node --test sdk/js/test/launcher.test.js
```

The launcher test is the one people forget. It fails if `package.json`'s
platform map and `bin/flowproof.js`'s `PLATFORM_PACKAGES` drift apart, because
a mismatch there is an "unsupported platform" error on a platform that is
actually supported.

## 2. Merge

**Add the `full-ci` label before merging.** Windows and the E2E suites are off
the pull-request path — they cost about fifty minutes — and a version bump is
exactly the commit you do not want to discover them on. This is the commit
that becomes two immutable uploads.

## 3. Tag

```bash
git checkout main && git pull origin main
git tag -a "v$V" -m "flowproof $V"
git push origin "v$V"
```

Tag before dispatching, so the published artifact has a commit you can name.
This step has been skipped: the repository has `v0.3.0` and `v0.10.0` and
nothing else, so most published versions cannot be traced to a commit without
reading dates. When 0.10.0's contents came into question, that is precisely
what had to be reconstructed by hand.

## 4. Publish to PyPI

Actions → **Publish to PyPI** → Run workflow, on `main`.

Three stages:

- **`guard`** — fails in seconds if `Cargo.toml` and `pyproject.toml` disagree,
  or if the version is already on PyPI. It exists because dispatching without a
  bump ends in `400 File already exists` at the very end of a seven-minute
  wheel matrix; that burned both the 0.2.0 and 0.2.1 releases.
- **`build`** — one wheel per platform (the wheel embeds the Rust engine, so
  each builds its own), plus exactly one sdist. The Linux wheel is built inside
  a manylinux container, because PyPI rejects raw `linux_x86_64` tags.
- **`publish`** — uploads via PyPI Trusted Publishing (OIDC) in the `pypi`
  environment. There is no stored token to rotate or leak.

## 5. Publish to npm

Actions → **Publish to npm** → Run workflow.

Two stages, in order:

- **`binaries`** — builds and publishes the four platform packages:
  `@automators/flowproof-cli-{linux-x64,darwin-x64,darwin-arm64,win32-x64}`.
- **`publish`** — publishes the `flowproof` launcher, which `needs` all four.
  The launcher can never reach the registry before the binaries it resolves.

Both stages publish with `--provenance` using the `NPM_TOKEN` secret.

Three things about this job are answers to real failures, not preferences:

- **The platform names are scoped.** Unscoped ones could not be published at
  all — npm answered `403 Package name triggered spam detection` for
  `flowproof-cli-darwin-x64` and `flowproof-cli-win32-x64` on every attempt, a
  heuristic that fires on brand-new unscoped names. Two legs stuck there
  permanently, and because the launcher `needs` every leg, npm sat on its
  `0.0.1` placeholder while PyPI was already at `0.5.0`.
- **darwin-x64 is cross-compiled** from an Apple Silicon runner, not built on
  an Intel Mac. `macos-13` stopped being schedulable — a 0.6.0 publish sat
  queued on it for over two hours with no runner ever assigned.
- **`fail-fast: false`, and each leg skips if already published.** One flaky
  runner must not cancel the other three and strand a partial publish. Re-run
  the same workflow and it converges on whatever is still missing.

## 6. Verify the published artifacts

Do not take a green publish as proof. `cargo test` proves the engine, not the
package a newcomer installs.

```bash
pip install "flowproof==$V" && flowproof --version
```

For npm, dispatch the smoke test rather than testing locally: Actions →
**npx smoke test** → Run workflow, with `version` set to the new number. It
installs from the registry on ubuntu, macOS and Windows from a clean cache,
with no checkout, exactly as someone with no clone would. It also runs daily
against `latest`, because a published package can break without anyone pushing
a commit.

**One honest gap:** darwin-x64 is verified by architecture at publish time
(`file` reports Mach-O x86_64) but is never executed in CI, because the Intel
macOS runner is not schedulable. Add it back to the smoke matrix the day that
changes.

## When it goes wrong

**You cannot fix a published version — only supersede it.** Both registries are
immutable. If a release ships broken, bump the patch and go again; do not try
to reupload.

**A partial npm publish is recoverable.** Re-run the workflow. Legs already at
that version skip themselves, so the run finishes the missing ones.

**Check both registries after any incident.** They are dispatched separately
and can drift: npm's published history has no `0.10.0` at all, going straight
from `0.9.1` to `0.10.1`, because the npm job was never dispatched for that
release while PyPI's was.

## The shape of the failures this process exists to prevent

Every rule above was bought:

- Dispatching without a version bump — burned 0.2.0 and 0.2.1.
- The spam heuristic on unscoped names — stranded npm on `0.0.1` through five
  PyPI releases.
- An unschedulable Intel macOS runner — blocked a publish for hours, and still
  leaves one platform unexecuted in CI.
- Renaming `## Unreleased` with nothing left behind — wrote two fixes into an
  already-published section, a permanently wrong record.
- Skipping the npm dispatch — left `npx flowproof` two releases behind, still
  hitting a bug that had been fixed twice.
