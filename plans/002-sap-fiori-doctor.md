---
status: proposed
---
# Plan 2 — `flowproof doctor` for SAP/Fiori

Split out of [001-credential-config.md](001-credential-config.md)'s Open
Questions on 2026-08-31: whether `flowproof doctor` (today, agent-boundary
connectivity only — checks that an agent's model traffic actually reaches
flowproof before you write a spec or spend a key on a recording) should grow
an equivalent check for SAP GUI / Fiori — read the config plan 1 writes, and
report what it can actually reach, before someone spends time writing a flow
against it.

Good idea, explicitly deferred: plan 1 is the write path only (interactive
prompt → config file, no validation at config time). This is that
validation, done as its own read-time command instead of folded into
`config`, once plan 1 has shipped and there's a real config file to read.

Not scoped yet — this is a placeholder, not a plan.
