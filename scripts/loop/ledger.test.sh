#!/usr/bin/env bash
# Tests for the gap ledger.
#
# The frequency gate is the whole point, and the case that matters is the one it
# must REFUSE: three observations inside one repository is one project's idiom,
# not a gap in flowproof. A gate that counts observations rather than distinct
# repositories would promote it, and the ledger would become a way of laundering
# a single opinion into a feature request.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

SUT="$(pwd)/scripts/loop/ledger.py"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
FAILED=0

fresh() {
  rm -rf "$TMP/w"; mkdir -p "$TMP/w/docs/loop"
  printf '# the gap ledger\n# a preserved header comment\nversion: 1\ngaps: []\n' \
    > "$TMP/w/docs/loop/ledger.yaml"
}
ledger() { ( cd "$TMP/w" && python3 "$SUT" "$@" ); }
status_of() { python3 -c "
import yaml,sys
d=yaml.safe_load(open('$TMP/w/docs/loop/ledger.yaml'))
g=[x for x in (d.get('gaps') or []) if x['key']=='$1']
print(g[0]['status'] if g else 'absent')"; }

ok()  { printf 'ok    %-52s %s\n' "$1" "$2"; }
bad() { printf 'FAIL  %-52s %s\n' "$1" "$2"; FAILED=1; }

echo "-- the gate counts distinct repositories, not observations --"
fresh
ledger record k org/a aaa "t1" >/dev/null
ledger record k org/a bbb "t2" >/dev/null
ledger record k org/a ccc "t3" >/dev/null
s="$(status_of k)"
[ "$s" = recording ] && ok "3 observations in ONE repo does not promote" "$s" \
                     || bad "3 observations in ONE repo does not promote" "$s"

ledger record k org/b ddd "t4" >/dev/null
s="$(status_of k)"
[ "$s" = recording ] && ok "a second repo is still not enough" "$s" \
                     || bad "a second repo is still not enough" "$s"

ledger record k org/c eee "t5" >/dev/null
s="$(status_of k)"
[ "$s" = eligible ] && ok "the third distinct repo promotes it" "$s" \
                    || bad "the third distinct repo promotes it" "$s"

echo "-- evidence has to be evidence --"
fresh
if ledger record k org/a "" "t1" >/dev/null 2>&1; then
  bad "an observation without a SHA is refused" "accepted"
else
  ok "an observation without a SHA is refused" "REFUSED"
fi

fresh
ledger record k org/a aaa "same test" >/dev/null
ledger record k org/a aaa "same test" >/dev/null
n="$(python3 -c "
import yaml
d=yaml.safe_load(open('$TMP/w/docs/loop/ledger.yaml'))
print(len(d['gaps'][0]['observed']))")"
[ "$n" = 1 ] && ok "the same observation is not counted twice" "$n" \
             || bad "the same observation is not counted twice" "$n"

echo "-- a declined gap stays declined --"
fresh
ledger record k org/a aaa t1 >/dev/null
python3 -c "
import yaml
p='$TMP/w/docs/loop/ledger.yaml'
d=yaml.safe_load(open(p)); d['gaps'][0]['status']='declined'
d['gaps'][0]['declined_why']='out of scope per CHARTER.md 3'
yaml.safe_dump(d, open(p,'w'), sort_keys=False)"
ledger record k org/b bbb t2 >/dev/null
ledger record k org/c ccc t3 >/dev/null
s="$(status_of k)"
[ "$s" = declined ] && ok "a declined gap is not re-proposed" "$s" \
                    || bad "a declined gap is not re-proposed" "$s"

echo "-- promotion is gated, and dry by default --"
fresh
ledger record k org/a aaa t1 >/dev/null
out="$(ledger promote 2>&1)"
case "$out" in *"0 gap(s) promoted"*) ok "an ineligible gap is not promoted" "0" ;;
               *) bad "an ineligible gap is not promoted" "$out" ;; esac
ledger record k org/b bbb t2 >/dev/null; ledger record k org/c ccc t3 >/dev/null
out="$(ledger promote 2>&1)"
case "$out" in *"would open an issue"*) ok "an eligible gap is promoted, dry" "dry-run" ;;
               *) bad "an eligible gap is promoted, dry" "$out" ;; esac

echo "-- the header survives a write --"
grep -q "a preserved header comment" "$TMP/w/docs/loop/ledger.yaml" \
  && ok "the explanatory header is preserved" "kept" \
  || bad "the explanatory header is preserved" "lost"

echo "-- the ranked table --"
out="$(ledger report 2>&1)"
case "$out" in *repos*capability*) ok "report prints the ranked table" "ok" ;;
               *) bad "report prints the ranked table" "$out" ;; esac

echo
[ "$FAILED" -ne 0 ] && { echo "ledger tests FAILED"; exit 1; }
echo "the ledger promotes only what three separate projects needed"
