**Automatic repair no longer changes what a check expects.** Told never to
weaken an assertion, the repair model still rewrote a vision flow's correct
`page shows Hello, Ada` to `Hello Ada!` - what OCR had misread on the page -
and saved it to the flow file. A repair that edits an expectation until it
matches is the very failure a test exists to catch. Repair now refuses any
patch to an `assert...:` step and says so, leaving the flow as written; it can
still give a check longer to come true, and it still fixes the steps that act.
