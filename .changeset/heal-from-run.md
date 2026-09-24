- **A degraded run can be healed without re-recording it.** `heal` always
  re-authored the whole flow against the live app, so fixing one step that
  had passed on a fallback selector meant driving the app again, sometimes
  calling a model, and reviewing a diff of every step that came out
  different. `heal --from-run <run>` reads that run's `result.json`, moves
  the selector that matched to the top of each degraded step's ladder and
  leaves every other line of the trace alone. It still only proposes until
  `--apply`.
