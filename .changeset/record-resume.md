**`record --resume` picks up where a failed recording stopped.** A recording
that failed at step 6 kept nothing, so recording it again asked the model to
author steps 1 to 5 again. A failed recording now leaves the steps it got
through in `<flow>.checkpoint.json`, and `record --resume` replays them (or,
with no checkpoint, the existing trace's steps) and authors only from where
they stop matching. Repair stays on, each replayed step is checked against the
live app first, and a finished recording removes the checkpoint. The JSON
result names what it resumed from (`resumed_from`). Flows that assert
`assert_no_secret_leak` keep no checkpoint.
