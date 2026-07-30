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
# Node was correct only by accident - `npm ci` writes node_modules into /work.
case "$KIND" in
  node)   IMAGE=docker.io/library/node:22-bookworm-slim
          # node_modules lands in /work already.
          INSTALL=(sh -lc 'npm ci --no-audit --fund=false 2>/dev/null || npm install --no-audit --fund=false')
          TEST=(sh -lc 'npm test --silent') ;;
  python) IMAGE=docker.io/library/python:3.12-slim
          # A venv inside /work, so the interpreter that runs the suite is the
          # one the dependencies were installed into. No `|| true`: an install
          # that fails must report UNRUNNABLE, not a fabricated verdict.
          INSTALL=(sh -lc 'python -m venv /work/.venv \
                           && /work/.venv/bin/pip install --quiet --upgrade pip \
                           && { /work/.venv/bin/pip install --quiet -e ".[test]" \
                                || /work/.venv/bin/pip install --quiet -e . \
                                || /work/.venv/bin/pip install --quiet -r requirements.txt; } \
                           && /work/.venv/bin/pip install --quiet pytest \
                           && /work/.venv/bin/python -c "import pytest"')
          TEST=(sh -lc '/work/.venv/bin/python -m pytest -q') ;;
  rust)   IMAGE=docker.io/library/rust:1-slim
          # CARGO_HOME and the target dir both default into the image.
          INSTALL=(sh -lc 'CARGO_HOME=/work/.cargo cargo fetch --quiet')
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
