**`app: vision` grounds like a computer-use agent, and is measured doing it.**
Vision found a control only if OCR read its text. A button drawn as an icon,
a key drawn as strokes (`=`, `−`, `÷` on Windows Calculator), or a label
that repeats on the page (Settings shows "Windows Update" as a heading above
the nav item of that name) was missed or, worse, clicked in the wrong place.
And nothing measured how often. A grounding benchmark now does
(`examples/vision_bench`, `scripts/vision-bench/run.ps1`). It asks the
window's own accessibility tree for the answer, checks whether
`Click "<name>"` lands inside the named control, and runs on eight desktop
apps. Pixels alone scored 26%. Four changes, borrowed from how Microsoft's
UFO2 and the frontier computer-use agents work, raise that:

- On a local window, vision now also reads the accessibility tree, as UFO2
  does. A control the tree names exactly is the target, unless another
  window is drawn over it, in which case the text on screen wins. With the
  tree, the benchmark scores 100% on seven of eight apps, with no wrong
  clicks. A Citrix or RDP client reports no tree, so vision stays
  pixels-only there.
- Operator keys are recognised by their shape: `=` is two bars of one
  width, `÷` a bar between two dots. Pixels-only, Calculator now computes
  `9−4=5`, `÷5`, `×7` and `+3` by clicking.
- Reading is done by two OCR engines, merged. Windows' own engine reads
  interface text faster, and `ocrs` reads the lone glyphs Windows drops.
- A field addressed by its label lands inside the box drawn there, not at a
  fixed offset.

Vision also gains `drag` and `hover`, and waits for the window to stop
changing before it reads a target. Recording a vision step sends the model
the screenshot as well as the OCR'd element list, so it can see which of the
listed elements the step means.
