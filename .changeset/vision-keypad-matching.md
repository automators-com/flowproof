**`app: vision` clicks the Calculator key it was asked for, not one that looks
like it.** Four matches clicked the wrong thing: `1` matched the `1/x` key by
prefix, `×` matched the italic `x` of `x²`, `5` matched the Keep-on-top icon
OCR read as `5` (switching Calculator to compact mode), and `0` and `×`
matched the window's own maximize and close buttons - the last closed
Calculator mid-flow. A one- or two-character anchor now never matches by
prefix, is looked for as a key of its own at every scale before it may match
inside a phrase, and the caption buttons are never a target; `×` matches the
`X` OCR reads it as, and a thin `1` the `I`. `Type 12+30=` is no longer
failed for not showing `1230`, a model's `*` is typed rather than refused,
and a key OCR cannot read at all - Calculator's `=` - fails saying so, and
that `Type =` presses it.
