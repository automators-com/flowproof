#!/usr/bin/env bash
# Tests for the Integrator.
#
# This is the only thing that puts a loop's work on main, so what it REFUSES to
# merge matters more than what it merges. Every case below drives the real
# script with a stub `gh` on PATH, so a regression in the shipped logic fails
# here - a copy of the logic pasted into a test cannot do that.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

SUT="$(pwd)/scripts/loop/integrate.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export LOOP_STATE="$TMP/state" INTEGRATOR_VERIFY_SECONDS=1
mkdir -p "$LOOP_STATE"
FAILED=0

# A stub `gh`. $GH_PRS is the pr-list payload; $GH_MERGED records any merge.
cat > "$TMP/gh" <<'STUB'
#!/usr/bin/env bash
case "$1 $2" in
  "pr list") cat "${GH_PRS:?}" ;;
  "pr merge") echo "$3" >> "${GH_MERGED:?}" ;;
  "api -X") echo "updated" ;;   # update-branch
  "issue create") echo "$*" >> "${GH_ISSUES:-/dev/null}" ;;
  *)
    case "$*" in
      *commits/main*)       echo "aaaaaaa" ;;
      *check-runs*)         cat "${GH_CHECKS:-/dev/null}" 2>/dev/null || echo success ;;
      *commits/*)           echo "bbbbbbb" ;;
      *) echo "" ;;
    esac ;;
esac
STUB
chmod +x "$TMP/gh"; export PATH="$TMP:$PATH"

pr() { # pr <number> <reviewDecision> <mergeStateStatus>
  printf '%s\n' "$(printf '{"number":%s,"reviewDecision":"%s","mergeStateStatus":"%s","title":"t"}' "$1" "$2" "$3" \
    | python3 -c 'import base64,sys;print(base64.b64encode(sys.stdin.read().strip().encode()).decode())')"
}

t() { # t <MERGE|SKIP> <description> <pr-rows...>
  local want="$1" desc="$2"; shift 2
  : > "$TMP/prs"; for r in "$@"; do printf '%s\n' "$r" >> "$TMP/prs"; done
  : > "$TMP/merged"; echo success > "$TMP/checks"
  rm -f "$LOOP_STATE/HALTED"
  GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" \
    "$SUT" --max 1 >/dev/null 2>&1
  local got; got=$([ -s "$TMP/merged" ] && echo MERGE || echo SKIP)
  if [ "$got" = "$want" ]; then printf 'ok    %-50s %s\n' "$desc" "$got"
  else printf 'FAIL  %-50s got %s, wanted %s\n' "$desc" "$got" "$want"; FAILED=1; fi
}

echo "-- what it merges --"
t MERGE "approved and clean"                    "$(pr 10 APPROVED CLEAN)"

echo "-- what it must never merge --"
t SKIP  "approved but checks not green"         "$(pr 10 APPROVED BLOCKED)"
t SKIP  "clean but never approved"              "$(pr 10 REVIEW_REQUIRED CLEAN)"
t SKIP  "clean but changes requested"           "$(pr 10 CHANGES_REQUESTED CLEAN)"
t SKIP  "behind main (updates, does not merge)" "$(pr 10 APPROVED BEHIND)"
t SKIP  "mergeability not yet known"            "$(pr 10 APPROVED UNKNOWN)"
t SKIP  "conflicting with main"                 "$(pr 10 APPROVED DIRTY)"
t SKIP  "nothing waiting at all"

echo "-- the breaker outranks everything --"
: > "$TMP/prs"; printf '%s\n' "$(pr 10 APPROVED CLEAN)" > "$TMP/prs"
: > "$TMP/merged"; echo "halted for a test" > "$LOOP_STATE/HALTED"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" "$SUT" --max 1 >/dev/null 2>&1
if [ -s "$TMP/merged" ]; then
  printf 'FAIL  %-50s merged while halted\n' "a halted fleet merges nothing"; FAILED=1
else
  printf 'ok    %-50s SKIP\n' "a halted fleet merges nothing"
fi
rm -f "$LOOP_STATE/HALTED"

echo "-- verification spans ticks, and blocks the next merge --"
# The synchronous case this replaced could not work: `web E2E` takes 39-52
# minutes and the poll waited 600s, so the halt-on-red branch was unreachable
# for exactly the job it existed to catch. Verification now records a SHA and a
# LATER run resolves it, so these cases are the merge and the resolution
# separately - and the merge must no longer halt by itself.
printf '%s\n' "$(pr 10 APPROVED CLEAN)" > "$TMP/prs"; : > "$TMP/merged"
echo success > "$TMP/checks"; rm -f "$LOOP_STATE/pending-verify" "$LOOP_STATE/HALTED"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" "$SUT" --max 1 >/dev/null 2>&1
if [ -s "$TMP/merged" ] && [ -f "$LOOP_STATE/pending-verify" ] && [ ! -f "$LOOP_STATE/HALTED" ]; then
  printf 'ok    %-50s %s\n' "a merge records a SHA and does not halt" "recorded"
else
  printf 'FAIL  %-50s merged=%s pending=%s halted=%s\n' "a merge records a SHA and does not halt" \
    "$([ -s "$TMP/merged" ] && echo y || echo n)" \
    "$([ -f "$LOOP_STATE/pending-verify" ] && echo y || echo n)" \
    "$([ -f "$LOOP_STATE/HALTED" ] && echo y || echo n)"; FAILED=1
fi

# Still running: nothing new may merge onto an unknown state.
: > "$TMP/merged"; printf 'pending\n' > "$TMP/checks"
printf '%s\n' "$(pr 11 APPROVED CLEAN)" > "$TMP/prs"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" "$SUT" --max 1 >/dev/null 2>&1
if [ ! -s "$TMP/merged" ]; then
  printf 'ok    %-50s %s\n' "an outstanding verification blocks new merges" "SKIP"
else
  printf 'FAIL  %-50s merged while unverified\n' "an outstanding verification blocks new merges"; FAILED=1
fi

# Resolves red on a later run: halt and escalate.
: > "$TMP/merged"; printf 'failure\n' > "$TMP/checks"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" GH_ISSUES="$TMP/issues" \
  "$SUT" --max 1 >/dev/null 2>&1
if [ -f "$LOOP_STATE/HALTED" ] && [ -s "$TMP/issues" ] && [ ! -s "$TMP/merged" ]; then
  printf 'ok    %-50s %s\n' "a red main on a later run halts and escalates" "HALTED"
else
  printf 'FAIL  %-50s halted=%s issue=%s\n' "a red main on a later run halts and escalates" \
    "$([ -f "$LOOP_STATE/HALTED" ] && echo y || echo n)" \
    "$([ -s "$TMP/issues" ] && echo y || echo n)"; FAILED=1
fi

# Resolves green: clears the record and lets work continue.
rm -f "$LOOP_STATE/HALTED"
printf 'aaaaaaa\n%s\n' "$(date +%s)" > "$LOOP_STATE/pending-verify"
: > "$TMP/merged"; printf 'success\n' > "$TMP/checks"
printf '%s\n' "$(pr 12 APPROVED CLEAN)" > "$TMP/prs"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" "$SUT" --max 1 >/dev/null 2>&1
if [ -s "$TMP/merged" ]; then
  printf 'ok    %-50s %s\n' "a green verification lets the next merge run" "MERGE"
else
  printf 'FAIL  %-50s did not merge after a green verification\n' "a green verification lets the next merge run"; FAILED=1
fi

# An unresolvable verification is itself a reason to stop.
rm -f "$LOOP_STATE/HALTED"
printf 'bbbbbbb\n%s\n' "$(( $(date +%s) - 99999 ))" > "$LOOP_STATE/pending-verify"
: > "$TMP/merged"; printf 'pending\n' > "$TMP/checks"
GH_PRS="$TMP/prs" GH_MERGED="$TMP/merged" GH_CHECKS="$TMP/checks" "$SUT" --max 1 >/dev/null 2>&1
if [ -f "$LOOP_STATE/HALTED" ]; then
  printf 'ok    %-50s %s\n' "a verification that never resolves halts" "HALTED"
else
  printf 'FAIL  %-50s did not halt on a stale verification\n' "a verification that never resolves halts"; FAILED=1
fi
rm -f "$LOOP_STATE/HALTED" "$LOOP_STATE/pending-verify"

echo
[ "$FAILED" -ne 0 ] && { echo "integrator tests FAILED"; exit 1; }
echo "the Integrator merges only what a reviewer approved and CI proved"
