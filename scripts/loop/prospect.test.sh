#!/usr/bin/env bash
# Tests for the Prospector's filters.
#
# What it REFUSES matters more than what it finds. A candidate that cannot run,
# or whose licence makes a migrated test a legal question, costs a Migrator turn
# to discover - and Migrator turns are the expensive ones.
#
# Drives the real script with a stub `gh` on PATH.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

SUT="$(pwd)/scripts/loop/prospect.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
FAILED=0

cat > "$TMP/gh" <<'STUB'
#!/usr/bin/env bash
# search/repositories -> the TSV rows under test; repos/*/commits/* -> a SHA.
case "$*" in
  *search/repositories*) cat "${GH_ROWS:?}" ;;
  *commits*)             cat "${GH_SHA:-/dev/null}" 2>/dev/null || echo "deadbeef" ;;
  *) echo "" ;;
esac
STUB
chmod +x "$TMP/gh"; export PATH="$TMP:$PATH"

row() { # row <full> <licence> <pushed> [branch] [desc]
  printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "${4:-main}" "${5:-a description}"
}

t() { # t <FOUND|DECLINED|SKIPPED> <description> <row> [corpus-contents]
  local want="$1" desc="$2" r="$3" corpus="${4:-}" out got
  printf '%s\n' "$r" > "$TMP/rows"
  printf 'version: 1\ncandidates:\n%s\n' "$corpus" > "$TMP/corpus"
  echo "deadbeefcafe" > "$TMP/sha"
  out="$(GH_ROWS="$TMP/rows" GH_SHA="$TMP/sha" CORPUS_FILE="$TMP/corpus" \
         "$SUT" --tier 2 --limit 5 2>/dev/null)"
  if   printf '%s' "$out" | grep -q '^  - repo:'; then got=FOUND
  elif printf '%s' "$out" | grep -q '^# declined';  then got=DECLINED
  else got=SKIPPED; fi
  if [ "$got" = "$want" ]; then printf 'ok    %-50s %s\n' "$desc" "$got"
  else printf 'FAIL  %-50s got %s, wanted %s\n' "$desc" "$got" "$want"; FAILED=1; fi
}

today="$(date -u +%Y-%m-%dT00:00:00Z)"
old="$(date -u -d '800 days ago' +%Y-%m-%dT00:00:00Z 2>/dev/null || echo 2020-01-01T00:00:00Z)"

echo "-- what it records --"
t FOUND    "permissive licence, recently pushed"  "$(row org/good mit "$today")"
t FOUND    "apache-2.0 is permissive too"         "$(row org/good apache-2.0 "$today")"

echo "-- what it refuses, and says why --"
t DECLINED "copyleft licence"                     "$(row org/gpl gpl-3.0 "$today")"
t DECLINED "no licence at all"                    "$(row org/none none "$today")"
t DECLINED "unmaintained for years"               "$(row org/stale mit "$old")"

echo "-- what it does not re-report --"
t SKIPPED  "already in the corpus"                "$(row org/known mit "$today")" "  - repo: org/known"

echo "-- a candidate without a pinned SHA is not evidence --"
: > "$TMP/emptysha"
printf '%s\n' "$(row org/nosha mit "$today")" > "$TMP/rows"
printf 'version: 1\ncandidates:\n' > "$TMP/corpus"
out="$(GH_ROWS="$TMP/rows" GH_SHA="$TMP/emptysha" CORPUS_FILE="$TMP/corpus" "$SUT" --tier 2 --limit 5 2>/dev/null)"
if printf '%s' "$out" | grep -q 'could not resolve'; then
  printf 'ok    %-50s %s\n' "unresolvable SHA is declined" "DECLINED"
else
  printf 'FAIL  %-50s did not decline: %s\n' "unresolvable SHA is declined" "$(printf '%s' "$out" | head -1)"
  FAILED=1
fi

echo "-- tiers that are not prospected --"
for tier in 3 4; do
  if GH_ROWS="$TMP/rows" CORPUS_FILE="$TMP/corpus" "$SUT" --tier "$tier" >/dev/null 2>&1; then
    printf 'FAIL  %-50s tier %s was prospected\n' "tier $tier is refused" "$tier"; FAILED=1
  else
    printf 'ok    %-50s %s\n' "tier $tier is refused" "REFUSED"
  fi
done

echo
[ "$FAILED" -ne 0 ] && { echo "prospector tests FAILED"; exit 1; }
echo "the Prospector records only what a Migrator could plausibly run"
