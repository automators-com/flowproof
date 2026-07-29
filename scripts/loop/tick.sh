#!/usr/bin/env bash
# One tick of the fleet. This is the thing that makes the loops autonomous:
# until it existed, every turn began with a person typing a command.
#
# Single-instance by flock. A Builder turn runs for twenty to thirty minutes,
# far longer than the tick interval, so overlapping ticks are the normal case
# and must be a no-op rather than a second Builder.
#
# Order is deliberate: the Warden can halt, so it runs first; the Integrator
# clears the queue before more work is added to it; the Builder goes last
# because it is the long pole.
#
# Usage:  tick.sh
# Exit 0 always, unless the environment is unusable. A tick that finds nothing
# to do is a successful tick.
set -uo pipefail

REPO_ROOT="${FLOWPROOF_ROOT:-/home/flow/worktrees/flowproof/loopmain}"
STATE="$REPO_ROOT/.loop"
ENV_FILE="${FLOWPROOF_ENV:-$HOME/.config/flowproof-loop.env}"

# A Builder compiles a Rust workspace. `headless_chrome` alone wanted ~2.4 GB and
# OOM-killed six times on this box when other processes held the memory, so a
# turn started without headroom does not fail cheaply - it fails slowly, after
# paying for the tokens. Skip instead.
MIN_FREE_MB="${LOOP_MIN_FREE_MB:-3000}"
WARDEN_EVERY="${LOOP_WARDEN_EVERY_SECONDS:-3600}"

mkdir -p "$STATE"/{logs,locks}
log() { printf '%s  %s\n' "$(date -u +%H:%M:%SZ)" "$1" | tee -a "$STATE/logs/tick.log"; }

exec 9>"$STATE/locks/tick.lock"
flock -n 9 || { echo "a tick is still running; skipping"; exit 0; }

[ -f "$ENV_FILE" ] || { log "no $ENV_FILE; the fleet has no credentials"; exit 1; }
set -a
# shellcheck disable=SC1090  # the credentials file is deliberately outside the repo
. "$ENV_FILE"
set +a
export PATH="$HOME/.local/opt/node/bin:$HOME/.cargo/bin:$PATH"

if [ -f "$STATE/HALTED" ]; then
  log "halted: $(head -1 "$STATE/HALTED")"
  exit 0
fi

cd "$REPO_ROOT" || { log "cannot enter $REPO_ROOT"; exit 1; }

# Track main, but only from a detached worktree. Checking out over a branch
# would silently discard whatever it was holding - and the runner tree being
# detached is exactly how it is created, so a branch here means a human is
# using this directory for something and the tick should keep its hands off.
if git symbolic-ref -q HEAD >/dev/null; then
  log "$REPO_ROOT is on a branch, not detached; refusing to move it"
  exit 1
fi
git fetch -q origin main 2>/dev/null && git checkout -q --detach origin/main 2>/dev/null

turn() { # turn <role> [arg]
  local role="$1" arg="${2:-}"
  log "-> $role ${arg}"
  if ./scripts/loop/run.sh "$role" $arg >> "$STATE/logs/tick.log" 2>&1; then
    log "<- $role ok"
  else
    log "<- $role failed (see the role's own log)"
  fi
}

# --- the Warden, hourly. It can halt, so it goes first. --------------------
now="$(date +%s)"
last_warden="$(cat "$STATE/last-warden" 2>/dev/null || echo 0)"
if [ $(( now - last_warden )) -ge "$WARDEN_EVERY" ]; then
  printf '%s\n' "$now" > "$STATE/last-warden"
  turn warden
  [ -f "$STATE/HALTED" ] && { log "the Warden halted the fleet; stopping this tick"; exit 0; }
fi

# --- the Integrator, every tick. Clear the queue before adding to it. ------
turn integrator
[ -f "$STATE/HALTED" ] && { log "halted during integration; stopping this tick"; exit 0; }

# --- the Builder, if there is work and room to do it. ----------------------
ready="$(GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" gh issue list --repo "${FLOWPROOF_REPO:-automators-com/flowproof}" \
          --state open --label ready --search "no:assignee" --limit 1 \
          --json number --jq '.[0].number' 2>/dev/null || true)"

if [ -z "$ready" ] || [ "$ready" = "null" ]; then
  log "no unassigned 'ready' issue; the queue is dry"
  # A dry queue is the signal the corpus engine exists to answer. Until the
  # Prospector and Migrator run, it means a human must file work.
  exit 0
fi

free_mb="$(free -m | awk 'NR==2{print $7}')"
if [ "${free_mb:-0}" -lt "$MIN_FREE_MB" ]; then
  log "only ${free_mb}MB available, need ${MIN_FREE_MB}MB; not starting a Builder on #$ready"
  exit 0
fi

turn builder "$ready"
log "tick complete"
