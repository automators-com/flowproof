#!/usr/bin/env bash
# Establish the oracle for one corpus entry: fetch it at its pinned SHA, run its
# own suite inside the sandbox, and record what its own tests say.
#
# This half is deterministic on purpose. The judgement - can flowproof express
# this, is the migration vacuous - belongs to the Migrator role. What must NOT be
# a matter of judgement is the baseline it is judged against, or the isolation it
# runs under.
#
# EVERYTHING THIRD-PARTY RUNS IN THE SANDBOX. A corpus repository's `npm install`
# executes arbitrary postinstall scripts, and this box holds SSH deploy keys and
# a GitHub token. That risk is not recoverable by revert, which makes it the most
# serious one in the whole system. `git clone` is the only step run on the host,
# because cloning executes nothing - and even that is done with hooks disabled.
#
# Usage:
#   migrate.sh baseline <owner/repo> <sha>   fetch, install, run the original suite
#   migrate.sh clean <owner/repo>            remove the working copy
#
# `baseline` prints a one-line verdict: PASS, FAIL, or UNRUNNABLE.
#   PASS/FAIL   the suite ran and said so - either is a usable oracle
#   UNRUNNABLE  it could not be made to run; that is a decline, never a reason
#               to try it outside the sandbox
set -euo pipefail

CORPUS_DIR="${CORPUS_DIR:-$HOME/corpus}"
SANDBOX="${SANDBOX:-$(git rev-parse --show-toplevel)/scripts/gate/sandbox-run.sh}"
INSTALL_TIMEOUT="${INSTALL_TIMEOUT:-900}"
TEST_TIMEOUT="${TEST_TIMEOUT:-900}"

cmd="${1:-}"; shift || true
say()  { printf '\033[36mmigrate\033[0m  %s\n' "$1" >&2; }
die()  { printf '\033[31mmigrate\033[0m  %s\n' "$1" >&2; exit 2; }

slug() { printf '%s' "$1" | tr '/' '_'; }

case "$cmd" in
  clean)
    repo="${1:?usage: migrate.sh clean <owner/repo>}"
    rm -rf "${CORPUS_DIR:?}/$(slug "$repo")"
    say "removed $(slug "$repo")"
    exit 0 ;;
  baseline) ;;
  *) die "usage: migrate.sh baseline <owner/repo> <sha> | clean <owner/repo>" ;;
esac

REPO="${1:?usage: migrate.sh baseline <owner/repo> <sha>}"
SHA="${2:?a pinned SHA is required - an unpinned candidate is not evidence}"
WORK="$CORPUS_DIR/$(slug "$REPO")"

[ -x "$SANDBOX" ] || die "the sandbox is missing at $SANDBOX; refusing to run corpus code on the host"

# ---------------------------------------------------------------------------
# Fetch. Pinned, shallow, and with hooks pointed at nothing - a repository can
# ship a .git/hooks payload, and `git clone` is the one step that touches this
# box directly.
# ---------------------------------------------------------------------------
mkdir -p "$CORPUS_DIR"
if [ ! -d "$WORK/.git" ]; then
  rm -rf "$WORK"
  say "fetching $REPO at ${SHA:0:8}"
  git -c core.hooksPath=/dev/null init -q "$WORK"
  # A local path is accepted as a remote so this is testable without a network.
  # Hardcoding the GitHub URL made the harness unrunnable outside CI, which is
  # how an unexercised script ships - the same mistake the adversary reviewer's
  # auth check made.
  if [ -d "$REPO/.git" ]; then remote="$REPO"; else remote="https://github.com/$REPO.git"; fi
  git -C "$WORK" -c core.hooksPath=/dev/null remote add origin "$remote"
fi
git -C "$WORK" -c core.hooksPath=/dev/null fetch -q --depth 1 origin "$SHA" 2>/dev/null \
  || die "could not fetch $SHA from $REPO - the SHA may have been garbage collected"
git -C "$WORK" -c core.hooksPath=/dev/null checkout -q --detach FETCH_HEAD

# ---------------------------------------------------------------------------
# Detect the suite. Nothing clever: if the manifest is not one of these, this is
# a decline rather than a guess. A wrong guess costs a full install.
# ---------------------------------------------------------------------------
if   [ -f "$WORK/package.json" ];    then KIND=node
elif [ -f "$WORK/pyproject.toml" ] || [ -f "$WORK/setup.py" ] || [ -f "$WORK/requirements.txt" ]; then KIND=python
elif [ -f "$WORK/Cargo.toml" ];      then KIND=rust
else
  say "no recognised manifest in $REPO"
  echo UNRUNNABLE; exit 0
fi
say "detected a $KIND project"

sandbox() { # sandbox <phase> <image> -- cmd...
  local phase="$1" image="$2"; shift 2
  "$SANDBOX" --phase "$phase" --image "$image" --work "$WORK" -- "$@"
}

# Install and test are two separate containers, and ONLY /work survives between
# them. Anything a package manager puts in the image - site-packages, CARGO_HOME
# - is discarded, so every install has to target /work or the test phase runs
# without its dependencies.
#
# This was wrong for Python and Rust. A Migrator measured one phase printing
# `pytest 9.1.1` and the next `No module named pytest`, and a `|| true` on the
# pip line hid it - so `baseline` reported FAIL for a repository whose 950 tests
# pass. That is worse than a crash: this script's own contract treats FAIL as a
# usable oracle, so a green flowproof run against it would be filed as a false
# green, which is the priority-1 finding the whole corpus exists to produce.
# Every candidate in the corpus at the time was Python.
#
# Node was correct only by accident - `npm ci` writes node_modules into /work -
# and the accident did not extend as far as it looked. Measured 2026-07-30
# against cypress-io/cypress-example-kitchensink: `npm ci` succeeded, and then
# the suite could not start, because Cypress caches its BROWSER under $HOME
# rather than in node_modules. $HOME is the image. So `baseline` printed FAIL
# for a suite that had executed nothing - and FAIL is a usable oracle by this
# script's own contract, so a flowproof migration passing against it would have
# been filed as a false green. Exactly the defect the paragraph above records
# for Python, in the path that paragraph called correct.
#
# Browser-cache locations are therefore pinned into /work the same way the venv
# and CARGO_HOME are. CYPRESS_CACHE_FOLDER is measured: with it set, the binary
# lands in /work/.cypress-cache/<version>/ and the replay phase starts the
# suite. PLAYWRIGHT_BROWSERS_PATH is by analogy and has NOT been measured - if
# it is wrong, the probe below reports UNRUNNABLE, which is the safe direction.
CACHE_ENV='export CYPRESS_CACHE_FOLDER=/work/.cypress-cache PLAYWRIGHT_BROWSERS_PATH=/work/.playwright;'

case "$KIND" in
  node)   IMAGE=docker.io/library/node:22-bookworm-slim
          # node_modules lands in /work already; the browser caches do not.
          INSTALL=(sh -lc "$CACHE_ENV"' npm ci --no-audit --fund=false 2>/dev/null || npm install --no-audit --fund=false')
          # Does the runner actually start? node_modules alone does not prove
          # it, which is the whole lesson of the Cypress case.
          PROBE=(sh -lc "$CACHE_ENV"' test -d /work/node_modules || exit 1
                         if [ -x /work/node_modules/.bin/cypress ]; then
                           /work/node_modules/.bin/cypress verify
                         fi')
          TEST=(sh -lc "$CACHE_ENV"' npm test --silent') ;;
  python) IMAGE=docker.io/library/python:3.12-slim
          # A venv inside /work, so the interpreter that runs the suite is the
          # one the dependencies were installed into. No `|| true`: an install
          # that fails must report UNRUNNABLE, not a fabricated verdict.
          INSTALL=(sh -lc 'python -m venv /work/.venv \
                           && /work/.venv/bin/pip install --quiet --upgrade pip \
                           && { /work/.venv/bin/pip install --quiet -e ".[test]" \
                                || /work/.venv/bin/pip install --quiet -e . \
                                || /work/.venv/bin/pip install --quiet -r requirements.txt; } \
                           && /work/.venv/bin/pip install --quiet pytest')
          # This used to be the tail of INSTALL. It is worth more here: in the
          # install container /work is populated and $HOME is warm, so it can
          # only prove pytest imports THERE. Run as its own replay-phase
          # container it proves it imports where the suite will actually run.
          PROBE=(sh -lc '/work/.venv/bin/python -c "import pytest"')
          TEST=(sh -lc '/work/.venv/bin/python -m pytest -q') ;;
  rust)   IMAGE=docker.io/library/rust:1-slim
          # CARGO_HOME and the target dir both default into the image.
          INSTALL=(sh -lc 'CARGO_HOME=/work/.cargo cargo fetch --quiet')
          PROBE=(sh -lc 'test -d /work/.cargo')
          TEST=(sh -lc 'CARGO_HOME=/work/.cargo CARGO_TARGET_DIR=/work/.target cargo test --quiet --offline') ;;
esac

# Install needs egress; the suite does not get it back. A test that needs the
# network is not a deterministic oracle, and finding that out here is cheaper
# than finding it out from a flaky verdict later.
say "installing dependencies (sandboxed, egress allowed)"
if ! SANDBOX_TIMEOUT="$INSTALL_TIMEOUT" sandbox install "$IMAGE" "${INSTALL[@]}" >/dev/null 2>&1; then
  say "dependencies would not install"
  echo UNRUNNABLE; exit 0
fi

# ---------------------------------------------------------------------------
# Can the runner start? A SEPARATE replay-phase container, so it is asked under
# the conditions the suite will meet: fresh image, only /work carried over, no
# egress.
#
# "The install succeeded" and "the suite can run" are different claims, and
# until now only the first was checked. That gap is not cosmetic, because of
# what this script does with the answer: UNRUNNABLE is a decline, but FAIL is a
# USABLE ORACLE. So every way of failing to start - a cache left in the image, a
# missing system library, a runner that needs the network it no longer has -
# arrived as FAIL, and a flowproof migration measured against it produced either
# a fabricated agreement or a fabricated false green. A false green is priority
# 1 in CHARTER.md section 5 precisely because it is the finding nobody
# double-checks.
#
# The probe must run BEFORE the suite. Inferring it afterwards from exit codes
# and log text cannot work: a suite that fails and a suite that never started
# both exit non-zero, and telling them apart by grepping output is a guess that
# gets less reliable as the corpus grows.
say "probing whether the runner starts (sandboxed, egress denied)"
if ! SANDBOX_TIMEOUT="$TEST_TIMEOUT" sandbox replay "$IMAGE" "${PROBE[@]}" > "$WORK/.probe.log" 2>&1; then
  say "the test runner cannot start; this is a decline, not a verdict"
  say "  see ${WORK#"$HOME"/}/.probe.log"
  echo UNRUNNABLE; exit 0
fi

say "running the original suite (sandboxed, egress denied)"
if SANDBOX_TIMEOUT="$TEST_TIMEOUT" sandbox replay "$IMAGE" "${TEST[@]}" > "$WORK/.baseline.log" 2>&1; then
  say "the original suite passes"
  echo PASS
else
  rc=$?
  # A suite that fails is still an oracle - FAIL/FAIL is agreement, and a
  # migration that turns it green is a false green, which is priority 1. Only a
  # suite that could not run at all is useless.
  if [ "$rc" -ge 124 ]; then
    say "the original suite timed out; not a usable oracle"
    echo UNRUNNABLE
  else
    say "the original suite fails (that is still an oracle)"
    echo FAIL
  fi
fi
