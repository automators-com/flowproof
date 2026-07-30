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
# Usage:  GH_TOKEN=... prospect.sh [--set adapters] [--tier 3] [--limit 10]
# Prints YAML candidate entries on stdout; writes nothing.
set -euo pipefail

TIER=""
SET=""
LIMIT=10
while [ $# -gt 0 ]; do
  case "$1" in
    --tier)  TIER="$2";  shift 2 ;;
    --set)   SET="$2";   shift 2 ;;
    --limit) LIMIT="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

# `--set` names the LANE, `--tier` names the KIND. They are not the same axis:
# `adapters` spans tiers 3 and 1, so `--set adapters` resolves to the tier that
# is runnable today and tier 1 has to be asked for by number.
case "$SET" in
  agents)   [ -n "$TIER" ] || TIER=2 ;;
  adapters) [ -n "$TIER" ] || TIER=3 ;;
  "")       ;;
  *) echo "unknown set: $SET (agents | adapters)" >&2; exit 2 ;;
esac

# Default to the lane that can actually finish a turn. It used to be tier 2, and
# that stopped being the right default when the record leg turned out to need a
# model credential the sandbox passes nothing into: a Prospector filling a queue
# nothing can drain looks like progress and is not.
[ -n "$TIER" ] || TIER=3

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

# A LIST of queries per tier, not one.
#
# One query per tier was the original shape and it failed in a way worth
# recording: tier 2's `mcp-server in:name,description` cannot match an agent CLI
# that never calls itself an MCP server, so a Prospector turn that wanted
# simonw/llm had to go outside this script to find it - and said so in the
# corpus. A single query does not just rank a class of repository low, it makes
# it unreachable, and nothing in the output distinguishes "none exist" from
# "this query cannot see them".
case "$TIER" in
  # Tier 3 - web UI suites. Open under the 3x-pass guard (CHARTER.md section 3)
  # and the only lane that can finish a turn today.
  #
  # Both queries are biased towards SELF-CONTAINED example suites - a repository
  # that carries the application AND its tests - because standing up a
  # third-party application is what the Prospector's judgement already spends
  # its time declining. Measured 2026-07-30 against the real API, first 30-40
  # results, counting permissively-licensed hits:
  #
  #   cypress-example in:name        11/40 permissive; surfaces
  #                                  cypress-io/cypress-example-kitchensink
  #   playwright-example in:name      6/30 permissive, similar shape
  #   topic:cypress stars:>20        mostly APPLICATIONS that happen to use
  #                                  Cypress (nx, rusefi) - wrong shape
  #   topic:e2e-testing stars:>20    mostly the FRAMEWORKS themselves
  #                                  (playwright, testplane) - wrong shape
  #   topic:cypress topic:e2e-testing  small dedicated suites, but most drive a
  #                                  public demo site, so the oracle depends on
  #                                  a third party staying up
  #
  # GitHub rejects `topic:a OR topic:b` outright ("logical operators only apply
  # to text, not to qualifiers"), which is the other reason this is a list.
  3) SET_OF_TIER=adapters
     QUERIES=('cypress-example in:name' 'playwright-example in:name') ;;
  # Tier 2 - agents and MCP servers. No third-party application has to be stood
  # up, because the agent IS the system under test, and it is where flowproof is
  # differentiated rather than competing. BLOCKED on the record leg today.
  2) SET_OF_TIER=agents
     QUERIES=('mcp-server in:name,description stars:>5') ;;
  # Tier 1 - API/HTTP suites. An exact external oracle, cheap to run.
  1) SET_OF_TIER=adapters
     QUERIES=('topic:rest-api language:python stars:>20') ;;
  *) echo "tier $TIER is not prospected (4 is never autonomous)" >&2; exit 2 ;;
esac

# `--set agents --tier 3` is a contradiction, not a preference. Refuse rather
# than silently honouring one of them.
if [ -n "$SET" ] && [ "$SET" != "$SET_OF_TIER" ]; then
  echo "--set $SET and --tier $TIER disagree: tier $TIER is in set $SET_OF_TIER" >&2
  exit 2
fi

seen="$(grep -oE '^\s+- repo: .+' "$CORPUS" 2>/dev/null | sed 's/.*repo: *//' || true)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# A repository description is arbitrary third-party text going into a YAML
# document, and `note: Cypress E2E: a suite` is not valid YAML - the second
# colon makes the parser reject the whole file. Emitting it single-quoted with
# internal quotes doubled is safe for any printable content, which matters more
# now than it did: every one of the tier 3 queries returns descriptions full of
# colons, so the unquoted form would have produced an unparseable corpus on the
# first adapters turn rather than on some unlucky later one.
yaml_scalar() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/''/g")"; }

# Fetch every query FIRST, then interleave, then filter once.
#
# Running the queries one after another and stopping at the limit looks
# equivalent and is not. Measured against the live API on 2026-07-30: at
# `--limit 6` the Cypress query alone supplied all six, so the Playwright query
# never ran. That is the SAME defect a single query has - a whole class of
# repository unreachable rather than merely ranked low - reintroduced one layer
# up, where it is harder to see because the list of queries looks right.
#
# `paste -d'\n'` takes one row from each query in turn and pads the shorter with
# blanks, which the loop already skips. So each query gets its share of the
# limit, and a query that returns nothing costs the others nothing.
i=0
files=()
for query in "${QUERIES[@]}"; do
  i=$(( i + 1 ))
  # `sort_by(.pushed_at)` cannot be done server-side with the filters we want,
  # so ask for more than we need and filter here.
  gh api -X GET search/repositories \
    -f q="$query" -f sort=updated -f order=desc -F per_page=$(( LIMIT * 4 )) \
    --jq '.items[] | [.full_name, (.license.key // "none"), .pushed_at, .default_branch, (.description // "" | gsub("\n";" "))] | @tsv' \
    > "$WORK/rows.$i" 2>/dev/null || true
  files+=("$WORK/rows.$i")
done
paste -d'\n' "${files[@]}" > "$WORK/rows"

# Read from a file, not from a pipe. `gh | while read` puts the loop in a
# SUBSHELL, so `LIMIT` and `seen` would be discarded when it exits and the
# caller would see neither the count nor the deduplication.
while IFS=$'\t' read -r full licence pushed branch desc; do

  # `paste` pads the shorter query's column with blanks. Skip them.
  [ -n "$full" ] || continue

  # Already recorded, or already emitted by another query this run.
  if printf '%s\n' "$seen" | grep -qxF "$full"; then continue; fi

  case " $ALLOWED_LICENCES " in
    *" $licence "*) ;;
    *) printf '# declined %s - licence %s is not on the allowlist\n' "$full" "$licence"
       seen="$seen
$full"
       continue ;;
  esac

  age_days=$(( ( $(date +%s) - $(date -d "$pushed" +%s 2>/dev/null || echo 0) ) / 86400 ))
  if [ "$age_days" -gt "$STALE_AFTER_DAYS" ]; then
    printf '# declined %s - last pushed %s days ago\n' "$full" "$age_days"
    seen="$seen
$full"
    continue
  fi

  # Pin the SHA. A candidate without one is not evidence: the repository moves
  # and the observation stops being reproducible.
  sha="$(gh api "repos/$full/commits/$branch" --jq .sha 2>/dev/null || true)"
  [ -n "$sha" ] || { printf '# declined %s - could not resolve %s\n' "$full" "$branch"; continue; }

  printf -- '  - repo: %s\n    sha: %s\n    licence: %s\n    tier: %s\n    set: %s\n    pushed: %s\n    status: candidate\n    note: %s\n' \
    "$full" "$sha" "$licence" "$TIER" "$SET_OF_TIER" "${pushed%%T*}" "$(yaml_scalar "${desc:0:80}")"

  seen="$seen
$full"
  LIMIT=$(( LIMIT - 1 ))
  [ "$LIMIT" -gt 0 ] || break
done < "$WORK/rows"
