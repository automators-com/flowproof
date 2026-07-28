#!/usr/bin/env bash
# The loop runner. One invocation = one turn of one role.
#
# Everything dangerous is in the preflight, and the preflight is fail-closed:
# any check that cannot prove itself stops the turn. A loop that declines to run
# costs a cycle; a loop that runs without its gate costs a repository.
#
# State lives in .loop/ (gitignored local scratch). The prompts and this runner
# live in scripts/loop/, which is constitution-protected: a Builder that could
# edit its own instructions is not bounded by them.
#
# Usage:
#   scripts/loop/run.sh <role> [issue-number]
#   roles: builder | prospector | migrator | ledger | warden
#
# Exit 0 = turn completed, or cleanly declined. Non-zero = the turn failed.
set -euo pipefail

ROLE="${1:-}"
ARG="${2:-}"
REPO_ROOT="$(git rev-parse --show-toplevel)"
STATE="$REPO_ROOT/.loop"
ROLES="$REPO_ROOT/scripts/loop/roles"
BUILDER_LOGIN="AutomatorsAgent"

# Bounds. A loop that never stops is a loop that cannot be reasoned about.
MAX_TURNS="${LOOP_MAX_TURNS:-60}"
MAX_ATTEMPTS="${LOOP_MAX_ATTEMPTS:-3}"

mkdir -p "$STATE"/{locks,logs,attempts}

die()  { printf '\033[31mstop\033[0m  %s\n' "$1" >&2; exit 1; }
say()  { printf '\033[36mloop\033[0m  %s\n' "$1"; }
halt() { printf '\033[33mhalt\033[0m  %s\n' "$1"; exit 0; }

[ -n "$ROLE" ] || die "usage: run.sh <role> [issue-number]"
[ -f "$ROLES/$ROLE.md" ] || die "no such role: $ROLE (see scripts/loop/roles/)"

# --- preflight ---------------------------------------------------------------
# 1. The circuit breaker, checked before anything else so the Warden can stop
#    the fleet without racing a loop that is already spending money.
if [ -f "$STATE/HALTED" ]; then
  halt "the fleet is halted: $(head -1 "$STATE/HALTED" 2>/dev/null || echo 'no reason recorded')
      clear it with: rm .loop/HALTED"
fi

# 2. The credential must not be able to edit the gate that judges it, nor bypass
#    the review that judges it. Both are checked by the gate script.
[ -n "${FLOWPROOF_LOOP_TOKEN:-}" ] || die "FLOWPROOF_LOOP_TOKEN is unset.
      Loops must never fall back to an interactive gh login - see
      scripts/gate/README.md for the token to mint."
"$REPO_ROOT/scripts/gate/token-scope-check.sh" >/dev/null \
  || die "the loop credential failed its scope check; run it directly to see why"

# 3. The model runtime. Without it the role prompt cannot be executed at all.
command -v claude >/dev/null || die "the claude CLI is not installed"
claude -p "ok" --output-format text >/dev/null 2>&1 \
  || die "the claude CLI is not authenticated (ANTHROPIC_API_KEY, or 'claude /login')"

# 4. Role tooling, checked here rather than discovered halfway through a turn
#    where the failure would look like a code problem.
case "$ROLE" in
  builder)  command -v cargo  >/dev/null || die "cargo is required to verify a build" ;;
  migrator) command -v podman >/dev/null || die "podman is required: corpus code is untrusted
      and must never share a trust domain with this box's credentials" ;;
esac

say "preflight clear; role=$ROLE"

# --- claiming ----------------------------------------------------------------
# A lock DIRECTORY, not a file: mkdir is atomic, so two loops cannot both believe
# they won. The GitHub assignee is the visible claim; this is the one that
# actually prevents a local collision.
claim() { # claim <key>
  local key="$1" lock="$STATE/locks/$key" age
  if ! mkdir "$lock" 2>/dev/null; then
    age=$(( $(date +%s) - $(stat -c %Y "$lock" 2>/dev/null || date +%s) ))
    if [ "$age" -gt 7200 ]; then
      say "reclaiming a stale lock on $key (${age}s old)"
      rm -rf "$lock" && mkdir "$lock"
    else
      return 1
    fi
  fi
  printf '%s\n' "$$" > "$lock/pid"
  CLAIMED="$lock"
}
release() { [ -n "${CLAIMED:-}" ] && rm -rf "$CLAIMED"; CLAIMED=""; return 0; }
trap release EXIT

# --- attempt budget ----------------------------------------------------------
# Three failures and the issue goes to a human. Unbounded retry burns tokens to
# produce the same wrong answer with more confidence each time.
attempts_of()  { cat "$STATE/attempts/$1" 2>/dev/null || echo 0; }
bump_attempt() { echo $(( $(attempts_of "$1") + 1 )) > "$STATE/attempts/$1"; }

# --- run ---------------------------------------------------------------------
run_role() { # run_role <workdir> <context>
  local workdir="$1" context="$2" log rc
  log="$STATE/logs/$(date -u +%Y%m%dT%H%M%SZ)-$ROLE.log"

  say "running $ROLE in $workdir (log: ${log#"$REPO_ROOT"/})"

  # The role prompt is a SYSTEM prompt, so per-turn task context cannot overwrite
  # the operating rules. Output goes to a file and the exit code is read
  # directly: piping would let the pipe's status mask the real one, which has
  # already produced three false results in this repository.
  set +e
  ( cd "$workdir" \
    && GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" \
       claude -p "$context" \
         --append-system-prompt "$(cat "$ROLES/$ROLE.md")" \
         --max-turns "$MAX_TURNS" \
         --permission-mode acceptEdits \
         --output-format text ) > "$log" 2>&1
  rc=$?
  set -e

  tail -20 "$log"
  return $rc
}

case "$ROLE" in
  builder)
    issue="$ARG"
    if [ -z "$issue" ]; then
      issue="$(GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" gh issue list --state open \
                 --label ready --search "no:assignee" --limit 1 \
                 --json number -q '.[0].number' 2>/dev/null || true)"
    fi
    [ -n "$issue" ] && [ "$issue" != "null" ] \
      || halt "no unassigned 'ready' issue; nothing to build"

    n="$(attempts_of "issue-$issue")"
    if [ "$n" -ge "$MAX_ATTEMPTS" ]; then
      say "issue #$issue has failed $n times; handing it to a human"
      GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" gh issue edit "$issue" \
        --add-label needs-human --remove-label ready >/dev/null 2>&1 || true
      halt "issue #$issue escalated after $n attempts"
    fi

    claim "issue-$issue" || halt "issue #$issue is already claimed"
    bump_attempt "issue-$issue"

    wt="$HOME/worktrees/flowproof/issue-$issue"
    branch="loop/issue-$issue"
    if [ ! -d "$wt" ]; then
      git -C "$REPO_ROOT" fetch origin main --quiet
      git -C "$REPO_ROOT" worktree add "$wt" -b "$branch" origin/main >/dev/null
    fi

    body="$(GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" gh issue view "$issue" \
             --json title,body -q '"# " + .title + "\n\n" + .body' 2>/dev/null \
             || echo "issue #$issue")"

    if run_role "$wt" "Work issue #${issue} on branch ${branch}.

${body}"; then
      say "turn complete for #$issue"
      rm -f "$STATE/attempts/issue-$issue"
    else
      say "turn failed for #$issue (attempt $(attempts_of "issue-$issue") of $MAX_ATTEMPTS)"
      exit 1
    fi
    ;;

  prospector|ledger|warden)
    claim "$ROLE" || halt "$ROLE is already running"
    run_role "$REPO_ROOT" "Perform one turn of your role. Read CHARTER.md first."
    ;;

  migrator)
    claim "migrator-${ARG:-any}" || halt "migrator is already running on ${ARG:-any}"
    run_role "$REPO_ROOT" "Perform one migration turn${ARG:+ for corpus entry: $ARG}.
Read CHARTER.md first. All third-party code runs via scripts/gate/sandbox-run.sh."
    ;;

  *) die "unknown role: $ROLE" ;;
esac
