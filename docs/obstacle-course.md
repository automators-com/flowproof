# What a deliberately awkward page taught us

A public obstacle web page — 40 pages, each built to be hard to automate,
each scoring itself by calling its own `obstacleCompleted()` — was used as a
corpus to find gaps in flowproof's grammar. This is what it found, so the
next person starts from the answers rather than the questions.

**28 of 40 are solved.** The other 12 are listed with reasons, because
"hard" is not a reason and a gap nobody has named gets rediscovered.

## What it was actually good for

Not the score. Four separate defects it surfaced share one shape:

> an operation answered a NARROWER question than the one asked, and the
> narrower answer was always yes.

- `is visible` checked that a selector RESOLVED. A `display:none` element
  resolves, so the assertion could not fail on the one condition anybody
  writes it for.
- A table cell was addressed by counting `th`s against `td`s. A header row
  with a stub cell shifts every data cell one place, so the assertion passed
  against the neighbouring column — a real cell, with real text.
- `Select` fell through to TYPING when no option matched. Typing into a
  `<select>` is itself a prefix search, so the step selected some other
  option and reported success.
- The published JSON Schema omitted two shipped action types. Nothing
  caught it because the conformance fixture exercised neither.

The fifth is the systemic one. `record` falls back to the LLM author for
anything the rules cannot parse — so every shape the grammar had DECLINED
was being silently improvised into a recording. `Click "Next" until the
label changes` became one click, recorded green, and failed one replay in
five. A decline that exists only in prose is a decline the engine does not
implement.

## The 9 that are not solved

### No completion path exists (5)

22505, 12952, 73590, 73591, 60469.

Their inline scripts never execute — `typeof drag === "undefined"`, no
handler bound, no call to `obstacleCompleted()` anywhere. Verified by
clicking in a real browser and watching the network: no request is made.
flowproof performs the correct action on several of them; the page simply
has no way to say so. **Not a flowproof limitation.**

### Blocked on step latency (2)

87912 accepts a generated value for 2 seconds. 16384 deletes its record
after 5. A step costs ~3.2s (measured; see `docs/authoring.md`), so both
deadlines pass before the next step lands. On 16384 the record is already
gone by the time the SECOND step runs.

The failure is at least loud — the page's own complaint surfaces as a failed
step, and flowproof's dialog safety net catches the alert — but neither is
reachable without making steps substantially cheaper, which would mean
giving up the verification that makes a step trustworthy.

### Taken by `Drag` (1)

23292 is solved: six rows dragged into order, recorded and replayed green
three times running. What unblocked it was not the API the notes twice named
but two structural defects in the dispatch — the two midpoints were read in
different layouts, and the moves named no held button. Measured 20/20; see
`docs/design.md`.

### Passes vacuously (1)

51130 completes without anything closing a window: its check fires when the
popup is `closed`, and the popup never really opens headless. Counting it
would be exactly the false green the rest of this work removed. **No
cassette for it may enter the repository.**

### Out of reach on timing (1)

82018 is a reaction game whose window depends on the frame rate, and a step
costs about 3.2 seconds. Measured, not assumed; it is the one obstacle in
the corpus no amount of grammar reaches.

## Taken by `repeat:` and `when:` (2 of 3)

70924, 73589 and 82018 were held here pending a decision on repetition and
branching. Two of the three fell once the blocks existed; the third is above.

**70924** resets its counter to zero on every fault, which is a `repeat:`
with a `when:` recovery nested inside it. The recorded trace is 18 presses —
three faults recovered, then a clean run of ten — and the loop, not the
author, worked out which. The condition had to name the button rather than
the page: `page shows ERROR` also matches the page's own "Errors occur"
heading, so the recovery fired every pass and drove the loop into its bound.

**73589** is a hand-driven bubble sort. The decision at each position is
whether the left number exceeds the right, which is the numeric comparison
condition. Its two controls sit at the overflow edge of a fixed-height
container, so the second one is genuinely covered at some window sizes —
pinning `browser.viewport` is what makes it clickable, and finding that out
turned up a defect worth more than the obstacle: **`Press` did not run the
occlusion gate that `Hover` and replay both run**, so a click landing on an
occluder recorded as a success and produced a trace replay then refused.

## What the corpus was worth, mechanically

Two obstacles that looked like they needed control flow did not: **32403**
(branch on a displayed operator) and **41036** (search a table, answer
True/False). Pinned, each is a straight line. Much of what looks like branching is randomness in
disguise, and `browser.random` — one feature — made 16 of the 22 then-open
obstacles expressible.

Two that looked like they needed pointer precision did not: **30034**'s
stripe and **66667**'s maze checkpoints are ordinary DOM elements, reachable
with an ordinary click. The page hints suggest coordinate clicking; the page
does not require it.

The lesson worth keeping: **read the value off the page, do not predict
it.** Simulating the seeded PRNG to precompute values failed twice, because
other page scripts consume draws first. The seed pins the SEQUENCE, not the
position — so a flow that types a constant derived from a draw should assert
the draw first.
