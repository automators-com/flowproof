**A flow spec may now name a `goal:` instead of writing `steps:` by hand.**
`goal:` is a new, optional, single-surface-only top-level field — mutually
exclusive with `steps:` — reserved for the forthcoming `author-from-goal`
exploration authoring mode. A spec that gives neither still fails to parse
with the same `spec has no steps` error as before; a spec giving both, or
pairing `goal:` with multi-surface `apps:`, is refused by name at parse time.
No existing spec changes behavior.
