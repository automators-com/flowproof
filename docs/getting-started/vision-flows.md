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

Text anchors match a whole OCR line or word exactly first, then a line by
prefix (for anchors of three characters or more - `"1"` never matches a
`1/x` key); `the 2nd "Amount" field` disambiguates repeats in reading
order. Words far apart on one row - a keypad's keys, a toolbar, a grid's
columns - are read as separate lines, so each is its own target. A target
not found at screen size is looked for again in the same frame read at two
and then three times the size, which finds a thin glyph such as a lone `1`.
An anchor of a character or two is a key: it is looked for as a line of its
own at every size before it may match a character inside a phrase. A glyph
matches what OCR reads it as (`×` as `X`, `−` as `-`, `÷` as `/`, `1` as
`I`), and a character or two in the window's caption band is never a
target: those are its minimize, maximize and close buttons. The recorded
trace carries `provenance: vision` text anchors with their spatial
`relation` (`inside` for clicks, `right_of` for fields), and freeform
steps work through the LLM author. The model is given the OCR lines as the
scene, plus a screenshot of the window, so it can tell which listed
element the step means.

Two OCR engines read each frame, and their readings are merged. Windows'
own engine (`Windows.Media.Ocr`, part of Windows 10 and 11) reads interface
text. Pure-Rust [ocrs](https://github.com/robertknight/ocrs) reads the lone
glyphs Windows drops, such as a keypad's digits, and also does the closer
second looks. The ocrs models (~12 MB) download on first use to
`~/.cache/flowproof/ocrs`. `FLOWPROOF_VISION_OCR=windows` or `=ocrs` pins
one engine.

## The accessibility tree, when there is one

A window running on your own machine usually has an accessibility tree, and
vision reads it alongside the pixels. Microsoft's UFO2 agent fuses the two
the same way.

- A control the tree names exactly is the target, so `Click "Back"` finds
  an icon-only button, and a label that also appears as a heading elsewhere
  resolves to the control. If another window (a popup menu, a dialog) is
  drawn over that control, the text on screen wins instead.
- A key matches the name it is spoken by: `=` matches `Equals`, `7`
  matches `Seven`.
- A field the tree names is typed into directly.

A Citrix or RDP client reports one opaque pane, so over a remote session
vision is pixels-only, as before. `FLOWPROOF_VISION_UIA=0` makes a local
window behave the same way, which proves a flow does not lean on the tree.

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
- **Waiting.** Before reading a target, vision waits (up to 1.5s) for the
  window to stop changing - a menu fading in, a page loading - so it does not
  read a frame mid-animation. A blinking caret does not count as change.
- **Clicks.** Click, double-click and right-click land on the matched text,
  or inside the control when the accessibility tree named it. A field is
  typed into inside the input box drawn beside or below its label, when one
  is drawn there. A right-click in a browser also opens the browser's own
  menu, which takes focus - add `Press Escape` after it before typing.
- **Drag and hover.** A drag presses on one target and releases on the
  other, moving in steps so the app sees it as a drag. Both targets are
  found before the button goes down. Hover moves the pointer onto a target
  without pressing, to open a tooltip or a hover menu.
- **Checkboxes.** `Check` and `Uncheck` click the label and watch the box
  beside it change; a click that changes nothing visible is refused. Asking
  whether a box *is* checked is not something pixels can answer on their own.
- **Reading.** Checks read the screen twice, half a caret blink apart, so a
  blinking text cursor beside freshly typed text cannot garble it.
- **Keys.** `Press Enter`, `Press Ctrl+S` and the like are keystrokes; a
  symbol with no key name (`*`, `=`) is sent as the keystroke that types it.
  A bare `Type 12+30=` types into whatever has focus, and is read back by its
  operands (`12`, `30` - too short, so not at all), since an app like a
  calculator shows the result, not the keys.

## Keypads and symbol-only buttons

OCR reads words better than lone glyphs. A key drawn only as strokes is not
text to OCR at all, so vision recognises the operators by their shape:

- `−` is one bar
- `=` is two bars of one width
- `÷` is a bar between two dots
- `+` and `×` are a square cross and a diagonal one

With pixels alone, every key on Windows Calculator can be clicked:
`Click "="`, `Click "÷"`. A symbol anchor looks for a shape before it falls
back to OCR. OCR still misreads lone digits at large sizes. When the app
takes keyboard input, typing the keys (`Type 12*7=`) is the surest route.

## Measuring vision on your machine

`scripts/vision-bench/run.ps1` opens a set of desktop apps and scores
vision against each app's own accessibility tree. The tree is the answer
key: for each named control, does `Click "<name>"` land inside it? Each
mode is scored in turn:

- `ocrs` and `windows`, one engine each
- `auto`, both engines on pixels alone
- `auto+uia`, with the tree

Each mode reports three numbers: coverage, accuracy by name, and wrong
clicks. A wrong click is one that lands on a different control, which is
worse than a miss.

## Open Windows Start

`Press the "Windows Start button" button` detects a four-pane white or blue
Windows logo in the bottom taskbar on every lookup. Recording saves a semantic
text anchor; replay finds fresh pixels after resizing or moving the window.
Missing, damaged, or duplicate logos fail without a guessed click or keyboard
fallback. The icon's label is excluded from OCR surface-text assertions.
`Press Meta` (also `Press Win` or `Press Windows`) sends the Windows key;
whether Citrix forwards that key depends on the client configuration.

## Know the current limits

Vision flows are Windows-only. They do not yet support visual-template matching
or OCR-region synchronization conditions. A target inside a scrolling area
*within* the page (a grid or list with its own scrollbar) is not searched -
the wheel scrolls the window. And the window has to be on a display that is
actually repainting: a VM or remote desktop left idle can stop updating its
screen, and every flow then fails for reasons that have nothing to do with it.

Start recognition is conservative: bottom taskbars, separated panes 3–28 pixels
wide/high, and gaps 1–6 pixels. Other logos, themes, compressed/occluded glyphs,
and taskbar positions may be unavailable. Synthetic replay tests cover scale
and window movement; live Citrix validation requires the separate Windows demo machine.
