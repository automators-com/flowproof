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
printf '%s\n' "$*" >> "${SANDBOX_CALLS:?}"
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
    "$SUT" baseline "$1" "$2" 2>/dev/null | tail -1
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

echo
if [ "$FAILED" -ne 0 ]; then echo "migrator tests FAILED"; exit 1; fi
echo "the Migrator establishes an oracle, and never on the host"
