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
# Tier 3 was here until 2026-07-30 and is deliberately gone: it is open under
# the 3x-pass guard (CHARTER.md section 3). Tier 4 stays - no public corpus.
for tier in 4 5; do
  if GH_ROWS="$TMP/rows" CORPUS_FILE="$TMP/corpus" "$SUT" --tier "$tier" >/dev/null 2>&1; then
    printf 'FAIL  %-50s tier %s was prospected\n' "tier $tier is refused" "$tier"; FAILED=1
  else
    printf 'ok    %-50s %s\n' "tier $tier is refused" "REFUSED"
  fi
done

# --- the two lanes -----------------------------------------------------------
# A lane a query cannot reach is indistinguishable from a lane with nothing in
# it. That is how tier 2's single `mcp-server` query hid every agent CLI, so
# these assert on the QUERIES the script sends, not only on what it does with
# the answers. The stub records them.
cat > "$TMP/gh" <<'STUB'
#!/usr/bin/env bash
# Logs each query, and answers a Playwright query from GH_ROWS_PW when that is
# set - so a test can tell which query a result came from.
case "$*" in
  *search/repositories*)
    q=""
    for a in "$@"; do case "$a" in q=*) q="${a#q=}"; printf '%s\n' "$q" >> "${GH_QLOG:-/dev/null}" ;; esac; done
    case "$q" in
      *playwright*) cat "${GH_ROWS_PW:-${GH_ROWS:?}}" ;;
      *)            cat "${GH_ROWS:?}" ;;
    esac ;;
  *commits*) cat "${GH_SHA:-/dev/null}" 2>/dev/null || echo "deadbeef" ;;
  *) echo "" ;;
esac
STUB
chmod +x "$TMP/gh"

run() { # run <args...>  -> stdout, with queries logged to $TMP/qlog
  : > "$TMP/qlog"
  printf 'version: 1\ncandidates:\n' > "$TMP/corpus"
  echo "deadbeefcafe" > "$TMP/sha"
  GH_ROWS="$TMP/rows" GH_ROWS_PW="${PW_ROWS:-$TMP/rows}" GH_SHA="$TMP/sha" \
    GH_QLOG="$TMP/qlog" CORPUS_FILE="$TMP/corpus" \
    "$SUT" "$@" 2>/dev/null
}
check() { # check <description> <condition-result> [detail]
  if [ "$2" = 0 ]; then printf 'ok    %-50s\n' "$1"
  else printf 'FAIL  %-50s %s\n' "$1" "${3:-}"; FAILED=1; fi
}

echo "-- the adapters lane can reach web suites at all --"
printf '%s\n' "$(row org/a mit "$today")" "$(row org/b mit "$today")" > "$TMP/rows"
out="$(run --set adapters --limit 10)"
grep -qi 'cypress' "$TMP/qlog"; check "an adapters turn queries for Cypress" $?
grep -qi 'playwright' "$TMP/qlog"; check "an adapters turn queries for Playwright" $?
nq="$(wc -l < "$TMP/qlog")"
if [ "$nq" -ge 2 ]; then check "more than one query, so one blind spot is not total" 0
else check "more than one query, so one blind spot is not total" 1 "sent $nq"; fi

out="$(run --set agents --limit 10)"
grep -qi 'mcp-server' "$TMP/qlog"; check "an agents turn still queries for MCP servers" $?
if grep -qi 'cypress' "$TMP/qlog"; then check "and does not leak the adapters query into it" 1 "it did"
else check "and does not leak the adapters query into it" 0; fi

echo "-- every candidate names its set, and the set matches the tier --"
printf '%s\n' "$(row org/a mit "$today")" > "$TMP/rows"
run --set adapters --limit 5 | grep -q '^    set: adapters$'; check "tier 3 emits set: adapters" $?
run --set agents   --limit 5 | grep -q '^    set: agents$';   check "tier 2 emits set: agents" $?
run --tier 1       --limit 5 | grep -q '^    set: adapters$'; check "tier 1 is in set: adapters too" $?
run --tier 3 --limit 5 | grep -cq '^    set:' ; check "the field is never omitted" $?

echo "-- a set and a tier that disagree is a contradiction, not a preference --"
if run --set agents --tier 3 >/dev/null 2>&1; then
  check "--set agents --tier 3 is refused" 1 "it ran anyway"
else check "--set agents --tier 3 is refused" 0; fi
if run --set nonsense >/dev/null 2>&1; then
  check "an unknown set is refused" 1 "it ran anyway"
else check "an unknown set is refused" 0; fi

echo "-- the second query must not be starved by the first --"
# Found by running the real thing against the live API rather than by reading
# it: at --limit 6 the Cypress query supplied all six results and the Playwright
# query never ran. A list of queries that executes in order and stops at the
# limit reproduces the exact defect one query has - a whole class unreachable
# rather than ranked low - one layer up, where the list looks right.
{ row cy/1 mit "$today"; row cy/2 mit "$today"; row cy/3 mit "$today"
  row cy/4 mit "$today"; row cy/5 mit "$today"; } > "$TMP/rows"
{ row pw/1 mit "$today"; row pw/2 mit "$today"; row pw/3 mit "$today"; } > "$TMP/rows.pw"
out="$(PW_ROWS="$TMP/rows.pw" run --set adapters --limit 4)"
printf '%s' "$out" | grep -q '^  - repo: cy/'; check "the Cypress query is represented" $?
printf '%s' "$out" | grep -q '^  - repo: pw/'; rc=$?
check "the Playwright query is too, at a limit the" "$rc" "Cypress query alone could fill; none from pw/"
n="$(printf '%s' "$out" | grep -c '^  - repo:')"
if [ "$n" -eq 4 ]; then check "and the limit is still honoured exactly" 0
else check "and the limit is still honoured exactly" 1 "got $n, wanted 4"; fi

echo "-- two queries must not report the same repository twice --"
# The stub answers every query identically, so a run that forgets what it has
# already emitted reports each repository once per query. This is the bug a
# `gh | while read` subshell reintroduces: LIMIT and the seen-set reset between
# queries and nothing downstream notices, because duplicate YAML entries parse.
printf '%s\n' "$(row org/dupe mit "$today")" > "$TMP/rows"
n="$(run --set adapters --limit 10 | grep -c '^  - repo: org/dupe$')"
if [ "$n" -eq 1 ]; then check "a repository both queries return is emitted once" 0
else check "a repository both queries return is emitted once" 1 "emitted $n times"; fi

echo "-- the output has to parse as YAML, whatever the description says --"
# A repository description is arbitrary third-party text. `note: Cypress E2E: a
# suite` is not valid YAML, and every tier 3 query returns colon-laden
# descriptions, so this would have broken on the first real adapters turn.
printf '%s\n' "$(row org/colon mit "$today" main "Cypress E2E: a suite, 100% green")" > "$TMP/rows"
run --set adapters --limit 5 > "$TMP/out.yaml"
if command -v python3 >/dev/null; then
  python3 -c '
import sys, yaml
d = yaml.safe_load("version: 1\ncandidates:\n" + open(sys.argv[1]).read())
assert d["candidates"][0]["repo"] == "org/colon", d
assert "Cypress E2E: a suite" in d["candidates"][0]["note"], d
' "$TMP/out.yaml" 2>/dev/null
  check "a description containing a colon still parses" $?
  printf '%s\n' "$(row org/quote mit "$today" main "it's a \"suite\"")" > "$TMP/rows"
  run --set adapters --limit 5 > "$TMP/out.yaml"
  python3 -c '
import sys, yaml
d = yaml.safe_load("version: 1\ncandidates:\n" + open(sys.argv[1]).read())
assert "it'"'"'s" in d["candidates"][0]["note"], d
' "$TMP/out.yaml" 2>/dev/null
  check "and so does one containing quotes" $?
else
  printf 'skip  %-50s python3 unavailable\n' "YAML parse checks"
fi

echo
[ "$FAILED" -ne 0 ] && { echo "prospector tests FAILED"; exit 1; }
echo "the Prospector records only what a Migrator could plausibly run"
