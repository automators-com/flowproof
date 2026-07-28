#!/usr/bin/env bash
# Tests for constitution-check.sh. Drives the real script via CHANGED_FILES so
# these cases cannot drift from the logic they check.
#
# Run directly, or via the `constitution` workflow which runs it on every PR.
cd "$(dirname "$0")"
SUT=./constitution-check.sh
FAILED=0

t() {  # t <ALLOW|BLOCK> <description> <author> <changed files...>
  local want="$1" desc="$2" author="$3"; shift 3
  local got
  if AUTHOR="$author" CHANGED_FILES="$*" "$SUT" >/dev/null 2>&1; then
    got=ALLOW
  else
    got=BLOCK
  fi
  if [ "$got" = "$want" ]; then
    printf 'ok    %-50s %s\n' "$desc" "$got"
  else
    printf 'FAIL  %-50s got %s, wanted %s\n' "$desc" "$got" "$want"
    FAILED=1
  fi
}

echo "-- a loop must not reach the things that constrain it --"
t BLOCK "loop edits CI"                        loop-bot .github/workflows/ci.yml
t BLOCK "loop edits the constitution job"      loop-bot .github/workflows/constitution.yml
t BLOCK "loop edits the charter"               loop-bot CHARTER.md
t BLOCK "loop edits a gate script"             loop-bot scripts/gate/token-scope-check.sh
t BLOCK "loop edits this very check"           loop-bot scripts/gate/constitution-check.sh
t BLOCK "loop edits the gate's tests"          loop-bot scripts/gate/constitution-check.test.sh
t BLOCK "loop edits the branch-protection json" loop-bot scripts/gate/protection.json
t BLOCK "loop edits CODEOWNERS"                loop-bot CODEOWNERS
t BLOCK "loop edits its own role prompt"       loop-bot scripts/loop/roles/builder.md
t BLOCK "loop edits the loop runner"           loop-bot scripts/loop/run.sh
t BLOCK "the Builder edits its own prompt"     AutomatorsAgent scripts/loop/roles/builder.md
t BLOCK "loop buries it among normal files"    loop-bot crates/a/src/lib.rs docs/b.md .github/workflows/ci.yml

echo "-- but must stay free to do its actual work --"
t ALLOW "loop edits engine code"               loop-bot crates/flowproof-trace/src/cassette.rs
t ALLOW "loop edits a non-gate script"         loop-bot scripts/demo/fake_model.py
t ALLOW "loop edits docs"                      loop-bot docs/trace-format.md
t ALLOW "loop adds a cassette"                 loop-bot tests/flowproof/new.cassette.json
t ALLOW "loop touches nothing protected"       loop-bot README.md

echo "-- a human bypasses --"
t ALLOW "human edits CI"                       AminChirazi .github/workflows/ci.yml
t ALLOW "human edits the charter"              AminChirazi CHARTER.md
t ALLOW "human edits a gate script"            AminChirazi scripts/gate/constitution-check.sh

echo "-- near-misses that must NOT be protected --"
t ALLOW "a doc merely named like the gate"      loop-bot docs/gate-notes.md
t ALLOW "a charter copy in a subdirectory"      loop-bot docs/CHARTER.md
t ALLOW "a workflow-like path outside .github"  loop-bot crates/x/workflows/mod.rs
t ALLOW "a gate-like path outside scripts/"     loop-bot crates/gate/src/lib.rs

echo "-- an unknown identity is treated as a loop (fail-closed) --"
t BLOCK "unknown actor edits CI"               some-new-bot .github/workflows/ci.yml
t BLOCK "a bot account edits CI"               dependabot[bot] .github/workflows/ci.yml

echo "-- misconfiguration must not silently pass --"
if CHANGED_FILES="CHARTER.md" ./constitution-check.sh >/dev/null 2>&1; then
  printf 'FAIL  %-50s AUTHOR unset was allowed\n' "unset AUTHOR is rejected"; FAILED=1
else
  printf 'ok    %-50s BLOCK\n' "unset AUTHOR is rejected"
fi

exit "$FAILED"
