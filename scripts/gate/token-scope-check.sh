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
#    This is the check the earlier design missed. The `default` ruleset requires
#    1 approving review, and its bypass list contains OrganizationAdmin and
#    RepositoryRole 5 (admin). Bypass is evaluated on the ACTOR'S ROLE, not on
#    the token's scopes -- so a fine-grained PAT owned by an admin sails past
#    the review requirement no matter how narrow its permissions are. The gate
#    would look enforced and be theoretical.
#
#    The loop therefore needs its own identity with `write`, not a weaker token
#    on an admin account.
# ---------------------------------------------------------------------------
actor="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
         https://api.github.com/user 2>/dev/null \
         | tr -d '\r\n ' | sed -n 's/.*"login":"\([^"]*\)".*/\1/p')"
[ -n "$actor" ] || fail "could not resolve the token's login."

role="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
        "https://api.github.com/repos/${REPO}/collaborators/${actor}/permission" 2>/dev/null \
        | tr -d '\r\n ' | sed -n 's/.*"role_name":"\([^"]*\)".*/\1/p')"

case "$role" in
  admin|maintain) fail "loop identity '${actor}' has repo role '${role}', which is
      in the ruleset's bypass list. It can merge without the Adversary's review.
      Use a separate machine account or GitHub App with 'write'." ;;
  write|push)     pass "actor '${actor}' has role '${role}' (cannot bypass review)" ;;
  "")             warn "could not read the actor's repo role; verify manually that
      '${actor}' is not an admin and not an org owner" ;;
  *)              fail "unexpected repo role '${role}' for '${actor}'." ;;
esac

# Direct confirmation when the token can read the ruleset: GitHub reports
# whether the *calling* identity may bypass. "always" is disqualifying.
bypass="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
          "https://api.github.com/repos/${REPO}/rulesets?includes_parents=true" 2>/dev/null \
          | grep -o '"current_user_can_bypass":"[a-z_]*"' | head -1 | sed 's/.*:"\(.*\)"/\1/')"
case "$bypass" in
  always|pull_requests_only) fail "GitHub reports this identity can bypass rulesets
      ('${bypass}'). The review gate would not apply to it." ;;
  never) pass "GitHub confirms this identity cannot bypass rulesets" ;;
  *)     warn "ruleset bypass status not readable with this token (role check above stands)" ;;
esac

printf '\n\033[32mloop credential is correctly scoped and cannot bypass review\033[0m\n'
