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

# What a role may execute.
#
# `--permission-mode acceptEdits` auto-accepts file edits but still PROMPTS for
# a mutating Bash command - and in a non-interactive turn a prompt is a denial.
# The first complete Builder turn wrote 333 lines and 8 tests, could not run
# `cargo fmt`, `cargo test`, `git add` or `gh`, and correctly refused to open a
# pull request it could not certify.
#
# This is a DENY-list, not an allow-list, and that was forced by the runtime
# rather than chosen. Measured against claude 2.1.220:
#
#   --allowed-tools "Bash"           grants Bash            (works)
#   --allowed-tools "Bash(cargo *)"  grants nothing         (does NOT work)
#   --disallowed-tools "Bash(x *)"   blocks x               (works)
#
# So per-command granting is unavailable and the honest description of what
# follows is "everything except these", which is weaker than "only these". What
# it removes is the path from a mistake to a breach - `flow` has passwordless
# sudo, so an unrestricted Builder would have root on the box holding the SSH
# keys. It does not make the Builder safe: `python3`, `rm` and `sed` remain, and
# any of them can do damage. Real isolation means a container, as the Migrator
# already has; this is the cheaper 90%.
#
# Verified blocked, not assumed: see the entry in scripts/gate/README.md.
DENIED_TOOLS='Bash(sudo *) Bash(sudo:*) Bash(curl *) Bash(curl:*) Bash(wget *) Bash(wget:*) Bash(ssh *) Bash(ssh:*) Bash(scp *) Bash(scp:*) Bash(npm *) Bash(npm:*) Bash(npx *) Bash(npx:*)'

# Every role that acts gets the same shell, and the deny-list is what narrows it.
#
# The earlier split gave the read-only roles `Bash(git *) Bash(gh *)`, which
# grants NOTHING - per-command allow-patterns do not work through these flags,
# as measured above. The Warden therefore had no shell, could not run `gh`, and
# could compute none of its halt conditions. On its first real turn it noticed,
# and halted the fleet rather than report all-clear.
#
# The distinction those roles need is not expressible as a permission here, so it
# lives where it can be stated: their prompts. The Warden must not fix what it
# finds, the Prospector never executes what it discovers, and the Ledger keeper
# implements nothing. That is weaker than a capability boundary and is worth
# knowing - the deny-list still removes the paths from a mistake to a breach.
ROLE_TOOLS="Bash Read Write Edit Glob Grep"

# Bounds. A loop that never stops is a loop that cannot be reasoned about - but
# a bound set too low is not a safety property, it is a way of failing after
# paying full price. 60 was arbitrary and wrong: the first real turn spent them
# on build-and-test cycles, produced 309 lines of sound work, and was killed
# before it could commit any of it. A Rust fix needs room to compile.
MAX_TURNS="${LOOP_MAX_TURNS:-300}"
MAX_ATTEMPTS="${LOOP_MAX_ATTEMPTS:-3}"

mkdir -p "$STATE"/{locks,logs,attempts}

die()  { printf '\033[31mstop\033[0m  %s\n' "$1" >&2; exit 1; }
say()  { printf '\033[36mloop\033[0m  %s\n' "$1"; }
halt() { printf '\033[33mhalt\033[0m  %s\n' "$1"; exit 0; }

[ -n "$ROLE" ] || die "usage: run.sh <role> [issue-number]"
# The Integrator is a script, not a prompt: finding an approved pull request
# whose checks are green needs no judgement, and a deterministic merge cannot
# talk itself into one. It is the only role with no file in roles/.
if [ "$ROLE" != "integrator" ]; then
  [ -f "$ROLES/$ROLE.md" ] || die "no such role: $ROLE (see scripts/loop/roles/)"
fi

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

# 3. The token must belong to the Builder, not merely be well-scoped. A
#    correctly-scoped token for the WRONG account would pass every check in
#    token-scope-check.sh and then open pull requests as a stranger.
actor="$(GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" gh api user --jq .login 2>/dev/null || true)"
[ "$actor" = "$BUILDER_LOGIN" ] \
  || die "the loop token belongs to '${actor:-<unresolved>}', not '$BUILDER_LOGIN'"

# 4. The model runtime, for the roles that use one. The Integrator does not.
if [ "$ROLE" != "integrator" ]; then
  command -v claude >/dev/null || die "the claude CLI is not installed"
  claude -p "ok" --output-format text >/dev/null 2>&1 \
    || die "the claude CLI is not authenticated (ANTHROPIC_API_KEY, or 'claude /login')"
fi

# 5. Role tooling, checked here rather than discovered halfway through a turn
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
  # Three separate `local`s, not one. `local` is a command, so bash expands ALL
  # its arguments before any assignment takes effect - `local key="$1"
  # lock=".../$key"` leaves $key unset in the second expansion, which under
  # `set -u` aborts the run. shellcheck SC2318; it broke the first real turn.
  local key="$1"
  local lock="$STATE/locks/$key"
  local age
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
  # shellcheck disable=SC2086  # both lists must word-split into separate args
  ( cd "$workdir" \
    && GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" \
       claude -p "$context" \
         --append-system-prompt "$(cat "$ROLES/$ROLE.md")" \
         --max-turns "$MAX_TURNS" \
         --permission-mode acceptEdits \
         --allowed-tools $ROLE_TOOLS \
         --disallowed-tools $DENIED_TOOLS \
         --output-format text < /dev/null ) > "$log" 2>&1
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

    # A previous attempt may have died mid-edit. The next attempt is a fresh
    # session with no memory of that work, so it would open on a tree carrying
    # changes it did not make and cannot explain. Preserve the diff where a
    # human can read it, then start clean - a retry that is not deterministic is
    # not a retry.
    if [ -n "$(git -C "$wt" status --porcelain)" ]; then
      keep="$STATE/logs/$(date -u +%Y%m%dT%H%M%SZ)-issue-$issue-abandoned.diff"
      git -C "$wt" diff > "$keep"
      say "previous attempt left $(git -C "$wt" diff --shortstat | tr -d '\n'); saved to ${keep#"$REPO_ROOT"/} and reset"
      git -C "$wt" reset -q --hard origin/main && git -C "$wt" clean -qfd
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
      changed="$(git -C "$wt" diff --shortstat | tr -d '\n')"
      [ -n "$changed" ] && say "it left:${changed} - see the log above for why it stopped"
      exit 1
    fi
    ;;

  integrator)
    # Exactly one at a time. Two integrators merging concurrently is precisely
    # the semantic-conflict case `strict` exists to catch.
    claim integrator || halt "an integrator is already running"
    GH_TOKEN="$FLOWPROOF_LOOP_TOKEN" LOOP_STATE="$STATE" \
      "$REPO_ROOT/scripts/loop/integrate.sh" --max "${LOOP_MERGE_MAX:-1}"
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
