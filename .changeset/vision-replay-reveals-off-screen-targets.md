**A recorded vision flow replays when its target is below the fold, or OCR
reads its case differently.** Replay waits for a step's target to exist
before acting, and that wait is a pure read of the visible frame - so a
target the recording had scrolled to timed out there, before the action's
own search could run. The wait now asks the driver to reveal the target
once before polling and once before giving up, never on every poll; only
`app: vision` does anything with it. Separately, a field recorded as
`Fake@email.com` read back as `fake@email.com`: vision anchors now fall
back to a case-insensitive match, as every other kind of anchor already
did, with an exact-case match still preferred. And a vision clear now
clears: it is recorded as typing nothing over a selected field, which
left the old value selected and still on screen.
