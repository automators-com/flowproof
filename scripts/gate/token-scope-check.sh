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
#
#    Reading proves nothing here: flowproof is PUBLIC, so `GET /pulls` returns
#    200 with no credential at all. The first version of this check asked
#    exactly that and passed a token which turned out to be sitting unapproved
#    with no write access whatsoever - the Builder wrote 296 verified lines and
#    then could not open the pull request. A check that a powerless token passes
#    is not a check.
#
#    Write capability is probed WITHOUT writing, by sending a deliberately
#    invalid body and reading which way it is rejected:
#
#      403  the token may not create pull requests
#      422  the token may; the body was invalid, and nothing was created
#
#    422 is the passing answer. Nothing is ever created by this probe.
# ---------------------------------------------------------------------------
code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -X POST -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" -d '{}' \
        "https://api.github.com/repos/${REPO}/pulls" 2>/dev/null || echo 000)"
case "$code" in
  422) pass "can create pull requests (probe rejected as invalid, not forbidden)" ;;
  403) fail "cannot create pull requests. If the token shows 'Pending' in the
      GitHub UI, an org owner has not approved it yet - a pending token still
      reads a public repository, which is why this used to look fine. Approve at
      github.com/organizations/<org>/settings/personal-access-token-requests, and
      check Pull requests: Read and write is granted." ;;
  401) fail "token rejected creating a pull request (HTTP 401)." ;;
  *)   fail "unexpected status ${code} probing pull-request creation." ;;
esac

# Same probe for issues: escalation and the ledger both need to comment.
code="$(curl -sS -o /dev/null -w '%{http_code}' \
        -X POST -H "Authorization: Bearer ${TOKEN}" \
        -H "Content-Type: application/json" -d '{}' \
        "https://api.github.com/repos/${REPO}/issues" 2>/dev/null || echo 000)"
case "$code" in
  422) pass "can write issues (probe rejected as invalid, not forbidden)" ;;
  403) fail "cannot write issues; escalation and the gap ledger both need it." ;;
  *)   warn "unexpected status ${code} probing issue creation" ;;
esac

# ---------------------------------------------------------------------------
# 5. The loop must not be able to BYPASS the review ruleset.
#
#    Bypass is evaluated on the ACTOR'S ROLE, not on the token's scopes, so a
#    fine-grained token owned by an admin sails past the review requirement no
#    matter how narrow its permissions are.
#
#    This check has been wrong twice, in opposite directions, and the history is
#    the point. It first read the role directly - correct. That returned 403 and
#    was rewritten to treat the 403 as PROOF of correct scoping, on the reasoning
#    that reading a role needs Administration access. It does not: it needs push.
#    The 403 was a symptom of the token still being Pending org approval, which
#    is an unrelated fault, and a correct check was replaced on the strength of a
#    misread symptom.
#
#    Reading the role is therefore back, asserting the value directly. Whether
#    the token holds Administration is a separate question, and check 3 already
#    answers it properly by probing branch protection.
# ---------------------------------------------------------------------------
actor="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
         https://api.github.com/user 2>/dev/null \
         | tr -d '\r\n ' | sed -n 's/.*"login":"\([^"]*\)".*/\1/p')"
[ -n "$actor" ] || fail "could not resolve the token's login."
pass "identity is '${actor}'"

# `permission` is present for any caller with push; `role_name` only appears for
# more privileged callers, so keying on it would fail for exactly the token we
# want to accept.
role="$(curl -sS -H "Authorization: Bearer ${TOKEN}" \
        "https://api.github.com/repos/${REPO}/collaborators/${actor}/permission" 2>/dev/null \
        | tr -d '\r\n ' | sed -n 's/.*"permission":"\([^"]*\)".*/\1/p')"

case "$role" in
  admin|maintain)
       fail "'${actor}' has repo role '${role}', which is in the ruleset's bypass
      list. It could merge without the Adversary's review. A loop needs its own
      identity with 'write' - not a narrower token on a privileged account." ;;
  write|push)
       pass "role is '${role}' - not in the ruleset's bypass list" ;;
  read|triage|none|"")
       fail "'${actor}' has role '${role:-<none>}'; it cannot push a branch." ;;
  *)   fail "unexpected repo role '${role}' for '${actor}'." ;;
esac

printf '\n\033[32mloop credential is correctly scoped and cannot bypass review\033[0m\n'
