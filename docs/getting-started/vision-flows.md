---
title: "Vision flows"
description: "Pixels-only flows for Citrix, RDP, and anything else only reachable by screen."
---

`app: vision` drives a window with **no accessibility API at all** —
perception is OCR over captured frames, action is real mouse/keyboard
injection. This is the mode for Citrix/RDP sessions where the remote app
is just pixels on your screen (Windows-only today: capture + SendInput).

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
steps work through the LLM author — the OCR lines are the scene. OCR
models (pure-Rust [ocrs](https://github.com/robertknight/ocrs), ~12 MB)
download on first use to `~/.cache/flowproof/ocrs`. Deliberately not in
this slice yet: visual-template matching and OCR-region sync conditions.
