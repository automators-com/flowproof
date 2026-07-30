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

mkdir -p "$STATE"
say()  { printf '\033[36mintegrate\033[0m  %s\n' "$1"; }
warn() { printf '\033[33mintegrate\033[0m  %s\n' "$1"; }
die()  { printf '\033[31mintegrate\033[0m  %s\n' "$1" >&2; exit 1; }

[ -f "$STATE/HALTED" ] && { warn "fleet halted; not merging"; exit 0; }
command -v gh >/dev/null || die "gh is not installed"

# How long a merge may sit unverified before that is itself a reason to stop.
# Not a poll window - the check spans ticks.
VERIFY_DEADLINE_SECONDS="${INTEGRATOR_VERIFY_DEADLINE:-10800}"

# Halt the fleet and say why. Every role checks this file before starting, so
# this stops the world without needing to find anything to signal.
halt() { # halt <reason>
  printf '%s\n' "$1" > "$STATE/HALTED"
  warn "HALTED: $1"
}

# Read the state of a commit's checks, ignoring `adversary` (it reports on the
# pull request, not on main).
checks_of() { # checks_of <sha> -> success | failure | pending
  gh api "repos/$REPO/commits/$1/check-runs" \
    --jq '[.check_runs[] | select(.name != "adversary")]
          | if length == 0 then "pending"
            elif any(.conclusion == "failure" or .conclusion == "timed_out") then "failure"
            elif all(.status == "completed") then "success"
            else "pending" end' 2>/dev/null || echo pending
}

# ---------------------------------------------------------------------------
# Resolve an outstanding verification BEFORE merging anything else.
#
# The first version polled for 600s after merging and then carried on. Measured
# across five pushes to main, `web E2E` took 39-52 minutes and `windows build +
# E2E` 13-15: the two jobs the pull-request path skips, and therefore the only
# ones that can fail after a merge. `web E2E` finished inside that window 0 times
# out of 5. The branch that halts on a red main was unreachable for it, and the
# timeout branch wrote a file nothing ever read.
#
# So verification spans ticks instead. A merge records its SHA; every later run
# resolves it first, and nothing new merges while one is outstanding - otherwise
# unverified merges stack, and the second one hides which change broke main.
# ---------------------------------------------------------------------------
resolve_pending() {
  local f="$STATE/pending-verify" sha started age
  [ -f "$f" ] || return 0
  sha="$(head -1 "$f")"
  started="$(sed -n '2p' "$f")"
  age=$(( $(date +%s) - ${started:-0} ))

  case "$(checks_of "$sha")" in
    success)
      say "main verified green at ${sha:0:8} (after $((age / 60))m)"
      rm -f "$f"; return 0 ;;
    failure)
      halt "main is red at ${sha:0:8}, $((age / 60))m after it was merged"
      gh issue create --repo "$REPO" --label needs-human \
        --title "main is red at ${sha:0:8}" \
        --body "A merge left \`main\` red. The fleet is halted; nothing else will merge.

A loop cannot push to \`main\`, so it cannot revert - the same property that stops it
bypassing review. Revert by hand, then \`rm .loop/HALTED\`." >/dev/null 2>&1 || true
      return 1 ;;
    *)
      if [ "$age" -gt "$VERIFY_DEADLINE_SECONDS" ]; then
        halt "main at ${sha:0:8} has been unverified for $((age / 60))m; refusing to merge onto an unknown state"
        return 1
      fi
      say "main at ${sha:0:8} is still being checked ($((age / 60))m); not merging yet"
      return 1 ;;
  esac
}

resolve_pending || exit 0

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
  # Record the merge for verification and stop. Checking it here would mean
  # blocking a turn for up to an hour, and the previous attempt to avoid that by
  # polling briefly is what made the check unable to see the jobs it exists for.
  # -------------------------------------------------------------------------
  printf '%s\n%s\n' "$after" "$(date +%s)" > "$STATE/pending-verify"
  say "recorded ${after:0:8} for verification; a later run resolves it"
  break
done

say "merged ${merged} pull request(s)"
