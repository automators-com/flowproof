# loop-gate

The two mechanical guards the autonomous loops need before they can run
unattended, plus the branch-protection payload. Intended home: `scripts/gate/`
in the flowproof repo — a path the CI scope filter already knows about — and
inside the constitution's human-write-only set, since these are what constrain
the loops.

## `token-scope-check.sh`

Run at the start of every loop session; non-zero exit means do not proceed.

Blocker #4: the interactive `gh` login on this box carries `repo` + `workflow`.
`workflow` lets a loop rewrite `.github/workflows/**`, and `repo` on a repo you
admin reaches the branch-protection API. A system whose only safety property is
"CI must be green" must not give its agents a credential that can edit CI.

Four checks, in ascending order of how much they prove:

1. The loop token is not the interactive `gh` login.
2. No `workflow` / `admin:org` / `site_admin` / `delete_repo` scope.
3. **A live capability probe: the token must get 403/404 on the branch-protection
   endpoint.** This is the check that matters — scope strings can mislead, a 403
   cannot.
4. It can still list pull requests, i.e. it is not so restricted it cannot work.

### The token you need to mint

This is the one step that cannot be automated: it needs the GitHub web UI, and
minting a credential should be yours. Create a **fine-grained personal access
token** at `Settings → Developer settings → Personal access tokens → Fine-grained`:

- Resource owner: `automators-com`, repository: `flowproof` only
- Repository permissions — **Contents: Read and write**, **Pull requests: Read
  and write**, **Issues: Read and write** (the ledger and escalation need it),
  **Metadata: Read** (mandatory)
- Everything else: **No access**. Specifically *not* Actions, *not*
  Administration, *not* Workflows.
- Expiry: 90 days maximum, calendared for rotation

Then, readable only by you and never passed into a container:

```bash
install -m 600 /dev/null ~/.config/flowproof-loop.env
printf 'FLOWPROOF_LOOP_TOKEN=github_pat_...\n' > ~/.config/flowproof-loop.env
```

Loops source that file. `sandbox-run.sh` passes no `-e` flags at all, so it
cannot reach the corpus containers.

## `sandbox-run.sh`

Blocker #2: a corpus repo's `npm install` runs arbitrary postinstall scripts,
and this box holds `~/.ssh/flowproof_deploy` and a `gh` token. That risk is not
recoverable by revert, which makes it the most serious one in the design.

Two-phase network, matching the concept: `--phase install` allows egress for
dependency fetch and model recording; `--phase replay` uses `--network=none`,
which is also a free mechanical proof of flowproof's zero-LLM-call claim.

Verified on this box, all six passing:

| Test | Property |
|---|---|
| T1 | replay phase has no egress |
| T2 | `~/.ssh` invisible inside the container |
| T3 | host env does not leak (`FLOWPROOF_LOOP_TOKEN`, `ANTHROPIC_API_KEY`) |
| T4 | install phase *does* have egress |
| T5 | memory cap enforced (rootless cgroup v2 delegation works) |
| T6 | refuses to mount anything outside the scratch allowlist |

T6 exists because a typo in `--work` would mount the very credentials the script
is there to isolate.

## `protection.json`

Branch protection payload for `main`. **Not applied** — the API call was
blocked by the permission classifier, correctly, since it writes your repo
settings. Apply with:

```bash
gh api -X PUT repos/automators-com/flowproof/branches/main/protection --input protection.json
```

Two deliberate choices worth understanding before you run it:

- **`enforce_admins: false`** — you keep a direct-push escape hatch. The loops
  are constrained by their *token scope*, not by admin enforcement, so this
  costs no safety and preserves your ability to hotfix and to clear the circuit
  breaker.
- **`required_pull_request_reviews: null`** (zero approvals) — this field never
  mattered. The review requirement comes from the **org ruleset**, not from
  branch protection, and GitHub enforces the union of both. The Adversary's
  approval is mechanically required today; see the ruleset sections below.

**`strict: true`** is load-bearing: it forces a branch to be up to date before
merging, which mechanically enforces the rebase-and-recheck that catches the
semantic conflicts described in the concept (two independently-green PRs that
break `main` together).

The five required checks are the ones that always report. `windows build + E2E`
and `web E2E (ubuntu)` are deliberately excluded — they are off the PR path by
design (~50 min), and the Integrator covers them post-merge on `main`.

## Applied state, as of this session

| Gate | Status |
|---|---|
| `protection.json` — branch protection on `main` | **applied**, verified server-side |
| `ruleset-repo-guard.json` — repo ruleset id `19879637` | **applied**, both flags `true` |
| `ruleset-hardened.json` | **superseded, do not use** — wrong endpoint (see below) |
| Loop credential (`AutomatorsAgent`) | **minted and verified**, all 5 checks green |
| Adversary identity | **not needed** — it is a workflow, see `CHARTER.md` §9 |

### Why `ruleset-hardened.json` is dead

The `default` ruleset (id 7514604) has `source_type: "Organization"` — it belongs
to `automators-com`, and flowproof merely inherits it. The repo owns no rulesets
of its own. So `PUT /repos/.../rulesets/7514604` returns **404**: wrong endpoint.

The correct endpoint is `/orgs/automators-com/rulesets/7514604`, which needs
`admin:org` scope and would change the gate for **every repo in the org**. Not
worth it to harden one repo.

Instead, `ruleset-repo-guard.json` **creates a repo-level ruleset** that stacks
on top of the org one, adding only the `pull_request` rule with the two flags
set. No `admin:org`, no blast radius.

### The stacking resolves as documented — measured

`GET /repos/automators-com/flowproof/rules/branches/main` reports **two separate
`pull_request` rules** rather than one merged value:

```
pull_request  from=Organization  dismiss_stale=false last_push_approval=false reviews=1
pull_request  from=Repository    dismiss_stale=true  last_push_approval=true  reviews=1
```

GitHub documents that the most restrictive value wins, but
`dismiss_stale_reviews_on_push` is a *behaviour* performed on push rather than a
condition a push must satisfy — so it was worth measuring rather than assuming.

Measured in #192: an approval was posted on `opened`, a second commit pushed, and
the approval went to **`DISMISSED`** with `reviewDecision` falling back to
`REVIEW_REQUIRED`. The repo ruleset's `true` beats the org ruleset's `false`. The
stale-approval hole is **provably** closed, not probably closed.

(The probe approved on `opened` only. Had it also fired on `synchronize` it would
have re-approved the instant the second commit landed and masked the result.)

### External dependency worth knowing

The org ruleset means `automators-com` policy is part of flowproof's gate, and
anyone with org admin can change it without touching this repo. For an autonomous
system whose safety rests on the gate, that is an external dependency the
constitution cannot protect.

## `ruleset-hardened.json` — superseded, kept for reference only

`protection.json` is applied and live. But branch protection was never the only
gate: an **active repo ruleset** (`default`, id 7514604) independently enforces
`required_approving_review_count: 1`, `copilot_code_review`, `non_fast_forward`,
and `deletion` on `main` / `dev`. Rulesets and branch protection are separate
mechanisms and GitHub enforces the **union**, so
`required_pull_request_reviews: null` in `protection.json` never removed the
review requirement — it was not coming from there.

This is good news: the mechanism for enforced adversarial review already exists.
Give the Adversary its own GitHub identity and `required_approving_review_count:
1` becomes real enforcement rather than advice. GitHub blocks self-approval, so
Builder ≠ Adversary comes for free.

Two flags were open holes under autonomy, and this payload closes them:

| Flag | Was | Now | Why |
|---|---|---|---|
| `dismiss_stale_reviews_on_push` | `false` | `true` | otherwise an approval survives later commits |
| `require_last_push_approval` | `false` | `true` | otherwise the Builder gets sign-off on one diff and merges another |

Everything else is preserved byte-for-byte from live state — all four rule
types, both bypass actors, the ref conditions. Verified by diffing against
`ruleset-current.json` (captured before any change).

```bash
gh api -X PUT repos/automators-com/flowproof/rulesets/7514604 --input ruleset-hardened.json
```

`PUT` replaces the ruleset, which is why the payload is complete rather than a
partial patch. `ruleset-current.json` is the rollback.

### The bypass trap — this supersedes the token advice above

The fine-grained-PAT recommendation is **necessary but not sufficient**. Ruleset
bypass is evaluated on the **actor's role**, not the token's scopes. The bypass
list contains `OrganizationAdmin` and `RepositoryRole: 5` (admin) at
`bypass_mode: always`. A fine-grained PAT owned by an admin account acts as that
admin, so it would sail past the review requirement no matter how narrow its
permissions are — the gate would look enforced and be theoretical.

The loops need a **separate identity with `write`**: a machine user added as a
collaborator, or a GitHub App. Not a weaker token on your account.
`token-scope-check.sh` check 5 enforces this, and it is the reason that check
exists.

Your own admin bypass is deliberately left in place — it is the human escape
hatch, the same role `enforce_admins: false` plays in `protection.json`.

### Test status of the checks

All five checks now run green against the real loop credential
(`AutomatorsAgent`, minted 2026-07-28):

```
ok  distinct from the interactive gh login
ok  fine-grained token (no OAuth scopes granted)
ok  cannot reach branch protection (HTTP 403)
ok  can read pull requests
ok  identity is 'AutomatorsAgent'
ok  cannot read its own repo role (HTTP 403) -- no Administration access
```

### Why check 5 is inverted

The first version asked GitHub for the identity's repo role and for
`current_user_can_bypass`. Neither can work: both require Administration access,
which is exactly what a correctly-scoped loop token must not have. **A token
restricted enough to be safe is too restricted to describe itself.**

So the test is inverted — the 403 *is* the evidence. A token that can read its
own repo role holds Administration access and is over-privileged by definition.
Refusal to answer is the passing answer.

The consequence is worth stating plainly: this check proves the token is **too
weak to bypass review**. It does not prove **which account it belongs to**. That
has to be established out of band by a human with an admin token:

```bash
gh api repos/automators-com/flowproof/collaborators/AutomatorsAgent/permission --jq .role_name
# => write        verified 2026-07-28
```

`write` is not in the ruleset's bypass list (`OrganizationAdmin`,
`RepositoryRole: 5`), so the identity cannot merge without the Adversary.
Re-verify this after any change to the account's repository role.

### A note on `jq`

`jq` is not installed on this box, so the parsing uses `sed` and `grep`. That is
also how the original check 5 failed: `grep -o` exits 1 when it matches nothing,
`pipefail` propagated it, and `set -e` killed the script *after* every individual
check had already printed `ok`. It looked like a failing credential; it was a
passing credential and a failing script. The fifth instance of exit-code masking
in this repository, and the reason the Builder prompt forbids piping a
verification command.

## Ratchet baseline note

The concept quotes 651 Rust tests from a static count of `#[test]` attributes.
A Linux run reports **644 passed** — the difference is platform-gated tests
(`cfg(windows)`) that never execute here. The ratchet must count *attributes*,
not runtime passes, or it will read as regressed depending on which OS ran it.
