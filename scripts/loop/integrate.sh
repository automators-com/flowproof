#!/usr/bin/env bash
# The Integrator: the only thing that puts a loop's work on main.
#
# Deliberately NOT a model role. Finding an approved pull request whose checks
# are green and merging it needs no judgement, and a deterministic script cannot
# talk itself into a merge. The judgement already happened - the Adversary's four
# lenses and the ratchets - and this only acts on a verdict someone else reached.
#
# SERIALIZED, one at a time, by the caller's lock. Two pull requests that are
# independently green can break main together; merging one, letting `strict`
# force the next to rebase, and re-running its checks is what catches that.
#
# It cannot bypass anything. The loop token is not in the ruleset's bypass list,
# so a merge only succeeds when GitHub agrees every required check passed and the
# Adversary approved. This script asks; branch protection decides.
#
# Usage:  GH_TOKEN=... integrate.sh [--max N]
# Exit 0 = did what there was to do (including nothing), 1 = failed or halted.
set -euo pipefail

REPO="${FLOWPROOF_REPO:-automators-com/flowproof}"
BUILDER="${BUILDER_LOGIN:-AutomatorsAgent}"
STATE="${LOOP_STATE:-$(git rev-parse --show-toplevel)/.loop}"
MAX="${1:-1}"; [ "$MAX" = "--max" ] && MAX="${2:-1}"
VERIFY_SECONDS="${INTEGRATOR_VERIFY_SECONDS:-600}"

mkdir -p "$STATE"
say()  { printf '\033[36mintegrate\033[0m  %s\n' "$1"; }
warn() { printf '\033[33mintegrate\033[0m  %s\n' "$1"; }
die()  { printf '\033[31mintegrate\033[0m  %s\n' "$1" >&2; exit 1; }

[ -f "$STATE/HALTED" ] && { warn "fleet halted; not merging"; exit 0; }
command -v gh >/dev/null || die "gh is not installed"

# Halt the fleet and say why. Every role checks this file before starting, so
# this stops the world without needing to find anything to signal.
halt() { # halt <reason>
  printf '%s\n' "$1" > "$STATE/HALTED"
  warn "HALTED: $1"
}

# ---------------------------------------------------------------------------
# Candidates: the Builder's own pull requests that a reviewer has approved.
# Human pull requests are never merged here - a human merges their own, and
# the Adversary does not review them.
# ---------------------------------------------------------------------------
candidates="$(gh pr list --repo "$REPO" --state open --author "$BUILDER" \
                --json number,reviewDecision,mergeStateStatus,title \
                --jq '[.[] | select(.reviewDecision == "APPROVED")]
                      | sort_by(.number) | .[] | @base64' 2>/dev/null || true)"

[ -n "$candidates" ] || { say "no approved Builder pull request is waiting"; exit 0; }

merged=0
for row in $candidates; do
  [ "$merged" -ge "$MAX" ] && break
  d() { printf '%s' "$row" | base64 -d | python3 -c "import json,sys;print(json.load(sys.stdin)['$1'])"; }
  n="$(d number)"; status="$(d mergeStateStatus)"; title="$(d title)"
  review="$(d reviewDecision)"

  # Re-assert the approval HERE, at the decision point, not only in the query
  # that built this list. The first version filtered in the `gh pr list --jq`
  # and never checked again, so the one property that matters - nothing merges
  # without a reviewer - rested on a query it had already stopped looking at.
  # The tests caught it merging a pull request with CHANGES_REQUESTED.
  if [ "$review" != "APPROVED" ]; then
    say "#$n has review '$review'; not merging"
    continue
  fi

  case "$status" in
    BEHIND)
      # `strict` requires the branch to be current. Updating it re-runs every
      # check against the merged result, which is the whole point: this is where
      # a semantic conflict between two independently-green branches shows up.
      say "#$n is behind main; updating it and leaving the checks to re-run"
      gh api -X PUT "repos/$REPO/pulls/$n/update-branch" >/dev/null 2>&1 \
        || warn "#$n could not be updated; it may need a manual rebase"
      continue ;;
    BLOCKED|UNSTABLE|DIRTY)
      say "#$n is $status; leaving it"
      continue ;;
    UNKNOWN)
      say "#$n mergeability not yet computed; will retry next turn"
      continue ;;
  esac

  before="$(gh api "repos/$REPO/commits/main" --jq .sha)"
  say "merging #$n - $title"
  if ! gh pr merge "$n" --repo "$REPO" --merge >/dev/null 2>&1; then
    warn "#$n refused the merge; branch protection did not agree"
    continue
  fi
  merged=$((merged + 1))
  after="$(gh api "repos/$REPO/commits/main" --jq .sha)"
  printf '%s\n' "$after" > "$STATE/last-merge"
  say "merged #$n; main $before -> $after"

  # -------------------------------------------------------------------------
  # Post-merge verification. Every check on the pull request was green against
  # the merge result, but main runs jobs the pull-request path skips, and a
  # scheduled job can fail on something no branch exercised.
  # -------------------------------------------------------------------------
  say "verifying main at $after"
  deadline=$(( $(date +%s) + VERIFY_SECONDS ))
  verdict=pending
  while [ "$(date +%s)" -lt "$deadline" ]; do
    st="$(gh api "repos/$REPO/commits/$after/check-runs" \
          --jq '[.check_runs[] | select(.name != "adversary")]
                | if length == 0 then "pending"
                  elif any(.conclusion == "failure" or .conclusion == "timed_out") then "failure"
                  elif all(.status == "completed") then "success"
                  else "pending" end' 2>/dev/null || echo pending)"
    [ "$st" = "pending" ] || { verdict="$st"; break; }
    sleep 20
  done

  case "$verdict" in
    success) say "main is green after #$n" ;;
    failure)
      # The loop cannot push to main - it is not a bypass actor - so it cannot
      # revert directly. Halting is the immediate act that matters: it stops
      # anything else landing on a broken main. The revert itself is a pull
      # request like any other, and a human can merge it faster than the queue.
      halt "main is red after merging #$n ($after); nothing else may merge"
      gh issue create --repo "$REPO" --label needs-human \
        --title "main is red after #$n" \
        --body "Merging #$n left \`main\` red at \`$after\`.

The fleet is halted (\`.loop/HALTED\`). Nothing else will merge until a human clears it.

A loop cannot push to \`main\` and therefore cannot revert directly - that is the same
property that stops it bypassing review. Revert with:

\`\`\`bash
git revert -m 1 $after
\`\`\`

then clear the breaker with \`rm .loop/HALTED\`." >/dev/null 2>&1 || true
      exit 1 ;;
    *)
      warn "main's checks did not settle within ${VERIFY_SECONDS}s; recorded for the next turn"
      printf '%s\n' "$after" > "$STATE/unverified-merge" ;;
  esac
done

say "merged ${merged} pull request(s)"
