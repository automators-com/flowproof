**A repair retry replays the steps that already passed.** After the repair
loop patched a failing step, it re-recorded the whole flow from the first
step, asking the model to author every step before the failure again on
every attempt. A retry now replays those steps as the failed attempt recorded
them, checks each against the live app as `--reuse` does, and asks the model
only from the patched step on. The progress line says how many it replays.
The fresh retry after an engine-gap verdict is unchanged: it is meant to be
independent, so it still records from the start.
