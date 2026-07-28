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
- **`required_pull_request_reviews: null`** (zero approvals) — required for
  autonomous merge. If you ever want the Adversary's sign-off to be
  *mechanically* required rather than advisory, it needs its own GitHub identity
  and this becomes `1`. Worth knowing that today the Adversary is advisory only.

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
| Loop credential + Adversary identity | **not done**, needs the GitHub web UI |

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

### The stacking is applied but its resolution is unverified

`GET /repos/automators-com/flowproof/rules/branches/main` reports **two separate
`pull_request` rules**, not one merged rule:

```
pull_request  from=Organization  dismiss_stale=false last_push_approval=false reviews=1
pull_request  from=Repository    dismiss_stale=true  last_push_approval=true  reviews=1
```

GitHub documents that the most restrictive value wins across overlapping
rulesets, so `true` should apply. But `dismiss_stale_reviews_on_push` is a
*behavior* performed on push rather than a condition a push must satisfy, and the
API exposes both rules side by side rather than a resolved value. **This is
documented behavior, not observed behavior.**

Settle it empirically as soon as the Adversary identity exists:

1. Adversary approves a test PR.
2. Push one more commit to that PR.
3. Check whether the approval was dismissed (`gh pr view N --json reviewDecision`).

If it was not dismissed, the org ruleset is winning and the two flags must move
to the org level after all — which means `admin:org` and an org-wide change.
Until this is checked, treat the stale-approval hole as **probably closed, not
provably closed**.

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

Honest accounting: check 1 (token ≠ interactive login) is verified, and it
short-circuits — so **checks 2–5 are syntax-valid but unexercised**. They cannot
be tested until a real loop credential exists. Run the script once against the
new token before trusting it, and expect to debug the `sed`-based JSON parsing;
`jq` is not installed on this box, which is why the parsing is done with `sed`.

## Ratchet baseline note

The concept quotes 651 Rust tests from a static count of `#[test]` attributes.
A Linux run reports **644 passed** — the difference is platform-gated tests
(`cfg(windows)`) that never execute here. The ratchet must count *attributes*,
not runtime passes, or it will read as regressed depending on which OS ran it.
