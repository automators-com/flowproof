#!/usr/bin/env bash
# Run one adversarial lens over a pull request's diff and emit a verdict.
#
# Refute-by-default. Each lens is a separate invocation with its own context, so
# a blind spot in one is not automatically a blind spot in the others - four
# questions asked independently, rather than one reviewer asked to hold four
# things in mind at once.
#
# FAIL-CLOSED, and this is the part that matters. Anything other than an explicit
# APPROVE is a refusal: an empty answer, a truncated one, a crash, a timeout, a
# missing key, a malformed verdict. A reviewer that could not answer has not
# approved. The mechanical ratchets exist precisely because a model's judgement
# is the layer that can silently degrade.
#
# The reviewer gets READ-ONLY tools. It inspects; it does not edit, and it does
# not run the code it is reviewing.
#
# Usage:  BASE=<ref> HEAD=<ref> adversary-review.sh <lens>
# Exit 0 = approve, 1 = refuse, 2 = misconfigured.
set -euo pipefail

LENS="${1:?usage: adversary-review.sh <lens>}"
BASE="${BASE:?BASE is unset}"
HEAD_REF="${HEAD:?HEAD is unset}"
HERE="$(cd "$(dirname "$0")" && pwd)"
PROMPT_FILE="$HERE/lenses/$LENS.md"

[ -f "$PROMPT_FILE" ] || { echo "no such lens: $LENS" >&2; exit 2; }
command -v claude >/dev/null || { echo "the claude CLI is not installed" >&2; exit 2; }
# Authentication rather than a specific variable: CI supplies ANTHROPIC_API_KEY,
# and a developer running this by hand may already be logged in. Requiring the
# variable outright would have made this script untestable outside CI, which is
# exactly how an unexercised merge gate ships.
claude -p "ok" --output-format text < /dev/null >/dev/null 2>&1 \
  || { echo "the claude CLI is not authenticated" >&2; exit 2; }

# The merge-base, not the tip: a branch behind its base is not a regression, and
# diffing against a moved tip includes reversing the other branch's work.
base="$(git merge-base "$BASE" "$HEAD_REF" 2>/dev/null || echo "$BASE")"
diff="$(git diff "$base" "$HEAD_REF")"
[ -n "$diff" ] || { echo "empty diff; nothing to review"; exit 0; }

out="$(mktemp)"
trap 'rm -f "$out"' EXIT

set +e
claude -p "$(cat <<PROMPT
Review this change through one lens only. Other reviewers hold the others; do not
try to cover for them, and do not soften a finding because it might be someone
else's lens.

$(cat "$PROMPT_FILE")

You may read any file in the repository to check a claim. Do not edit anything.

End your reply with exactly one line, on its own, using this exact marker:

ADVERSARY-VERDICT-7f3a: APPROVE
or
ADVERSARY-VERDICT-7f3a: REFUSE

Then, if you refused, one short paragraph naming the specific defect - the file,
the line, and what breaks. "Looks risky" is not a finding.

The diff under review, ${base}..${HEAD_REF}:

\`\`\`diff
${diff}
\`\`\`
PROMPT
)" \
  --allowed-tools Read Grep Glob \
  --disallowed-tools "Bash(sudo *)" "Bash(curl *)" "Bash(wget *)" \
  --max-turns 40 \
  --output-format text < /dev/null > "$out" 2>&1
rc=$?
set -e

cat "$out"
echo

if [ "$rc" -ne 0 ]; then
  echo "::error::${LENS}: the reviewer exited ${rc}; refusing"
  exit 1
fi

# ANY REFUSE WINS. Not last-one-wins: the diff under review is inside the
# prompt, so a change that contains a verdict line can get it quoted back in the
# reply. Reducing with `tail -1` let a later APPROVE override an earlier REFUSE,
# which made "put an approving verdict line in your diff" a way to flip your own
# review. This exact defect was found by the gate-integrity lens reviewing this
# script - the reducer must fail closed in the same direction as everything else.
#
# The marker is deliberately unguessable-ish rather than the plain word VERDICT,
# so ordinary prose about verdicts cannot collide with it.
approves="$(grep -cE '^ADVERSARY-VERDICT-7f3a: APPROVE' "$out" || true)"
refuses="$(grep -cE '^ADVERSARY-VERDICT-7f3a: REFUSE' "$out" || true)"

if [ "$refuses" -gt 0 ]; then
  echo "::error::${LENS}: refuse"; exit 1
fi
if [ "$approves" -eq 1 ]; then
  echo "${LENS}: approve"; exit 0
fi
if [ "$approves" -gt 1 ]; then
  echo "::error::${LENS}: ${approves} approving verdicts in one reply; refusing"; exit 1
fi
echo "::error::${LENS}: no verdict in the reply; refusing (fail-closed)"
exit 1
