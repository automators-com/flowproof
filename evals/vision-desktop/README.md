# Vision on desktop apps, end to end

Flows that drive real Windows desktop apps through `app: vision`, from
recording (where plain-English steps are authored by the configured model)
to replay. `run.ps1` opens each app, records every flow, replays what
recorded, and prints a pass/fail table. Pair it with
`scripts/vision-bench/run.ps1`, which measures grounding alone with no model.

Recorded traces are written beside the specs and are not committed: they
depend on this machine's screen.
