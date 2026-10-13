**A vision recording no longer second-guesses a plain `Type`, and says when a
check cannot fail.** On `app: vision` the model is shown only OCR'd text, and
a step that names no element gave it nothing to ground - only a screen to
doubt: after a double-click that visibly opened an input, it refused a bare
`Type` as "the previous step left a problem", and looped on another. Such a
step is now performed as the grammar reads it. Separately, a type step whose
value never reached its field replayed green, because the page also printed
that value in a code sample and `page shows` matched it there. When a vision
check's text was already on screen before the actions it follows, recording
now warns that the check cannot fail if they do nothing.
