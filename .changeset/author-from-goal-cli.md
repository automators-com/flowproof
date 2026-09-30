**New `flowproof author-from-goal` command.** Give a spec `goal:` instead of
`steps:` and this explores the live app toward that outcome, bounded by
`--budget` (default 40), producing a DRAFT `.flow.yaml` next to the spec
(`<name>.draft.flow.yaml`) — same draft-first review, then `flowproof
record`, shape as `author-from-doc`. `--recording-detail` controls capture
density; unlike `record`, a GIF of the whole attempt is always assembled
when recording isn't `off`, since watching what an autonomous exploration
actually did is the point. `--json` reports the drafted steps, the outcome
(reached / budget-exhausted / no-progress), and the recording bundle's
resolved location. EXPERIMENTAL, web driver only for now.

Also: `draft_assembly::assemble` gained an `extra_head` parameter so a
draft can carry forward spec-level context (`url:`, `session:`, `mock:`,
...) a live authoring pass already resolved — needed so a goal-authored
draft's `url:` survives into the file `flowproof record` reads next,
instead of silently vanishing.
