**`record --repair-attempts` sets how many fixes the repair loop may try.**
The loop was fixed at three model calls, and an engine-gap verdict's fresh
retry started a second loop with three more, so one recording could ask for
anywhere from one fix to six, and the progress line counted "attempt 1 of 3"
twice. `--repair-attempts N` (1–10, default 3) is now the most model calls one
recording makes, across both loops: the fresh retry spends what the first loop
left and keeps counting from there. The loop still stops sooner when the same
failure repeats or the model says a fix cannot help.
