**A `when:` or `repeat: until:` condition now reads `${VAR}` while recording.**
Recording compared the condition's raw text against the app, so
`when: page shows ${ORDER_ID}` looked for the literal `${ORDER_ID}` and never
held: the block was never recorded, and every replay passed without running
the checks inside it. A `repeat:` whose `until:` named a variable ran to its
bound. Replay already resolved the reference, so record and replay read the
same condition differently. Recording now resolves it for the comparison, the
trace keeps the reference, and an unset variable fails the recording by name.
