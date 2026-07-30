#!/usr/bin/env bash
# Tests for the Migrator's baseline harness.
#
# The property that matters is not that it runs a suite - it is that it NEVER
# runs corpus code outside the sandbox. A corpus repository's install executes
# arbitrary code, and this box holds SSH deploy keys and a GitHub token: the one
# failure here that a revert cannot undo.
#
# Drives the real script with a stub sandbox that records how it was called.
set -uo pipefail
cd "$(git rev-parse --show-toplevel)" || exit 2

SUT="$(pwd)/scripts/loop/migrate.sh"
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
export CORPUS_DIR="$TMP/corpus"
FAILED=0

cat > "$TMP/sandbox.sh" <<'STUB'
#!/usr/bin/env bash
# ONE LINE PER CALL. Several of the commands under test are multi-line, so
# recording them verbatim spread a single call over six physical lines and the
# `sed -n 2p` addressing below silently read the middle of the previous call.
# The existing head -1 / tail -1 assertions had the same latent hole and passed
# by luck - the substring they grep for happened to sit on the first line.
printf '%s\n' "${*//$'\n'/ }" >> "${SANDBOX_CALLS:?}"
# SANDBOX_FAIL_ON fails only the call whose arguments contain it, so a test can
# break exactly one phase. A uniform SANDBOX_RC cannot express "the runner would
# not start but the install was fine", which is the case that matters most.
if [ -n "${SANDBOX_FAIL_ON:-}" ]; then
  case "$*" in *"$SANDBOX_FAIL_ON"*) exit "${SANDBOX_FAIL_RC:-1}" ;; esac
  exit 0
fi
exit "${SANDBOX_RC:-0}"
STUB
chmod +x "$TMP/sandbox.sh"

# A local repository to fetch from, so no network is involved.
seed() {
  local src="$TMP/src"
  rm -rf "$src"; mkdir -p "$src"
  git init -q "$src"
  # Fetching an arbitrary SHA needs the serving side to allow it; GitHub does,
  # a bare local repository does not unless told to.
  git -C "$src" config uploadpack.allowAnySHA1InWant true
  git -C "$src" config uploadpack.allowReachableSHA1InWant true
  [ -n "${1:-}" ] && : > "$src/$1"
  git -C "$src" add -A >/dev/null 2>&1
  git -C "$src" -c user.name=t -c user.email=t@t commit -q --allow-empty -m seed
  git -C "$src" rev-parse HEAD
}

run() {
  : > "$TMP/calls"
  rm -rf "$CORPUS_DIR"
  SANDBOX="$TMP/sandbox.sh" SANDBOX_CALLS="$TMP/calls" SANDBOX_RC="${RC:-0}" \
    SANDBOX_FAIL_ON="${FAIL_ON:-}" SANDBOX_FAIL_RC="${FAIL_RC:-1}" \
    "$SUT" baseline "$1" "$2" 2>/dev/null | tail -1
}

# Same, but keeping the human-readable progress output instead of the verdict.
why() {
  : > "$TMP/calls"
  rm -rf "$CORPUS_DIR"
  SANDBOX="$TMP/sandbox.sh" SANDBOX_CALLS="$TMP/calls" SANDBOX_RC="${RC:-0}" \
    SANDBOX_FAIL_ON="${FAIL_ON:-}" SANDBOX_FAIL_RC="${FAIL_RC:-1}" \
    "$SUT" baseline "$1" "$2" 2>&1 >/dev/null
}

ok()  { printf 'ok    %-52s %s\n' "$1" "$2"; }
bad() { printf 'FAIL  %-52s %s\n' "$1" "$2"; FAILED=1; }

echo "-- a pinned SHA is not optional --"
if "$SUT" baseline org/repo >/dev/null 2>&1; then
  bad "an unpinned candidate is refused" "accepted"
else
  ok "an unpinned candidate is refused" "REFUSED"
fi

echo "-- corpus code never runs on the host --"
sha="$(seed package.json)"
out="$(SANDBOX=/nonexistent/sandbox.sh "$SUT" baseline "$TMP/src" "$sha" 2>&1 | tail -1)"
case "$out" in
  *"refusing to run corpus code on the host"*) ok "a missing sandbox refuses the run" "REFUSED" ;;
  *) bad "a missing sandbox refuses the run" "got: ${out:0:44}" ;;
esac

echo "-- what it recognises, and what it declines --"
for m in package.json pyproject.toml Cargo.toml; do
  sha="$(seed "$m")"; v="$(run "$TMP/src" "$sha")"
  if [ "$v" = PASS ]; then ok "$m is recognised" "$v"; else bad "$m is recognised" "$v"; fi
done
sha="$(seed)"; v="$(run "$TMP/src" "$sha")"
if [ "$v" = UNRUNNABLE ]; then ok "no manifest declines" "$v"; else bad "no manifest declines" "$v"; fi

echo "-- a run that did not pass never reports PASS --"
sha="$(seed package.json)"; v="$(RC=1 run "$TMP/src" "$sha")"
if [ "$v" != PASS ]; then ok "a failed run never reports PASS" "$v"; else bad "a failed run never reports PASS" "$v"; fi

echo "-- install gets egress, the suite does not --"
sha="$(seed package.json)"; run "$TMP/src" "$sha" >/dev/null
if head -1 "$TMP/calls" | grep -q -- "--phase install" \
   && tail -1 "$TMP/calls" | grep -q -- "--phase replay"; then
  ok "phases are install then replay" "ordered"
else
  bad "phases are install then replay" "$(tr '\n' '|' < "$TMP/calls" | cut -c1-52)"
fi

echo "-- dependencies must survive into the test container --"
# Install and test are separate containers and only /work persists. A Migrator
# measured one phase printing `pytest 9.1.1` and the next `No module named
# pytest`, so baseline reported FAIL for a repository whose suite passes - and
# FAIL is treated as a usable oracle, so that would have been filed as a false
# green. The stub records the commands, so this asserts the shape that makes
# persistence possible rather than re-running containers.
for m in pyproject.toml Cargo.toml; do
  sha="$(seed "$m")"; run "$TMP/src" "$sha" >/dev/null
  inst="$(head -1 "$TMP/calls")"; tst="$(tail -1 "$TMP/calls")"
  case "$m" in
    pyproject.toml)
      if printf '%s' "$inst" | grep -q '/work/.venv' \
         && printf '%s' "$tst" | grep -q '/work/.venv/bin/python'; then
        ok "python installs into /work and tests with it" "venv"
      else
        bad "python installs into /work and tests with it" "install=${inst:0:40}"
      fi
      if printf '%s' "$inst" | grep -q '|| true'; then
        bad "a failed python install is not swallowed" "'|| true' is back"
      else
        ok "a failed python install is not swallowed" "no '|| true'"
      fi ;;
    Cargo.toml)
      if printf '%s' "$inst" | grep -q 'CARGO_HOME=/work' \
         && printf '%s' "$tst" | grep -q 'CARGO_HOME=/work'; then
        ok "rust uses a CARGO_HOME under /work" "persisted"
      else
        bad "rust uses a CARGO_HOME under /work" "install=${inst:0:40}"
      fi ;;
  esac
done

echo "-- a suite that never started is a DECLINE, not a verdict --"
# The one that started all of this. Measured 2026-07-30 against
# cypress-io/cypress-example-kitchensink: `npm ci` succeeded, Cypress could not
# find its browser (cached under $HOME, which is the image and does not survive
# into the replay container), and `baseline` printed FAIL.
#
# FAIL is a USABLE ORACLE by this script's contract. So a suite that executed
# zero tests became the thing flowproof was measured against: a migration that
# passed would be filed as a FALSE GREEN - priority 1, the finding nobody
# double-checks - and one that failed would be filed as agreement. Both records
# fabricated. UNRUNNABLE is the only honest answer.
sha="$(seed package.json)"; v="$(FAIL_ON='cypress verify' run "$TMP/src" "$sha")"
if [ "$v" = UNRUNNABLE ]; then ok "a runner that will not start declines" "$v"
else bad "a runner that will not start declines" "got $v, wanted UNRUNNABLE"; fi

sha="$(seed pyproject.toml)"; v="$(FAIL_ON='import pytest' run "$TMP/src" "$sha")"
if [ "$v" = UNRUNNABLE ]; then ok "and the same holds for python" "$v"
else bad "and the same holds for python" "got $v, wanted UNRUNNABLE"; fi

# The opposite direction, which matters just as much: a suite that genuinely
# runs and genuinely fails is still an oracle. A probe that over-declines would
# silently empty the corpus of every FAIL/FAIL agreement, and nothing would
# report that either.
sha="$(seed package.json)"; v="$(FAIL_ON='npm test' run "$TMP/src" "$sha")"
if [ "$v" = FAIL ]; then ok "a suite that runs and fails is still an oracle" "$v"
else bad "a suite that runs and fails is still an oracle" "got $v, wanted FAIL"; fi

echo "-- the probe is asked under the conditions the suite will meet --"
sha="$(seed package.json)"; run "$TMP/src" "$sha" >/dev/null
probe="$(sed -n 2p "$TMP/calls")"; suite="$(sed -n 3p "$TMP/calls")"
if printf '%s' "$probe" | grep -q -- '--phase replay'; then
  ok "the probe runs with egress denied" "replay"
else
  bad "the probe runs with egress denied" "${probe:0:44}"
fi
if printf '%s' "$probe" | grep -q 'cypress verify' \
   && printf '%s' "$suite" | grep -q 'npm test'; then
  ok "and BEFORE the suite, not after it" "ordered"
else
  bad "and BEFORE the suite, not after it" "probe=${probe:0:40}"
fi

echo "-- a browser cache must survive the phase boundary too --"
# node_modules lands in /work by accident of npm's layout. Cypress's browser
# does not - it goes to $HOME, which is the image. Same class as the venv and
# CARGO_HOME, and it had no equivalent pin until it produced a false oracle.
sha="$(seed package.json)"; run "$TMP/src" "$sha" >/dev/null
missing=""
for line in 1 2 3; do
  printf '%s' "$(sed -n "${line}p" "$TMP/calls")" | grep -q 'CYPRESS_CACHE_FOLDER=/work' || missing="$missing $line"
done
if [ -z "$missing" ]; then
  ok "install, probe and suite share a /work cypress cache" "pinned"
else
  bad "install, probe and suite share a /work cypress cache" "missing on call(s):$missing"
fi

echo "-- a Cypress suite needs an image that can actually run a browser --"
# node:*-slim has no Xvfb, so `cypress verify` fails there and no verdict is
# trustworthy. Measured 2026-07-30: with cypress/base the same probe passes.
seed_cypress() {
  local src="$TMP/src"
  rm -rf "$src"; mkdir -p "$src"
  git init -q "$src"
  git -C "$src" config uploadpack.allowAnySHA1InWant true
  git -C "$src" config uploadpack.allowReachableSHA1InWant true
  printf '{"devDependencies":{"cypress":"15.19.0"}}\n' > "$src/package.json"
  git -C "$src" add -A >/dev/null 2>&1
  git -C "$src" -c user.name=t -c user.email=t@t commit -q -m seed
  git -C "$src" rev-parse HEAD
}
sha="$(seed_cypress)"; run "$TMP/src" "$sha" >/dev/null
if grep -q 'cypress/base' "$TMP/calls"; then
  ok "a repo depending on cypress gets a browser-capable image" "cypress/base"
else
  bad "a repo depending on cypress gets a browser-capable image" "$(head -1 "$TMP/calls" | cut -c1-46)"
fi
# The other direction. The Cypress image is 738 MB against 233 MB, so a project
# that does not need it must not pay for it on every baseline.
sha="$(seed package.json)"; run "$TMP/src" "$sha" >/dev/null
if grep -q 'node:22-bookworm-slim' "$TMP/calls" && ! grep -q 'cypress/base' "$TMP/calls"; then
  ok "and a plain node repo does not" "node slim"
else
  bad "and a plain node repo does not" "$(head -1 "$TMP/calls" | cut -c1-46)"
fi

echo "-- running out of memory is not 'timed out' --"
# Both are UNRUNNABLE. Saying the wrong one sends someone hunting a slow test
# when the truth is a heap limit, which is a different fix on a different box.
# 134 is 128+6: V8 aborting. Measured on cypress-example-kitchensink.
sha="$(seed package.json)"; v="$(FAIL_ON='npm test' FAIL_RC=134 run "$TMP/src" "$sha")"
if [ "$v" = UNRUNNABLE ]; then ok "an OOM-killed suite is a decline" "$v"
else bad "an OOM-killed suite is a decline" "got $v"; fi

out="$(FAIL_ON='npm test' FAIL_RC=134 why "$TMP/src" "$sha")"
if printf '%s' "$out" | grep -qi "out of memory\|signal 6"; then
  ok "and says so, rather than blaming the clock" "named"
else
  bad "and says so, rather than blaming the clock" "$(printf '%s' "$out" | tail -1 | cut -c1-46)"
fi
out="$(FAIL_ON='npm test' FAIL_RC=124 why "$TMP/src" "$sha")"
if printf '%s' "$out" | grep -qi "timed out"; then
  ok "a real timeout still reads as a timeout" "named"
else
  bad "a real timeout still reads as a timeout" "$(printf '%s' "$out" | tail -1 | cut -c1-46)"
fi

echo
if [ "$FAILED" -ne 0 ]; then echo "migrator tests FAILED"; exit 1; fi
echo "the Migrator establishes an oracle, and never on the host"
