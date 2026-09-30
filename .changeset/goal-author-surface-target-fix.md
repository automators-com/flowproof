**Fixed: a surface-targeted assertion (`target: surface`) could never read as
true through `resolved_assertion_holds`.** `target_selector` returns `None`
for `Target::Surface` by design ("the surface is not an element — it
resolves via `surface_text`"), the same way live step execution already
knows to read the whole page instead of an element. `resolved_assertion_holds`
treated that `None` the same as every other unresolvable selector — always
false — which made any `goal:` (or `when:`/`repeat:` guard) written against
the whole page, e.g. `page shows X`, silently unreachable regardless of the
live page's real content. Now reads `surface_text()` in that case, matching
execution's own behavior.
