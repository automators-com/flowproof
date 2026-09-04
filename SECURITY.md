# Security policy

flowproof is an assurance tool: adopters decide whether an agent may touch a
production system partly on the strength of what flowproof's containment,
secret handling, and audit output tell them. A vulnerability here does not
just affect flowproof — it can make a downstream verdict wrong. Report
security issues privately, not as a public issue.

## Reporting a vulnerability

**Preferred: GitHub private vulnerability reporting.** Open the
[Security tab](https://github.com/automators-com/flowproof/security) on this
repository and use **"Report a vulnerability."** This creates a private draft
security advisory visible only to the maintainers and you — not a public
issue, not a public PR. Enabled as of 2026-08-25.

**Fallback:** `hello@automators.com` if you cannot use GitHub (e.g. you found
the issue without a GitHub account). Put `SECURITY` in the subject line.

Please include, as far as you're able:

- the affected version(s) or commit;
- which trust boundary is crossed (see `docs/threat-model.md`) and why the
  existing mitigation doesn't hold;
- a minimal reproduction — a `.flow.yaml` and, if relevant, a redacted
  `.trace.jsonl` are usually enough; you do not need working exploit code.

Do not include real credentials, customer data, or a live customer's trace in
a report. If a finding requires customer data to reproduce, describe the
mechanism and we'll reproduce it against a synthetic fixture.

## Supported versions

flowproof is a Rust workspace where every crate, the Python wheel, and the
npm package move together on one version (`CLAUDE.md`, enforced by the
`versions agree` CI job). There is no maintained-branch matrix: **the latest
published release is the only supported version.** A fix ships as a new
release rather than a backport.

## What happens after you report

> **Draft — the specific windows below are a starting proposal, not yet a
> committed policy.** They need sign-off before this document should be read
> as a binding SLA by an external reporter.

- **Acknowledgement:** within 2 business days.
- **Initial triage** (confirmed / not a vulnerability / need more info):
  within 5 business days of acknowledgement.
- **Remediation target**, once confirmed: guided by severity — critical
  issues (e.g. a false-green verdict, a credential or secret reaching a
  trace, egress containment silently not enforcing) are treated as
  release-blocking; lower-severity issues are scheduled normally. No fixed
  calendar SLA is promised here yet.
- We will coordinate a disclosure timeline with you before anything is made
  public. Once a fix ships, we publish a
  [GitHub Security Advisory](https://github.com/automators-com/flowproof/security/advisories)
  describing the issue and crediting the reporter, unless you ask not to be
  credited.

## Independent review

flowproof does not yet have a completed independent (non-maintainer) security
review. This is tracked in
[issue #376](https://github.com/automators-com/flowproof/issues/376); the
review's scope, budget, and reviewer are open decisions for a human, not
something either the docs or the autonomous loops that develop this repo
decide unilaterally. `docs/threat-model.md` is the current, maintainer-written
threat model pending that review.

## Scope

In scope: the flowproof engine (all crates in this repository), the Python
SDK (`sdk/python`), and the npm package. The trust boundaries this covers are
listed in `docs/threat-model.md`.

Out of scope: vulnerabilities in a third-party MCP server, agent SDK, or
application under test that flowproof records/replays against — report those
to the relevant upstream project. If you're unsure which side of that line a
finding falls on, report it here anyway and we'll redirect it.
