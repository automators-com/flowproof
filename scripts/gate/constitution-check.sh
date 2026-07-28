#!/usr/bin/env bash
# Refuse a change to the files that constrain the autonomous loops.
#
# The loops' safety rests on one property: "CI is green" means the work is sound.
# An agent optimising for green has two routes - do the work, or weaken the thing
# that measures the work. The second is always cheaper. So the charter the loops
# take direction from, the gate scripts, and CI itself must be outside their
# reach, or the system has no fixed point.
#
# Lives in scripts/gate/ rather than .loop/ because .loop/ is gitignored local
# scratch ("not product"), and the gate has to be committed to be enforceable.
#
# Called by .github/workflows/constitution.yml. Kept as a script rather than
# inline YAML so the logic is testable directly - see constitution-check.test.sh.
#
# Usage:
#   AUTHOR=<login> BASE_REF=<branch> constitution-check.sh
#   AUTHOR=<login> CHANGED_FILES="a b c" constitution-check.sh   # test mode
#
# Exit 0 = allowed, 1 = refused, 2 = misconfigured.
set -euo pipefail

# Anything that constrains the loops: the charter they steer by, the gate that
# judges them, the prompts that define them, and CI. `scripts/gate/` and
# `scripts/loop/` only - the loops own the rest of scripts/ and must stay free to
# work there.
#
# CLAUDE.md is protected for the same reason at one remove: it is loaded into
# every session's context, so editing it rewrites how every future loop behaves.
#
# scripts/loop/ is protected because a Builder that can edit its own role prompt
# is not bounded by it. An instruction its subject can rewrite is a suggestion.
PROTECTED='^(CHARTER\.md|CLAUDE\.md|CODEOWNERS|scripts/gate/|scripts/loop/|\.github/workflows/)'

# Fail-closed: anyone NOT on this list is treated as a loop, so a new loop
# identity is constrained the moment it exists with no edit here required. This
# list living inside a protected path is deliberate - a loop cannot add itself.
HUMANS="${HUMANS:-AminChirazi}"

AUTHOR="${AUTHOR:-}"
[ -n "$AUTHOR" ] || { echo "AUTHOR is unset" >&2; exit 2; }

if [ -n "${CHANGED_FILES:-}" ]; then
  changed="$(printf '%s\n' $CHANGED_FILES)"
else
  base="$(git merge-base "origin/${BASE_REF:?BASE_REF is unset}" HEAD)"
  changed="$(git diff --name-only "$base" HEAD)"
fi

touched="$(printf '%s\n' "$changed" | grep -E "$PROTECTED" || true)"

if [ -z "$touched" ]; then
  echo "no protected path touched"
  exit 0
fi

echo "this change touches protected paths:"
printf '%s\n' "$touched" | sed 's/^/  /'

for h in $HUMANS; do
  if [ "$AUTHOR" = "$h" ]; then
    echo
    echo "author '$AUTHOR' is human-authorised: allowed"
    exit 0
  fi
done

echo
echo "::error::author '$AUTHOR' may not modify the constitution."
cat <<'MSG'

These paths define what constrains the autonomous loops: the charter they take
direction from, the gate scripts, and CI itself. A loop that can edit them can
edit its own limits.

If this change is genuinely needed, a human opens it.
MSG
exit 1
