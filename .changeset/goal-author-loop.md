**New `flowproof_agent::goal_author` module: bounded, outcome-only flow exploration.**
Given a `goal:` spec (see the `goal:` spec field changeset), `author_from_goal`
explores the live app toward that outcome instead of following a written
`steps:` list — trying one action at a time, checking the goal after each,
never undoing a prior action, and giving up after a small hardcoded deny-list
match or a repeated stuck signature rather than burning its whole action
budget. Every attempt, including abandoned ones, is captured through the
existing recording bundle for review. Produces a draft `.flow.yaml`, never a
committed trace — the same draft-first shape `author-from-doc` already uses.
Not yet reachable from the CLI; that lands in a follow-up.
