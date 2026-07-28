#!/usr/bin/env bash
# Refuse a change that gets to green by weakening the thing that measures it.
#
# An agent optimising for green has two routes: do the work, or weaken the gate.
# The second is always cheaper - delete a failing test, add #[ignore], re-record
# the cassette so the new behaviour becomes the expectation, relax an assertion.
# None of that is malice; it is gradient descent on the stated objective. These
# checks close the cheap routes so that weakening the gate takes deliberate
# effort, which is what the Adversary's judgement is then for.
#
# Everything is measured BASE vs HEAD at run time rather than against a stored
# baseline. A stored number would need a human to raise it every time a PR adds
# a test, and a loop that could edit it would have found the cheapest route of
# all.
#
# Usage:  BASE=<ref> HEAD=<ref> ratchets.sh
# Exit 0 = clean, 1 = refused, 2 = misconfigured.
set -euo pipefail

BASE_INPUT="${BASE:?BASE is unset}"
HEAD_REF="${HEAD:-HEAD}"

# Compare against the MERGE-BASE, not the tip of the base branch.
#
# Using the tip means a branch is measured against commits it does not contain:
# when main moves ahead, every count the newer commits raised looks like a fall.
# The first use of this in anger reported "rust tests fell from 662 to 659" for a
# branch that touched no Rust at all - the three tests belonged to a pull request
# merged after the branch was cut. A gate that accuses honest work of the exact
# thing it exists to prevent is worse than no gate: it teaches people to override
# it. constitution-check.sh already does this correctly.
BASE="$(git merge-base "$BASE_INPUT" "$HEAD_REF" 2>/dev/null || echo "$BASE_INPUT")"

# A PR larger than this escalates to a human. Large diffs are exactly where
# review - model or human - stops working, and the cap also forces the
# decomposition the loops need anyway.
MAX_DIFF_LINES="${MAX_DIFF_LINES:-400}"

FAILED=0
note() { printf '  %s\n' "$1"; }
ok()   { printf 'ok      %s\n' "$1"; }
bad()  { printf 'REFUSE  %s\n' "$1"; FAILED=1; }

# Count matching lines at a revision without checking it out.
#
# `git grep` exits 1 when it finds nothing, which under `set -e -o pipefail` is
# indistinguishable from a real failure - and "nothing" is the CORRECT answer for
# a healthy ratchet like #[ignore]. Left unguarded, a clean repo aborts the run
# and the remaining checks never execute: a silent pass disguised as a failure.
# The substitution is isolated so a zero count stays a zero count.
count_at() { # count_at <rev> <ere> <pathspec...>
  local rev="$1" pat="$2"; shift 2
  local out
  out="$(git grep -hEc -- "$pat" "$rev" -- "$@" 2>/dev/null || true)"
  printf '%s\n' "$out" | awk '{s+=$1} END{print s+0}'
}

ratchet_up() {  # a count that must not FALL:  <label> <ere> <pathspec...>
  local label="$1" pat="$2"; shift 2
  local b h
  b="$(count_at "$BASE" "$pat" "$@")"
  h="$(count_at "$HEAD_REF" "$pat" "$@")"
  if [ "$h" -lt "$b" ]; then
    bad "$label fell from $b to $h"
    note "removing a test is a constitution-level act: a human opens it"
  else
    ok "$label $b -> $h"
  fi
}

ratchet_down() {  # a count that must not RISE:  <label> <ere> <pathspec...>
  local label="$1" pat="$2"; shift 2
  local b h
  b="$(count_at "$BASE" "$pat" "$@")"
  h="$(count_at "$HEAD_REF" "$pat" "$@")"
  if [ "$h" -gt "$b" ]; then
    bad "$label rose from $b to $h"
    note "silencing a test is not the same as fixing it"
  else
    ok "$label $b -> $h"
  fi
}

echo "ratchets: $BASE -> $HEAD_REF"
echo

# --- the suite may not shrink -------------------------------------------------
ratchet_up   "rust tests"      '^[[:space:]]*#\[(tokio::)?test\]' '*.rs'
ratchet_up   "python tests"    '^[[:space:]]*def test_'           '*.py'

# --- and may not be silenced --------------------------------------------------
ratchet_down "#[ignore]"       '#\[ignore'                        '*.rs'
ratchet_down "skip/xfail"      '@pytest\.mark\.(skip|xfail)'      '*.py'

# --- a cassette is ground truth ----------------------------------------------
# Adding one is normal work. MODIFYING a committed one silently redefines what
# correct means - the recording stops being evidence and becomes whatever the
# change needed it to be. CHARTER.md invariant 8 makes that human-only.
modified_cassettes="$(git diff --name-status "$BASE" "$HEAD_REF" -- '*.trace.jsonl' \
                      | awk '$1 ~ /^M/ {print $2}')"
if [ -n "$modified_cassettes" ]; then
  bad "a committed cassette was modified"
  printf '%s\n' "$modified_cassettes" | sed 's/^/    /'
  note "adding a cassette is fine; rewriting one redefines the expectation"
else
  ok "no committed cassette modified"
fi

# --- the format and its documentation move together ---------------------------
schema_changed="$(git diff --name-only "$BASE" "$HEAD_REF" -- 'crates/flowproof-trace/schema/*')"
docs_changed="$(git diff --name-only "$BASE" "$HEAD_REF" -- 'docs/trace-format.md')"
if [ -n "$schema_changed" ] && [ -z "$docs_changed" ]; then
  bad "the trace schema changed but docs/trace-format.md did not"
  note "CONTRIBUTING.md requires both to move in the same commit"
elif [ -n "$schema_changed" ]; then
  ok "schema and docs/trace-format.md moved together"
else
  ok "trace schema untouched"
fi

# --- size ---------------------------------------------------------------------
# Cargo.lock is generated, so it is excluded: a routine dependency bump would
# otherwise blow the cap on its own and teach everyone to ignore this check.
lines="$(git diff --numstat "$BASE" "$HEAD_REF" -- . ':(exclude)Cargo.lock' \
         | awk '{a+=$1; d+=$2} END{print a+d+0}')"
if [ "$lines" -gt "$MAX_DIFF_LINES" ]; then
  bad "diff is $lines changed lines, over the $MAX_DIFF_LINES cap"
  note "split it, or a human takes this one - review stops working at this size"
else
  ok "diff size $lines <= $MAX_DIFF_LINES"
fi

echo
if [ "$FAILED" -ne 0 ]; then
  echo "::error::the gate would be weaker after this change"
  exit 1
fi
echo "the gate is no weaker after this change"
