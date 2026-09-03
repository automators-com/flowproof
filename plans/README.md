# flowproof — Plans

This directory is for feature-level implementation plans, in the style already
used in `mono` and `agent-framework`: a living prose document per proposal,
grounded in the actual code, with its open questions written down instead of
decided silently.

It is **not** a restatement of the product vision — that already lives in
[`CHARTER.md`](../CHARTER.md) (mission, invariants, out-of-scope list, priority
ordering) and [`docs/design.md`](../docs/design.md) (architecture). A plan here
takes those as given and works out one feature against them. `docs/` itself
already holds a few documents written in this exact register —
[`docs/multi-surface.md`](../docs/multi-surface.md) is effectively plan 0 of
this series, just filed before the series existed — new proposals belong here
instead so they're easy to find as a set.

## The series

| Plan | Doc | What it covers |
|------|-----|----------------|
| 1 | [001-credential-config.md](001-credential-config.md) | `flowproof config sap` / `flowproof config fiori`: a global, per-machine file for SAP GUI and Fiori credentials, replacing hand-exported env vars |
| 2 | [002-sap-fiori-doctor.md](002-sap-fiori-doctor.md) | Stub — `flowproof doctor` for SAP/Fiori connectivity, split out of plan 1's open questions, not yet scoped |
| 3 | [003-agent-config-skill.md](003-agent-config-skill.md) | `flowproof config skill`: installs an Agent Skill into an end user's own project so their coding agent (Claude Code, Codex CLI, and others) can walk them through `flowproof config sap`/`fiori` — issue #529 |
| 4 | [004-single-flow-shareability.md](004-single-flow-shareability.md) | Closing the loop on "one `.flow.yaml` + one `flowproof config` + one `flowproof run`, no `suite.yaml`": a stale-naming inventory fix and a missing-var error that now names the fix — issue #534 |
| 5 | [005-fiori-field-commit.md](005-fiori-field-commit.md) | Make Fiori/SAP WebGUI framed input typing pass only after the field value is committed and read back, preventing prefilled values from silently restoring |
| 8 | [008-ai-authoring-config.md](008-ai-authoring-config.md) | Add `flowproof config ai` for provider-neutral model authoring settings, storing one AI key and seeding `FLOWPROOF_AI_*` plus compatibility env vars — issue #541 |

## How to read and amend these

- **Plans are living documents.** When a decision changes, amend the plan in
  place — don't write a new doc that contradicts an old one. Git history is
  the changelog.
- **Open questions are explicit.** Each doc ends with an "Open questions"
  section. If something isn't decided, it lives there — nothing gets decided
  silently.
- **New plans get the next number.**
- **CHARTER.md wins conflicts.** It is constitution — see the "autonomous
  loops" section of `CLAUDE.md` — and no plan here may propose changing it,
  `scripts/gate/`, or the other constitution-protected paths. A plan that
  needs one of those to change says so explicitly and leaves it to a human.
