**A vision recording performs the steps it can read, and says when a check
cannot fail.** On `app: vision` the model is shown only OCR'd text: for a step
naming a quoted label it can only produce the anchor the grammar already
reads, and what it added was doubt - after a double-click that visibly opened
an input it refused a bare `Type` as "the previous step left a problem", and it
refused to type into fields it could see only as their labels. On vision a
step the grammar reads completely is now performed as written; anything else
still goes to the model. Separately, a type step whose value never reached its
field replayed green, because the page also printed that value in a code
sample and `page shows` matched it there. When a vision check's text was
already on screen before the actions it follows, recording now warns that the
check cannot fail if they do nothing.
