---
status: ready
model: opus
effort: high
---
# Plan 16 - Testing and evals for `author-from-video`

## Problem

`flowproof author-from-video` has been tried on one recording. Everything we
believe about it comes from that one file, and the code has a lot of places
where a different recording would behave differently.

Walk through what actually happens to a video. The CLI hands it to
`video_frames::extract` (`crates/flowproof-adapters/src/video_frames.rs`),
which decodes the whole thing into 128px greyscale thumbnails at two samples a
second, keeps a sample when its mean brightness differs from the last kept one
by more than `CHANGE = 0.003`, trims any band at an edge of the screen that
changed fewer than `STILL_CHANGES = 3` times in the whole recording, and then
re-decodes the kept moments as JPEGs at 1280px wide. At most 60 of them
survive, and past that "the largest changes win". Those 60 pictures go to the
model in one call, and `video_author::parse_reply` turns its `STEP:` /
`UNEXPLAINED:` / `OUT_OF_SCOPE:` / `NOT_APP:` lines into a draft.

Every constant in that paragraph is a guess that one video happened to agree
with. Some of what could go wrong:

- **The 60-frame budget.** A twenty-minute recording with ninety actions in it
  loses most of them before the model sees anything, because sampling also
  thins out past `MAX_SAMPLES = 720` (six minutes at the default half-second
  step). We do not know how much is lost, and nothing tells the user that
  frames were dropped.
- **The change threshold.** `CHANGE` is low on purpose so that typing a short
  value into one field registers. Low thresholds also keep frames for a mouse
  pointer drifting across a page, and those spend the budget.
- **Edge trimming.** A taskbar with a ticking clock, a Dock that magnifies on
  hover, a browser tab strip that animates: each of these changes more than
  three times, so the band stays and the frames carry it. In the other
  direction, a still band that happens to hold something the model needs gets
  cut.
- **Scale.** A Retina screen recorded at 2560px wide arrives at the model as
  1280px, and a 10px label in SAP becomes unreadable. Nothing measures where
  that floor is, which is the same question the terminal-grid evals answer for
  vision by running the same flows at four font sizes.
- **The prompt's own rules.** "A street and a house number are two Type lines",
  "a count going from 2 to 3 and then to 4 is two clicks", "never write an
  assertion", "a masked field is `${PASSWORD}`". Each rule is a claim about
  model behaviour that holds or does not hold per recording, and the one video
  exercised maybe half of them.

So the thing worth building is not a bigger pile of videos. It is a way to put
a number on each of those behaviours and watch the number when we change the
prompt or the sampling.

### Why this plan is here and not in the desktop

The feature has two halves and this plan is about the engine's. Everything
above lives here: the frame selection in `flowproof-adapters`, the prompt and
the reply parsing in `flowproof-agent/src/video_author.rs`, the CLI wiring in
`flowproof-cli/src/lib.rs`, and the `evals/` convention the plan reuses.

The desktop app owns the sheet around it, which is a thin surface: the
extension gate (`VIDEO` in its `src-tauri/src/draft.rs`), the progress lines
parsed out of this engine's stderr (`parse_frame_progress`), and the error
wording (`explain_for`). All three already have unit tests covering every video
error the engine can raise, and none of them care what is in the recording. The
desktop's share is a short manual pass, written up at the end of the
implementation guide, and it is tracked in that repository rather than here.

## Approach

Three layers, cheapest first, because only the cheapest one can afford to run
on every pull request.

### Where the videos come from

The obvious idea is to have an agent record a real screen, and we should turn
that down for the generated corpus, for a plain reason: a recorded screen
still needs somebody to write down, by hand, what the correct draft would have
been. That leaves us with one video nobody can reproduce and one answer key
somebody typed, which is what recording it by hand gives us with none of the
machinery. A recording of a real screen also cannot be committed to
`flowproof`, which is a public repository.

So we build the recording instead of capturing it, the same move
`evals/terminal-grid` already makes. That corpus needed a host terminal screen
to test vision against, there was none, so it has a replica that draws one from
a YAML layout and writes its own state out as the ground truth. We do the same
here: invented apps as plain HTML pages, and a driver that performs a scripted
action list against them while taking a screenshot every 100ms. The screenshots
are muxed into an `.mp4`, and the driver writes out the expected `.flow.yaml`
plus the exact millisecond of every action as it goes. The answer key comes from
the script that caused the actions, so it cannot drift from the video.

Every fixture is invented, including the SAP-shaped one. It is a dense grey form
with 10px labels and a function-key hint bar, written as HTML, with made-up
material numbers in it. No real system is reached, no address is published and
nothing in the corpus points at the Fiori account the `evals/fiori` work uses.
That case exists because 10px grey-on-grey labels at 1280px are where the
scaling floor will bite first, and that is where the customers are.

That gets us reproducible videos, fixtures we can deform on purpose (record at
2560px, draw a taskbar on top, stretch the idle gaps to twenty minutes), and
nothing sensitive in the repo. What it gives up is realism: a page we wrote is
cleaner than SAP over Citrix, and a model that reads our fixture perfectly may
still fumble a customer's Teams recording. The holdout below is how we keep
ourselves honest about that.

Note that muxing needs a full ffmpeg. The one that ships beside the engine is
built decode-only by `scripts/ffmpeg/build.sh`, so the generator uses whatever
ffmpeg is on the machine, and the engine's own bundled binary stays on the
decode path where it belongs.

### The cursor has to be in the frames

A screenshot taken through Chrome's debugging protocol has no mouse pointer in
it, and that would make the whole corpus easier than real life without anyone
noticing. The prompt's rules lean on seeing what was used: a change no visible
action explains is what triggers an `UNEXPLAINED:` line, and a corpus where the
pointer is never visible would make the model flag things it should have read,
or read things it should have flagged, and we would be measuring the wrong
feature.

So the generator draws the pointer into every screenshot at the position it is
about to act on. The engine already has that code: `draw_pointer` and
`draw_ring` in `crates/flowproof-driver/src/recording.rs:352` draw a cursor
bitmap and two high-contrast rings into captured frames for the run filmstrip,
behind `highlight_cursor`. The generator reuses it, drawing the plain arrow for
movement and the rings on the frames where a click lands, which is also what
macOS does with "show clicks" turned on.

One useful side effect: a visible pointer moving between actions is exactly the
small-change noise that `CHANGE = 0.003` keeps frames for, so layer A finally
measures how much of the 60-frame budget a drifting cursor spends.

### Layer A: frame selection, no model

Given a case, we know every action's timestamp. So we can ask, with no model
call at all, whether `video_frames::extract` kept a frame that shows each
action's effect: for each action at time `t`, is there a kept frame in
`[t, t + window]`? That ratio is the ceiling on everything the model could
possibly do, and it is the number most likely to move when somebody tunes
`CHANGE` or `every_ms`.

Alongside it, two checks that are plain assertions: the `kept` area equals the
screen minus the bands we drew on purpose and no more, and the frame count
stays inside the budget.

The frames and their times come out of the engine through a new
`author-from-video --frames-out <dir>`, which writes the kept JPEGs and a
manifest and calls no model. `--check` stays the flag that writes nothing.

This layer is free, deterministic, and fast, so it gates. A committed baseline
holds a per-case threshold and CI fails when a case drops below it, the way
`evals/fiori/replay-reliability-baseline.json` is used today.

### Layer B: the draft, one model call per case

Layer A's generator writes the kept JPEGs out beside each case, and those get
committed. Layer B reads frames from disk rather than decoding video, so it
needs no ffmpeg, no Chrome and no generator run to score a prompt change. A
case is a folder: the frames, `expected.flow.yaml`, and a small `case.json`
with the app id, the duration and the start URL.

Scoring splits in two, and keeping them apart is the point.

**Invariants pass or fail with no judgement involved.** The draft parses as a
spec. It contains no `assert:` anywhere, whatever the frames bait the model
with. A masked field's value is `${PASSWORD}` and the literal string we typed
into it appears nowhere in the output. Every flagged line carries its
`observed` text. The not-an-app case comes back as `NotAnAppRecording` rather
than as a draft of observations. The second-app case produces an
`out_of_scope` line. These are the feature's promises, they are cheap to check,
and any one of them breaking is a real regression.

**The step sequence gets a graded score.** Align the drafted actions against
the expected ones as a longest common subsequence over normalised steps, where
a match needs the same grammar form (`Go to` / `Click` / `Type`), the same
target label and, for a `Type`, the same value. Report recall, precision, how
many steps came out of order, and the ratio of drafted to expected step count.
That last one matters more than it looks: a draft with four steps for a
twelve-step recording is a worse product than a draft with twelve steps where
two labels are slightly off, and a diff-based eval scores them the same.

Steps that fail to align mechanically go to a model as a judge, one call per
leftover pair, asking only whether the two describe the same action. This
catches the near-misses that matter (`Open Produkte and select Haushaltsstrom`
against a `Click` plus a `Click`, which is a legitimate split rather than a
miss) and it stays out of the pass/fail verdict. Its answers land in the
scoreboard JSON so a run can be read back and argued with, which is what the
Fiori scoreboards are for.

#### What a pass costs

60 frames is the ceiling, and most cases are nowhere near it. A twelve-action
case on a page that is still between actions keeps maybe fifteen frames, and a
1280px JPEG is roughly 1.2k input tokens, so that case costs about 18k tokens.
Only the twenty-minute case and the Retina case reach the 60-frame ceiling at
about 75k each. A full ten-case pass is therefore somewhere around 250k to 350k
input tokens, which is well under a dollar on a Sonnet-class model. The judge
compares two short strings with no pictures at all, so it rounds to nothing.

That is cheap enough to run on a weekly schedule on `main`, which is what
catches a provider changing its model under us, and cheap enough that one
chosen case can run as a smoke test on pull requests that touch the prompt or
the reply parsing. One case is a few cents and it would catch the kind of
mistake that makes every draft fail, which is the mistake worth catching
before merge. The full pass stays off pull requests, because the model key is a
secret and `flowproof` takes pull requests from forks.

### Layer C: the round trip

The fixture is a real page we own, so a draft taken from a recording of it can
be recorded straight back against it. Video in, draft out, `flowproof record --no-repair` against the same page, and either the recording reaches the end or
it does not. That is the same first-attempt measure the Fiori corpus calls FAA,
and it is the only number that says the feature works rather than that the
model wrote plausible text.

It is the most machinery of the three, so it is the last of the stacked pull
requests below. The decision it forces now is that the fixtures have to be DOM
pages with real labels, which rules out drawing the fake apps on a canvas the
way the terminal replica draws its screen.

### The captured set, and the holdout

A generated video still goes through our own screenshot loop rather than
through a real screen recorder, so it misses whatever macOS does to a window,
what a 60fps H.264 encode does to text, and what happens when a person moves
the mouse like a person. The implementing agent closes that gap by recording
its own screen for real: it drives a browser window on this machine, records
with `screencapture -v -C -k` (video, cursor included, clicks shown) or with
ffmpeg's avfoundation input and `-capture_cursor 1 -capture_mouse_clicks 1`,
and because its own drive script caused every action, the answer key still
comes free. These go in `evals/video-to-flow/captured/`, uncommitted, with a
README naming them and the script that made them.

Two of them should be of apps we can point at without any credentials: a public
web app, and the Flowproof desktop app itself being used. One should be the same
recording re-encoded at a low bitrate to imitate what Teams does to a screen
share, which gets the compression signal without needing a meeting. A real SAP
GUI or Fiori session needs your credentials and the private system, so that one
stays yours to record or does not happen.

`evals/video-to-flow/holdout/` stays separate and stays yours: recordings of
your own real work, scored by hand, read once at the end and never during the
build. The reason to keep it apart is that the agent recording the captured set
is the same agent tuning the prompt against it, and a set you can re-record
until it passes is a practice exam rather than a holdout. That is the
discipline `evals/fiori/holdout/README` protects by insisting its specs are
hand-written by the person running the brief.

So: the generated corpus answers "did we break it since Tuesday", the captured
set answers "does it survive a real screen recorder", and the holdout answers
"does it work on video somebody actually made".

### Is an eval a flow file

Yes. The expected artifact per case is a
`.flow.yaml` sitting beside the frames, which makes the corpus readable and
reviewable the way `evals/fiori/dev/*.flow.yaml` is. Two things keep it from
being a diff. The comparison is a rubric, because a model that writes `Click
"Continue"` where we expected `Click "Weiter"` has made one mistake and should
not score zero. And the expected file is emitted by the generator rather than
written by hand, because a hand-written answer key is a thing people end up
editing until the prompt passes. Anyone adding a case by hand needs to know
that temptation is there.

## Implementation guide

Four pull requests. PR 2 branches off PR 1 and PR 3 off PR 2, because each
needs the harness the one before it adds; PR 4 goes off `main` on its own.
Retarget each stacked one at `main` before it merges, so it does not sit on a
dead base.

### PR 1 - the fixtures, the generator and layer A

- [ ] `evals/video-to-flow/fixture/app.html` - one invented web app, self
  contained, no network. A login panel with a masked password field, a
  search box with a typeahead list where the typed text differs from the
  picked entry, an address form whose street and house number are separate
  fields, a quantity stepper, a tab strip, a dialog that opens on Enter
  with no visible click, and a banner that reads like an expected result to
  bait an `assert:`. Every name and number in it is made up.
- [ ] `evals/video-to-flow/fixture/sap-form.html` - the second invented app: a
  dense grey transaction form with 10px labels, a function-key hint bar and
  made-up material and plant numbers. Nothing real is reached and no
  address is published; this is the scaling-floor case, drawn by hand.
- [ ] `evals/video-to-flow/fixture/chrome.html` - the deformations, drawn
  around a fixture in an iframe: a taskbar band at the bottom with a clock
  that ticks every second, a menu bar at the top, and a full-screen fake
  mail client that can be shown for a few seconds mid-run.
- [ ] `evals/video-to-flow/generator/` - a workspace member crate
  (`flowproof-video-fixture`, next to `evals/terminal-grid/replica` in
  `Cargo.toml`'s `members`) that drives a fixture with `headless_chrome`,
  screenshots it every 100ms, muxes the sequence with the system ffmpeg,
  and writes `video.mp4`, `actions.json` (every action with its
  millisecond) and `expected.flow.yaml` per case.
- [ ] The pointer, drawn into every screenshot before the mux, since a CDP
  screenshot has none. Reuse `draw_pointer` / `draw_ring`
  (`crates/flowproof-driver/src/recording.rs:352`): the arrow for movement
  between actions, the rings on the frames where a click lands. Move the
  pointer across a few frames rather than teleporting it, so the drift is
  real noise the budget has to survive.
- [ ] `evals/video-to-flow/cases/*.toml` - one file per case: the fixture, the
  action list, the viewport, which deformations are on, and how long the
  idle gaps are. Ten to start: a four-step smoke, a twelve-step order form,
  a counter-and-tabs case for the "2 to 3 to 4 is two clicks" rule, a
  twenty-minute idle case, a 2560px Retina case, a taskbar-and-menubar
  case, a second-app case, a slideshow with no app in it, a keyboard-only
  case that advances on Enter, and the assert-bait negative control. The
  SAP form carries the Retina case and one of the short ones.
- [ ] `crates/flowproof-cli/src/lib.rs` - `author-from-video --frames-out

  <dir>`: write the kept JPEGs and a manifest of their times, call no
  model, write no draft. Document it in `docs/authoring/troubleshooting.md`
  beside `--check`.
- [ ] `scripts/video-frames-eval.py` - for each case, run `--frames-out` over
  the generated `video.mp4`, then score action recall inside the window,
  the `kept` area against the bands the case drew, and the frame count
  against the budget. Writes a scoreboard JSON in `evals/video-to-flow/`,
  shaped like `evals/fiori/replay-reliability-baseline.json`.
- [ ] `scripts/test_video_frames_eval.py` - unit tests for the scoring,
  alongside `scripts/test_fiori_eval.py`.
- [ ] `evals/video-to-flow/frames-baseline.json` - the committed per-case
  thresholds CI compares against.
- [ ] `evals/video-to-flow/README.md` - what the corpus is, what each case
  exists to catch, what is deliberately unrealistic about it, and the
  holdout rule. Follow `evals/fiori/dev/README.md`'s register, including
  its "what is unverified, on purpose" section.
- [ ] `.github/workflows/video-evals.yml` - the layer A job: `pull_request` on
  paths touching `video_frames.rs`, `ffmpeg.rs`, the fixtures or the
  harness. Installs ffmpeg and Chrome, regenerates the corpus, scores it,
  fails on a drop below baseline, puts the table in the run summary.

### PR 2 - layer B, the draft scored against the expected flow

- [ ] `evals/video-to-flow/cases/<case>/frames/*.jpg` plus `case.json` - the
  committed frames, written by PR 1's generator, so a prompt change can be
  scored without regenerating any video.
- [ ] `crates/flowproof-cli/src/lib.rs` - `author-from-video --frames-dir

  <dir>`: draft from committed frames instead of a video.
  `video_author::draft_from_frames` is already public and takes frames plus
  a client, so this is the way in from the CLI.
- [ ] `scripts/video-draft-eval.py` - reads the frames, drafts through the
  engine, checks the invariants, aligns the step sequence, calls the judge
  for the leftovers, writes a scoreboard JSON with every verdict in it.
- [ ] `scripts/test_video_draft_eval.py` - tests for the normalisation and the
  alignment, which are the parts most likely to rot without anyone
  noticing.
- [ ] `.github/workflows/video-evals.yml` - two more jobs. The full pass on
  `workflow_dispatch` and a weekly `schedule`, `main` only, model key from
  secrets, reporting and never gating the way `vision-evals.yml` does. And
  a one-case smoke on pull requests that touch `video_author.rs` or the
  prompt, which costs a few cents and only fails when every draft would
  fail.

### PR 3 - layer C, the round trip

- [ ] `scripts/video-draft-eval.py` - a `--record` pass that serves the
  fixture, records the draft against it with `--no-repair`, and reports a
  first-attempt rate per case.
- [ ] Add the number to the layer B scoreboard and the run summary.

### PR 4 - docs

- [ ] `docs/authoring/troubleshooting.md` - once the numbers exist, say what
  the feature's measured limits are in the `author-from-video` section.
  Today it describes the mechanism and promises nothing about accuracy,
  which is the right place to put "a recording longer than about N minutes
  loses steps".
- [ ] `internal/how-flowproof-tests-flowproof.md` - the assert-bait negative
  control is a red-path proof in that page's sense (an invariant that
  cannot fail is not an invariant), so it belongs in the table there.

### The captured recordings

Done by the implementing agent as part of PR 2, since that is where there is a
harness to score them with. Nothing here is committed.

- [ ] Three or four real screen recordings, made on this machine with the
  cursor and clicks visible: `screencapture -v -C -k out.mov`, or ffmpeg's
  avfoundation input with `-capture_cursor 1 -capture_mouse_clicks 1`. Needs
  Screen Recording permission for the terminal, which is a one-time prompt.
- [ ] One of a public web app, one of the Flowproof desktop app being used,
  and one of those re-encoded at a low bitrate to imitate a Teams screen
  share. Each with the drive script that produced it, so the expected flow
  comes from the script rather than from memory.
- [ ] `evals/video-to-flow/captured/README.md` - what each recording is, where
  it lives on disk, and the command that made it. Committed; the videos are
  not.
- [ ] Score them with the layer B harness and write the numbers into the
  scoreboard beside the generated cases, labelled as captured so nobody
  reads the two sets as one.

### The desktop app's own pass

Not part of these pull requests, since none of it is engine code. It is a
manual pass in `flowproof-desktop`, run once per release that changes anything
here, and recorded there:

- [ ] A twenty-minute recording: does the progress bar move sensibly across
  roughly 720 samples, and does the draft sheet stay responsive.
- [ ] Cancel while frames are being found, and again during the model call.
  `cancel_draft` kills the child; check the temporary folder is gone.
- [ ] An HEVC `.mov` straight off an iPhone, a `.webm` from OBS, a `.mkv`.
- [ ] A recording of something that is not an app, for the refusal wording.
- [ ] The draft sheet's header for a video, which reports length and no step
  count because `--check` has no steps to report for a recording.

## Out of scope

- Teaching the feature to write assertions. A recording states no belief about
  what is correct, and that invariant is one of the things the evals defend.
- Any change to the sampling constants or the frame budget. This plan measures
  them. Changing them is the next piece of work, and it should be argued from
  the numbers.
- A video corpus in `flowproof-desktop`. Its share is parsing and wording, both
  already unit-tested there, and duplicating the corpus would double the
  maintenance for no new signal.
- Real customer recordings in the repository. The holdout stays outside it.
- Any eval that reaches a real system. Every fixture is invented and local,
  including the SAP-shaped one.

Three unrelated things found while reading the code, each worth its own branch.
One is here, two are in `flowproof-desktop`:

- `author-from-video --check --json` reports `duration_ms`, `width` and
  `height` only (`crates/flowproof-cli/src/lib.rs:4319`), while the desktop's
  `check_draft_source` asks for `steps` and `with_expected` and gets 0 for
  every recording. The CLI's own help text and
  `docs/authoring/troubleshooting.md:176` both say `--check` reports how many
  frames would be sent, and it does not.
- `flowproof-desktop`'s `src-tauri/src/draft.rs:32` leaves `.avi` out of the
  accepted extensions because "Chrome generally cannot play it". Chrome no
  longer decodes recordings, the bundled ffmpeg does, and
  `docs/authoring/troubleshooting.md:184` lists `.avi` as readable. The
  exclusion and its comment are both left over from before #818.
- The same file's `explain_for` tells the user a recording "cannot be played
  here" (`src-tauri/src/draft.rs:274`), which is Chrome-era wording for what is
  now an ffmpeg decode failure.

## Open questions

- ~~Engine repo or desktop repo?~~ Decided: the engine, as
  `plans/016-video-to-flow-evals.md`. The code under test, the `evals/`
  convention and the eval harness precedent are all there.
- ~~An agent records your screen to build the corpus?~~ Decided: no for the
  generated corpus, which needs to be reproducible and committed. Yes for the
  captured set: the implementing agent records its own screen for real, with
  the cursor visible, and its drive script supplies the answer key. See "The
  captured set, and the holdout".
- ~~How should the harness get the kept frames and their times out of the
  engine?~~ Decided: a new `author-from-video --frames-out <dir>`, so the
  frames land in a folder of their own and `--check` stays the flag that
  writes nothing.
- ~~Is a weekly scheduled layer B run worth the model spend, or should it be
  dispatch-only?~~ Decided: weekly on `main`, plus dispatch, plus one chosen
  case as a smoke test on pull requests that touch the prompt. A full pass is
  cheaper than first estimated (see "What a pass costs"): most cases keep
  nowhere near 60 frames, so a pass is roughly 250k to 350k input tokens and
  one case is a few cents.
- ~~Does layer C belong in this plan's build or the next one?~~ Decided: in
  this one, as its own pull request. All three layers plus the docs ship as
  four stacked pull requests, split in the implementation guide above.
- ~~Should the corpus include a SAP-shaped case?~~ Decided: yes, and it reaches
  nothing real. It is an invented HTML page with 10px grey labels, a
  function-key hint bar and made-up material numbers, drawn by hand the way
  `evals/terminal-grid`'s replica draws an invented host screen. No address is
  published and the Fiori account the `evals/fiori` corpus uses is not touched.
- ~~Can you record three or four real videos of your own for the holdout?~~
  Decided: the implementing agent records them, with the cursor and clicks
  visible, and they become the captured set in PR 2. The cursor requirement
  also lands on the generated corpus, which needs the pointer drawn into every
  frame because a CDP screenshot has none; that turned out to matter for what
  the evals measure, so it has a section of its own above.

  What the agent cannot record is a real SAP GUI or Fiori session, which needs
  your credentials and the private system. If a recording of one would be
  useful, it stays yours to make, and it is the one thing left that nobody
  else can supply.
  - Answer: The agent that will be working on this will be responsible for recording these and it would be great if the recordings could show a cursor too so it’s easier

