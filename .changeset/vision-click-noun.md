**`Click the "5" button` is a click, and vision can click inside a phrase.**
The grammar refused a `Click` with a role noun after its label, though that
is how people write it and `Press the "Save" button` always took one. On
`app: vision` a step the grammar refuses goes to the model, and the model,
shown a Calculator keypad row as the one OCR line `7 8 9 ×`, answered with a
click a third of the way along it - which the vision driver refused as
"clicking at an offset is not supported by this driver". The simplest
calculator flow failed on its first step. `Click` now takes an optional
`button`, `link`, `tab`, `key` or `icon` after the label, which never changes
what resolves, and vision performs a click at a point inside the matched
text's box.
