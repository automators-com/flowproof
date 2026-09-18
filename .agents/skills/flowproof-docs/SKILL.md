---
name: flowproof-docs
description: Write, restructure, or audit Flowproof documentation for task success, technical accuracy, scanability, and maintainability. Use for README and docs changes in this repository; do not use for internal plans or code comments unless requested.
---

# Flowproof documentation

Help one primary reader complete one task or understand one system. Preserve
Flowproof's technical precision while making the shortest correct path easy to
find.

## Before editing

1. Name the primary reader and their goal.
2. Choose one dominant content type for each page:
   - tutorial: learn by completing a guided sequence;
   - how-to: complete a specific task;
   - reference: look up exact contracts, values, and constraints;
   - explanation: understand concepts, architecture, or tradeoffs.
3. Read the relevant code, schemas, tests, examples, and current docs. Do not
   infer product behavior from prose alone.
4. Run `python3 .agents/skills/flowproof-docs/scripts/audit_docs.py` to find
   broken local links and density hotspots. Treat density findings as prompts
   for judgment, not automatic failures.

## Structure

- Start tutorials and how-to guides with the outcome, prerequisites, shortest
  working path, and a recognizable success result.
- Put the common path before advanced options. Link to reference pages instead
  of embedding complete contracts in a tutorial.
- Keep reference prose minimal. Prefer tables for fields, flags, defaults,
  accepted values, side effects, and platform differences.
- Split a page when audiences or content types conflict. Do not fragment one
  coherent task across shallow pages.
- Use descriptive headings that remain meaningful when retrieved alone.
- Define product terms, trust boundaries, prerequisites, and expected outputs
  before relying on them.
- Use the same name for the same concept throughout the docs.

## Flowproof-specific rules

- Keep `README.md` concise and orienting. Make `docs/` the canonical home for
  detailed behavior, and link to it rather than duplicating it.
- Preserve the distinction between containment, observed behavior, regression
  evidence, and deterministic replay. Never strengthen a security claim while
  shortening it.
- Preserve exact commands, structured field names, exit codes, platform
  limitations, and destructive side-effect warnings.
- Keep examples copyable and realistic. Never add secrets or unverifiable
  production values.
- Update the nearest `meta.json` when adding, removing, or renaming a page.
- Prefer relative links that resolve in both the repository and the docs site.

## Visuals

Add a small diagram or decision table when it makes a relationship materially
easier to understand, especially for:

- state or lifecycle transitions;
- trust and execution boundaries;
- one source affecting three or more downstream artifacts;
- choosing among adapters, authoring modes, or assertion mechanisms.

Use Mermaid only when the docs renderer supports it. Provide meaningful labels
and surrounding text so the page remains understandable without the visual.
Do not use screenshots for facts that will become stale quickly.

## Style

- Lead with the outcome. Use concise, active, reader-centered prose.
- Use numbered lists for procedures and bullets for unordered facts.
- Use fenced blocks for commands, specs, and expected output.
- State the consequence and trigger in every warning.
- Remove repetition, vague cross-references, marketing filler, and unsupported
  claims.
- Keep sections self-contained enough for search and AI retrieval. Prefer
  explicit nouns over `it`, `this`, and `the above` when context could be lost.

## Review

Before finishing:

1. Run the docs audit again and resolve all broken local links introduced or
   touched by the change.
2. Verify commands and claims with the narrowest relevant tests or source
   inspection. Do not claim commands were run when they were not.
3. Check that the target reader can identify applicability, complete the task,
   recognize success, and recover from expected failures.
4. Report the primary audience and content type, structural changes, sources
   used for verification, and any remaining risk.

Score the result from 0 to 2 on audience fit, task success, content-type
discipline, accuracy, structure, examples, terminology, independent retrieval,
maintenance, and style. A publishable page has no zero and totals at least
16/20.
