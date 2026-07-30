#!/usr/bin/env bash
# Tests for the fleet's tick.
#
# The property that matters is not that it runs roles - it is that it NEVER
# runs them against a tree that is not what it claims to be. The runner tree is
# detached and only tick.sh advances it, so a tick that cannot track main and
# carries on regardless executes the whole fleet from a stale commit: an old
# charter, old role prompts, and old gate scripts, with nothing in the log
# saying so. That is worse than a tick that does nothing.
#
# Drives the real script against a real temporary git repository, with stubs on
# PATH for everything that would leave the box.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

SUT="$(pwd)/scripts/loop/tick.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
FAILED=0

ok()  { printf 'ok    %-54s %s\n' "$1" "$2"; }
bad() { printf 'FAIL  %-54s %s\n' "$1" "$2"; FAILED=1; }

# `gh` must never be reached; if a test gets far enough to call it, the tick has
# already gone further than it should have.
cat > "$TMP/gh" <<'STUB'
#!/usr/bin/env bash
printf 'gh %s\n' "$*" >> "${GH_CALLS:-/dev/null}"
echo ""
STUB
chmod +x "$TMP/gh"; export PATH="$TMP:$PATH"

# An "upstream" main, and a runner tree detached onto it - exactly how the real
# one is created.
UP="$TMP/upstream"
git init -q --bare "$UP"
git -C "$UP" symbolic-ref HEAD refs/heads/main
SEED="$TMP/seed"
git init -q "$SEED"
mkdir -p "$SEED/scripts/loop"
cat > "$SEED/scripts/loop/run.sh" <<'STUB'
#!/usr/bin/env bash
printf 'run.sh %s\n' "$*" >> "${ROLE_CALLS:?}"
STUB
chmod +x "$SEED/scripts/loop/run.sh"
# tracked.txt CHANGES between the two commits. That detail is the test: git
# only refuses a checkout when the dirty file differs across it, so a fixture
# whose file is identical in both commits proves nothing - the checkout
# succeeds and carries the edit forward. The first version of this test made
# exactly that mistake and reported the fix as broken.
printf 'one\n' > "$SEED/tracked.txt"
git -C "$SEED" add -A
git -C "$SEED" -c user.name=t -c user.email=t@t commit -qm v1
git -C "$SEED" branch -M main
git -C "$SEED" remote add origin "$UP"
git -C "$SEED" push -q origin main

# A second upstream commit, so "tracked main" and "stale" are distinguishable.
printf 'two\n' > "$SEED/tracked.txt"
git -C "$SEED" add -A
git -C "$SEED" -c user.name=t -c user.email=t@t commit -qm v2
git -C "$SEED" push -q origin main
V2="$(git -C "$SEED" rev-parse HEAD)"
V1="$(git -C "$SEED" rev-parse HEAD~1)"

# A runner tree pinned at v1, so a correct tick must advance it to v2.
runner() {
  local r="$TMP/runner"
  rm -rf "$r"
  git clone -q "$UP" "$r"
  cp "$SEED/scripts/loop/run.sh" "$r/scripts/loop/run.sh" 2>/dev/null || true
  git -C "$r" checkout -q --detach "$V1"
  printf '%s' "$r"
}

tick() { # tick <runner-dir> -> exit code, with output in $TMP/out
  : > "$TMP/roles"; : > "$TMP/ghcalls"
  ROLE_CALLS="$TMP/roles" GH_CALLS="$TMP/ghcalls" \
  FLOWPROOF_ROOT="$1" FLOWPROOF_ENV="$TMP/env" \
  LOOP_WARDEN_EVERY_SECONDS=999999 LOOP_MIN_FREE_MB=0 \
    "$SUT" > "$TMP/out" 2>&1
  echo $?
}

printf 'FLOWPROOF_LOOP_TOKEN=stub\n' > "$TMP/env"

echo "-- a tick that cannot track main must not run anything --"
# The case measured on 2026-07-30: an uncommitted file in the runner tree makes
# `git checkout` refuse. The tick used to swallow that and run every role from
# the stale commit.
r="$(runner)"
printf 'a local edit\n' > "$r/tracked.txt"
rc="$(tick "$r")"
at="$(git -C "$r" rev-parse HEAD)"
roles="$(wc -l < "$TMP/roles")"

if [ "$rc" -ne 0 ]; then ok "a blocked checkout exits non-zero" "rc=$rc"
else bad "a blocked checkout exits non-zero" "rc=$rc - systemd would call this a success"; fi

if [ "$roles" -eq 0 ]; then ok "and runs NO role from the stale tree" "0 roles"
else bad "and runs NO role from the stale tree" "ran $roles: $(tr '\n' ' ' < "$TMP/roles")"; fi

if [ "$at" = "$V1" ]; then ok "the tree is left where it was, not forced" "still v1"
else bad "the tree is left where it was, not forced" "moved to ${at:0:8}"; fi

if grep -q "uncommitted changes" "$TMP/out"; then ok "the log names the actual reason" "reported"
else bad "the log names the actual reason" "$(head -3 "$TMP/out" | tr '\n' '|')"; fi

# Assert on the PERSISTENT log, not on stdout. git's own "your local changes
# would be overwritten by checkout: tracked.txt" lands on stderr and is visible
# either way, so grepping the console output passes whether or not the tick
# reports anything itself - it tests git, not this script. What matters is that
# the name survives into .loop/logs/tick.log, which is what a human reads a day
# later. Mutation-tested: removing the report leaves this the only failure.
if grep -q "tracked.txt" "$r/.loop/logs/tick.log" 2>/dev/null; then
  ok "and names the file in the persistent log" "reported"
else bad "and names the file in the persistent log" "not in tick.log"; fi

# Losing the diff is how the explanation for the dirty tree disappears. The
# runner tree is only ever advanced by this script, so anything uncommitted in
# it is unexplained by construction.
if ls "$r"/.loop/logs/*runner-tree-blocked.diff >/dev/null 2>&1; then
  ok "the blocking diff is preserved for a human" "saved"
else
  bad "the blocking diff is preserved for a human" "no diff saved"
fi
if grep -q "a local edit" "$r/tracked.txt" 2>/dev/null; then ok "and the working file itself is untouched" "intact"
else bad "and the working file itself is untouched" "it was discarded"; fi

echo "-- the healthy path still advances and still runs --"
r="$(runner)"
rc="$(tick "$r")"
at="$(git -C "$r" rev-parse HEAD)"
if [ "$at" = "$V2" ]; then ok "a clean tree is advanced to origin/main" "v2"
else bad "a clean tree is advanced to origin/main" "at ${at:0:8}, wanted ${V2:0:8}"; fi
if grep -q 'integrator' "$TMP/roles"; then ok "and roles do run" "integrator dispatched"
else bad "and roles do run" "roles=[$(tr '\n' ' ' < "$TMP/roles")] rc=$rc"; fi

echo "-- the pre-existing guards still hold --"
r="$(runner)"; git -C "$r" checkout -q -B somebranch
rc="$(tick "$r")"
if [ "$rc" -ne 0 ] && grep -q "not detached" "$TMP/out"; then
  ok "a tree on a branch is refused" "rc=$rc"
else bad "a tree on a branch is refused" "rc=$rc"; fi

r="$(runner)"; mkdir -p "$r/.loop"; echo "halted by a test" > "$r/.loop/HALTED"
rc="$(tick "$r")"
if [ "$(wc -l < "$TMP/roles")" -eq 0 ]; then ok "HALTED still stops the fleet" "0 roles"
else bad "HALTED still stops the fleet" "ran $(wc -l < "$TMP/roles")"; fi

echo
if [ "$FAILED" -ne 0 ]; then echo "tick tests FAILED"; exit 1; fi
echo "the tick runs roles only against the tree it says it is running against"
