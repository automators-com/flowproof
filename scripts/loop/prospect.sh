#!/usr/bin/env bash
# Find corpus candidates: public repositories whose tests flowproof could try to
# reproduce, or whose agents it could try to record.
#
# Deterministic on purpose. Searching, filtering by licence, pinning a SHA and
# deduplicating against what is already recorded need no judgement, and a script
# that does them costs nothing per candidate. The Prospector role's judgement -
# is this worth a Migrator turn - is applied to what this surfaces.
#
# READ-ONLY OUTWARD, always. It reads public metadata. It never clones, never
# executes anything it finds, and never contacts a third-party repository. Only
# the Migrator runs corpus code, and only inside scripts/gate/sandbox-run.sh.
#
# Usage:  GH_TOKEN=... prospect.sh [--tier 2] [--limit 10]
# Prints YAML candidate entries on stdout; writes nothing.
set -euo pipefail

TIER=2
LIMIT=10
while [ $# -gt 0 ]; do
  case "$1" in
    --tier)  TIER="$2";  shift 2 ;;
    --limit) LIMIT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

CORPUS="${CORPUS_FILE:-docs/loop/corpus.yaml}"

# Permissive licences only. The corpus stores pointers, never vendored code, so
# reading is not the risk - but a migrated test is written from someone else's
# test, and for a copyleft licence that argument is one a lawyer should make
# rather than a loop. An unlisted licence is a decline, and the reason is
# reported so the list gets widened deliberately rather than by accident.
ALLOWED_LICENCES="mit apache-2.0 bsd-2-clause bsd-3-clause isc unlicense 0bsd"

# Repositories that have not moved in a year are unlikely to run: their
# dependencies have rotted even if the code is fine, and a Migrator turn spent
# discovering that is the expensive way to learn it.
STALE_AFTER_DAYS="${STALE_AFTER_DAYS:-365}"

command -v gh >/dev/null || { echo "gh is not installed" >&2; exit 2; }

case "$TIER" in
  # Tier 2 - agents and MCP servers. Run these first: no third-party
  # application has to be stood up, because the agent IS the system under test,
  # and it is where flowproof is differentiated rather than competing.
  2) QUERY='mcp-server in:name,description stars:>5' ;;
  # Tier 1 - API/HTTP suites. An exact external oracle, cheap to run.
  1) QUERY='topic:rest-api language:python stars:>20' ;;
  *) echo "tier $TIER is not prospected (3 is declined, 4 is never autonomous)" >&2; exit 2 ;;
esac

already="$(grep -oE '^\s+- repo: .+' "$CORPUS" 2>/dev/null | sed 's/.*repo: *//' || true)"

# `sort_by(.pushed_at)` cannot be done server-side with the filters we want, so
# ask for more than we need and filter here.
gh api -X GET search/repositories \
  -f q="$QUERY" -f sort=updated -f order=desc -F per_page=$(( LIMIT * 4 )) \
  --jq '.items[] | [.full_name, (.license.key // "none"), .pushed_at, .default_branch, (.description // "" | gsub("\n";" "))] | @tsv' \
  2>/dev/null | while IFS=$'\t' read -r full licence pushed branch desc; do

  [ -n "$full" ] || continue

  # Already recorded: not a finding.
  printf '%s\n' "$already" | grep -qxF "$full" && continue

  case " $ALLOWED_LICENCES " in
    *" $licence "*) ;;
    *) printf '# declined %s - licence %s is not on the allowlist\n' "$full" "$licence"; continue ;;
  esac

  age_days=$(( ( $(date +%s) - $(date -d "$pushed" +%s 2>/dev/null || echo 0) ) / 86400 ))
  if [ "$age_days" -gt "$STALE_AFTER_DAYS" ]; then
    printf '# declined %s - last pushed %s days ago\n' "$full" "$age_days"
    continue
  fi

  # Pin the SHA. A candidate without one is not evidence: the repository moves
  # and the observation stops being reproducible.
  sha="$(gh api "repos/$full/commits/$branch" --jq .sha 2>/dev/null || true)"
  [ -n "$sha" ] || { printf '# declined %s - could not resolve %s\n' "$full" "$branch"; continue; }

  printf -- '  - repo: %s\n    sha: %s\n    licence: %s\n    tier: %s\n    pushed: %s\n    status: candidate\n    note: %s\n' \
    "$full" "$sha" "$licence" "$TIER" "${pushed%%T*}" "${desc:0:80}"

  LIMIT=$(( LIMIT - 1 ))
  [ "$LIMIT" -le 0 ] && break
done
