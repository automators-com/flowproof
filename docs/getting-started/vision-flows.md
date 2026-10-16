---
title: "Vision flows"
description: "Pixels-only flows for Citrix, RDP, and anything else only reachable by screen."
---

`app: vision` drives a window with **no accessibility API at all**:
perception is OCR over captured frames, action is real mouse/keyboard
injection. This is the mode for Citrix/RDP sessions where the remote app
is just pixels on your screen (Windows-only today: capture + SendInput).

## Write a vision flow

```yaml
name: Post order
app: vision
window: Citrix Receiver        # title (substring) of the window to drive
steps:
  - Type ZOR into the "Order Type" field    # OCR finds the LABEL; the click
                                            #   lands right of it, in the field
  - Press the "Submit" button               # clicks the text itself
  - assert: page shows Order saved          # asserts on the OCR'd frame
```

Text anchors match OCR lines exactly first, then by prefix; `the 2nd
"Amount" field` disambiguates repeats in reading order. The recorded
trace carries `provenance: vision` text anchors with their spatial
`relation` (`inside` for clicks, `right_of` for fields), and freeform
steps work through the LLM author; the OCR lines are the scene. OCR
models (pure-Rust [ocrs](https://github.com/robertknight/ocrs), ~12 MB)
download on first use to `~/.cache/flowproof/ocrs`.

An action whose text is not on screen is looked for by scrolling: the
mouse wheel turns a page at a time, down to the bottom and back up, and
the step acts where the text is found. Replay repeats the same search, so
start each run from the same page. Assertions read only what is visible.

OCR loses punctuation more than letters, so a positive `page shows` on a
vision surface also matches with punctuation at the edges of words, case
and spacing ignored: `page shows Hello, Ada` holds against an OCR reading
of `Hello Adal`. Punctuation inside a word still counts (`$10.00` is not
`$1000`), and `does not show` stays exact.

## How vision acts

- **The window.** Vision brings its window to the front before it reads or
  acts, and again whenever another program has taken the foreground since -
  a capture copies the screen and input goes to whatever is in front, so a
  covered window would be read and clicked as the window on top of it. If
  Windows will not hand over the foreground, the step fails and names the
  window in the way. Capture covers the window as drawn, not its invisible
  resize border.
- **Fields.** A field is typed beside its label (the SAP GUI layout) or, when
  the value does not appear there, below it - recording reads the screen back
  after typing and keeps whichever worked, so the trace records where the
  field is. A typed value that never appears on screen fails the step instead
  of passing; a value from a `${...}` reference is never read back, since a
  password field shows dots.
- **Clicks.** Click, double-click and right-click land on the matched text.
  A right-click in a browser also opens the browser's own menu, which takes
  focus - add `Press Escape` after it before typing.
- **Checkboxes.** `Check` and `Uncheck` click the label and watch the box
  beside it change; a click that changes nothing visible is refused. Asking
  whether a box *is* checked is not something pixels can answer on their own.
- **Reading.** Checks read the screen twice, half a caret blink apart, so a
  blinking text cursor beside freshly typed text cannot garble it.

## Know the current limits

Vision flows are Windows-only. They do not yet support visual-template matching
or OCR-region synchronization conditions. A target inside a scrolling area
*within* the page (a grid or list with its own scrollbar) is not searched -
the wheel scrolls the window. And the window has to be on a display that is
actually repainting: a VM or remote desktop left idle can stop updating its
screen, and every flow then fails for reasons that have nothing to do with it.
