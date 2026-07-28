#!/usr/bin/env bash
# Verify the loop credential cannot edit the gate it is judged by.
#
# Blocker #4 from the autonomous-development concept: a token with `workflow`
# scope can rewrite .github/workflows/**, and a classic PAT with `repo` on a
# repo you admin reaches the branch-protection API. A system whose only safety
# property is "CI must be green" must not hand its agents a credential that can
# edit CI.
#
# Run this at the start of every loop session. Exit non-zero => do not proceed.
#
# Usage:  FLOWPROOF_LOOP_TOKEN=github_pat_... ./token-scope-check.sh
set -euo pipefail

REPO="${FLOWPROOF_REPO:-automators-com/flowproof}"
TOKEN="${FLOWPROOF_LOOP_TOKEN:-}"

fail() { printf '\033[31mFAIL\033[0m  %s\n' "$1" >&2; exit 1; }
pass() { printf '\033[32mok\033[0m    %s\n' "$1"; }
warn() { printf '\033[33mwarn\033[0m  %s\n' "$1"; }

[ -n "$TOKEN" ] || fail "FLOWPROOF_LOOP_TOKEN is unset. Loops must never fall back
      to the interactive gh login, which carries repo+workflow scope."

# ---------------------------------------------------------------------------
# 1. The token must not be the interactive gh login.
# ---------------------------------------------------------------------------
if command -v gh >/dev/null 2>&1; then
  if interactive="$(gh auth token 2>/dev/null)"; then
    [ "$TOKEN" != "$interactive" ] || fail "the loop token IS the interactive gh
      login. Mint a separate least-privilege credential (see README in this dir)."
  fi
fi
pass "distinct from the interactive gh login"

# ---------------------------------------------------------------------------
# 2. Classic tokens: read the granted scopes off a live API response.
#    Fine-grained tokens (github_pat_*) report no X-OAuth-Scopes header at all,
#    which is itself the desired answer.
# ---------------------------------------------------------------------------
hdrs="$(curl -sS -D - -o /dev/null -H "Authorization: Bearer ${TOKEN}" \
        https://api.github.com/user 2>/dev/null || true)"

status="$(printf '%s' "$hdrs" | awk 'NR==1{print $2}')"
[ "$status" = "200" ] || fail "token rejected by the API (HTTP ${status:-none})."

scopes="$(printf '%s' "$hdrs" \
          | tr -d '\r' \
          | awk -F': ' 'tolower($1)=="x-oauth-scopes"{print $2}')"

if [ -z "$scopes" ]; then
  pass "fine-grained token (no OAuth scopes granted)"
else
  warn "classic token with scopes: ${scopes}"
  for bad in workflow admin:org site_admin delete_repo; do
    case ",${scopes// /}," in
      *",${bad},"*) fail "token carries '${bad}' scope. It can edit the gate.
      Fine-grained token with contents:write + pull_requests:write only." ;;
    esac
  done
  pass "no workflow/admin scope"
fi

# ---------------------------------------------------------------------------
# 3. Capability probe: the token must NOT be able to read, and therefore not
#    write, branch protection. This is the check that actually matters --
#    scope strings can mislead, a 403 cannot.
# ---------------------------------------------------------------------------
code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -H "Authorization: Bearer ${TOKEN}" \
        "https://api.github.com/repos/${REPO}/branches/main/protection" 2>/dev/null || echo 000)"

case "$code" in
  200) fail "token can READ branch protection => it is admin-capable and can
      disable the gate. Reduce it to contents:write + pull_requests:write." ;;
  403|404) pass "cannot reach branch protection (HTTP ${code})" ;;
  *)   fail "unexpected status ${code} probing branch protection." ;;
esac

# ---------------------------------------------------------------------------
# 4. It must still be able to do its actual job.
# ---------------------------------------------------------------------------
code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -H "Authorization: Bearer ${TOKEN}" \
        "https://api.github.com/repos/${REPO}/pulls?per_page=1" 2>/dev/null || echo 000)"
[ "$code" = "200" ] || fail "token cannot list pull requests (HTTP ${code}); it
      is too restricted to open PRs."
pass "can read pull requests"

# ---------------------------------------------------------------------------
# 5. The loop must not be able to BYPASS the review ruleset.
#
#    Bypass is evaluated on the ACTOR'S ROLE, not on the token's scopes -- so a
#    fine-grained PAT owned by an admin sails past the review requirement no
#    matter how narrow its permissions are. The gate would look enforced and be
#    theoretical.
#
#    The obvious check -- ask GitHub for this identity's repo role, or read
#    `current_user_can_bypass` -- CANNOT WORK. Both need Administration access,
#    which is precisely what a correctly-scoped loop token must not have. A
#    token restricted enough to be safe is too restricted to describe itself.
#
#    So the test is inverted: the 403 IS the evidence. If this token can read
#    its own repo role, it holds Administration access and is over-privileged by
#    definition. Refusal to answer is the passing answer.
#
#    What the role actually IS therefore has to be established out of band, by a
#    human with an admin token, and recorded in scripts/gate/README.md. This
#    check proves the token is too weak to bypass; it does not prove which
#    account it belongs to.
# ---------------------------------------------------------------------------
actor="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
         https://api.github.com/user 2>/dev/null \
         | tr -d '\r\n ' | sed -n 's/.*"login":"\([^"]*\)".*/\1/p')"
[ -n "$actor" ] || fail "could not resolve the token's login."
pass "identity is '${actor}'"

code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -H "Authorization: Bearer ${TOKEN}" \
        "https://api.github.com/repos/${REPO}/collaborators/${actor}/permission" 2>/dev/null || echo 000)"
case "$code" in
  200) fail "'${actor}' can read its own repository role, so this token holds
      Administration access. An Administration-capable identity is in the
      ruleset's bypass list and could merge without the Adversary's review.
      Re-mint with Contents + Pull requests + Issues only." ;;
  403|404) pass "cannot read its own repo role (HTTP ${code}) -- no Administration access" ;;
  *)   fail "unexpected status ${code} probing the repo role for '${actor}'." ;;
esac

printf '\n\033[32mloop credential is correctly scoped and cannot bypass review\033[0m\n'
