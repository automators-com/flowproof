#!/usr/bin/env bash
# Tests for ratchets.sh.
#
# A ratchet that has never refused anything is indistinguishable from one that
# cannot. Each case below builds a real commit that degrades the gate in one
# specific way and asserts the ratchet catches it - and, just as importantly,
# that the legitimate near-miss beside it is allowed through.
#
# Work happens in a throwaway worktree so the repo under test is never touched.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)"

RATCHETS="$(pwd)/scripts/gate/ratchets.sh"
BASE_REF="$(git rev-parse HEAD)"
TMP="$(mktemp -d)"
WT="$TMP/wt"
FAILED=0

cleanup() {
  git worktree remove --force "$WT" >/dev/null 2>&1 || true
  rm -rf "$TMP"
}
trap cleanup EXIT

git worktree add --detach "$WT" "$BASE_REF" >/dev/null 2>&1 || {
  echo "could not create the test worktree"; exit 2; }

# scenario <REFUSE|ALLOW> <description> <shell that mutates the worktree>
scenario() {
  local want="$1" desc="$2" mutate="$3" got rev
  ( cd "$WT" && git reset -q --hard "$BASE_REF" && git clean -qfd ) || return
  ( cd "$WT" && eval "$mutate" ) || { printf 'SETUP  %s\n' "$desc"; FAILED=1; return; }
  ( cd "$WT" && git add -A && git -c user.name=t -c user.email=t@t commit -q -m "test: $desc" ) || {
      printf 'SETUP  %s (nothing to commit)\n' "$desc"; FAILED=1; return; }
  rev="$( cd "$WT" && git rev-parse HEAD )"
  if BASE="$BASE_REF" HEAD="$rev" "$RATCHETS" >/dev/null 2>&1; then got=ALLOW; else got=REFUSE; fi
  if [ "$got" = "$want" ]; then
    printf 'ok     %-46s %s\n' "$desc" "$got"
  else
    printf 'FAIL   %-46s got %s, wanted %s\n' "$desc" "$got" "$want"
    FAILED=1
  fi
}

echo "-- the suite may not shrink --"
scenario REFUSE "a rust test is deleted" \
  'f=$(git grep -lE "^\s*#\[test\]" -- "*.rs" | head -1);
   perl -0pi -e "s/^\s*#\[test\]\n\s*fn [a-z_0-9]+\(\)[^\n]*\{//m" "$f"'
scenario ALLOW  "a rust test is added" \
  'printf "\n#[test]\nfn ratchet_probe_added() { assert!(true); }\n" \
     >> crates/flowproof-trace/src/lib.rs'

echo "-- and may not be silenced --"
scenario REFUSE "#[ignore] is added" \
  'f=$(git grep -lE "^\s*#\[test\]" -- "*.rs" | head -1);
   perl -0pi -e "s/^(\s*)#\[test\]/\$1#[test]\n\$1#[ignore]/m" "$f"'
scenario REFUSE "a pytest skip is added" \
  'f=$(git grep -lE "^\s*def test_" -- "*.py" | head -1);
   perl -0pi -e "s/^(\s*)def test_/\$1\@pytest.mark.skip\n\$1def test_/m" "$f"'

echo "-- a cassette is ground truth --"
scenario REFUSE "a committed cassette is modified" \
  'f=$(git ls-files "*.trace.jsonl" | head -1); printf "\n" >> "$f"'
scenario ALLOW  "a new cassette is added" \
  'cp "$(git ls-files "*.trace.jsonl" | head -1)" examples/_ratchet_probe.trace.jsonl'

echo "-- the format and its docs move together --"
scenario REFUSE "schema changes without the doc" \
  'printf "\n" >> crates/flowproof-trace/schema/trace-v1.schema.json'
scenario ALLOW  "schema and doc change together" \
  'printf "\n" >> crates/flowproof-trace/schema/trace-v1.schema.json;
   printf "\n" >> docs/trace-format.md'

echo "-- size --"
scenario REFUSE "a diff over the cap" \
  'seq 1 500 | sed "s/^/\/\/ line /" >> crates/flowproof-trace/src/lib.rs'
scenario ALLOW  "a diff under the cap" \
  'seq 1 20 | sed "s/^/\/\/ line /" >> crates/flowproof-trace/src/lib.rs'
scenario ALLOW  "a large generated Cargo.lock bump is exempt" \
  'seq 1 500 | sed "s/^/# generated /" >> Cargo.lock'

echo
if [ "$FAILED" -ne 0 ]; then echo "ratchet tests FAILED"; exit 1; fi
echo "every ratchet refuses what it is meant to, and allows what it is not"
