# Video-to-flow evals

Generated screen recordings with a known answer, for measuring
`flowproof author-from-video` (`plans/016-video-to-flow-evals.md`). Nothing
here is a real recording of a real system: every app is an invented HTML
page in `fixture/`, every name and number in it is made up, and the
fixtures reach no network.

## How a case becomes a video

`generator/` (`flowproof-video-fixture`, a workspace member) serves
`fixture/` on `127.0.0.1:8765`, drives a case's page in headless Chrome,
and films it on its own clock in 100ms ticks: a screenshot whenever the
page may have changed, the pointer drawn on top with the run filmstrip's
own cursor (`flowproof_driver::recording::draw_cursor`), click rings on the
frames a click lands, and the pictures muxed into `video.mp4` by the
system ffmpeg. Because the clock is the film's own, a regenerated video is
the same video and every action's millisecond is exact.

```bash
cargo build -p flowproof-cli -p flowproof-video-fixture
target/debug/flowproof-video-fixture evals/video-to-flow/cases --out evals/video-to-flow/generated
python3 scripts/video-frames-eval.py evals/video-to-flow/generated
```

Per case, the generator writes `video.mp4`, `actions.json` (every action
with when it started and ended), `case.json` (what the scorers need) and
`expected.flow.yaml`, the draft a perfect reading would give. The expected
flow comes out of the script that caused the actions, so it cannot drift
from the picture. **Regenerate it, never edit it**: a hand-edited answer key
is one somebody ends up editing until the prompt passes. `generated/` is
not committed.

The pointer matters. A screenshot taken through Chrome's debugging protocol
has none, and a corpus without one would make every action look like it
happened by itself, which is what the prompt's `UNEXPLAINED:` rule exists to
flag. The pointer also travels between targets over a few frames rather
than jumping, so the small-change noise a moving pointer makes is spent
against the frame budget as it is in a real recording.

## The cases

| Case | Fixture | What it exists to catch |
|---|---|---|
| `01-smoke` | `app.html` | A draft that fails outright. The one case cheap enough to run on every pull request that touches the prompt. |
| `02-order-form` | `app.html` | Steps lost from an ordinary twelve-step form, a typeahead whose picked entry differs from what was typed, a street and house number folded into one line. |
| `03-counter-and-tabs` | `app.html` | "A count going from 2 to 3 to 4 is two clicks", with clicks close together. |
| `04-twenty-minutes` | `app.html` | The frame budget on a twenty-minute recording, where sampling thins out past six minutes. |
| `05-retina-sap` | `sap-form.html` at 2560px | The scaling floor: 10px grey labels that reach the model at 1280px. |
| `06-taskbar-menubar-sap` | `sap-form.html` in `chrome.html` | Edge trimming: a still menu bar to cut, a taskbar whose clock ticks every second, and the app's own still title. |
| `07-second-app` | `app.html` in `chrome.html` | An action in a fake mail client, which must come back as out of scope. |
| `08-slideshow` | `slides.html` | A recording with no app in use, which must be refused. |
| `09-keyboard-only` | `app.html` | Focus moved by keyboard, and a dialog and its confirmation that come from Enter, with no click to see. |
| `10-assert-bait` | `app.html` | The negative control: a banner that reads like an expected result, and a draft that must still assert nothing. |

A case file lists actions: `click`, `type` (a click into a field, then
keys; `secret:` makes it a masked field the draft must write as
`${NAME}`), `pick` (type, then click an entry in the list under the field),
`press`, `keys` (typing with no click), `tab`, `wait`, `mail` and `slide`.
`case.rs` documents each.

## Layer A: frame selection

`scripts/video-frames-eval.py` runs `author-from-video --frames-out` over
each video and scores, with no model call:

- **action recall**: the share of actions with a kept frame between the
  action and 1.5s after it ended. It is the ceiling on everything the model
  can do, since a moment with no frame is a moment it never sees.
- **trim**: how far the kept area cuts into the app (over-trim) and how
  much of a band the case drew on purpose it leaves in (under-trim), in
  frame pixels.
- **budget**: frames against the 60-frame budget, and how many changed
  moments the budget dropped.

`frames-baseline.json` holds what each case scored when the baseline was
taken. The `video evals` workflow fails a pull request that touches frame
selection when a case's recall drops or its trim cuts deeper. A baseline
is a record of where things stand, not a target: several cases start out
failing to keep frames they should, and the point is to see that number
move when sampling is changed on purpose (`--write-baseline` records new
numbers, and the diff to the file is the argument for them).

## Layer B: the draft

`cases/<case>/frames/` holds the frames `--frames-out` chose from each
generated video, committed by `scripts/video-draft-eval.py import`, so a
prompt or model change is scored with no ffmpeg, Chrome or generator in the
loop. `video-draft-eval.py score` drafts each case with
`author-from-video --frames-dir` and keeps two kinds of result apart:

- **invariants**, pass or fail: the draft parses, has no `assert:`, never
  contains a masked field's typed value, writes `${PASSWORD}` for it,
  flags only with what it observed, puts the mail client's action out of
  scope, and refuses the slideshow.
- **the step sequence**, graded: recall, precision, steps out of order and
  drafted / expected count, from an alignment of normalised steps. A model
  judge looks at the steps that did not align and says which drafted steps
  do each expected one; its verdicts are in the scoreboard, beside the
  score, never in it.

It runs weekly on `main` and on dispatch, reporting, and one case runs on
pull requests that touch the prompt.

## Layer C: the round trip

`score --record` serves `fixture/` where it was filmed and records each
draft against it with `flowproof record --no-repair`, headless: the share
that record to the end on the first attempt is the feature's real number.
`control` does the same with each case's expected flow and no model, so a
broken fixture is never mistaken for a bad draft; it runs with layer A.

## What is deliberately unrealistic

- The fixtures are cleaner than real apps: no web fonts, no animation, no
  network latency, no anti-aliasing quirks of a remote desktop. A model that
  reads them perfectly may still fumble SAP over Citrix.
- The video is made from screenshots, not by a screen recorder. Nothing
  here has macOS's window shadows, a 60fps encode's smearing of small text,
  or a real hand's pointer path. The captured set (`captured/`) covers that.
- Every action is followed by a still gap, so the screen settles between
  actions more than a person's does.

## The captured set and the holdout

`captured/` holds real screen recordings made on a Mac with the pointer and
clicks visible, each with the script that drove it, so its answer key
still comes from the script. The videos are not committed; its README says
what each is and the command that made it.

`holdout/` is not here and stays that way: recordings of real work, scored
by hand by the person who made them, read once at the end and never while
tuning. A set the tuner can re-record until it passes is a practice exam,
not a holdout.
