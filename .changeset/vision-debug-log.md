**`FLOWPROOF_VISION_LOG=1` shows where a vision run clicked.** A pixels-only
run leaves no DOM to inspect afterwards, so when a Citrix flow typed into
nothing there was no record of which text it had matched or where its click
landed. Setting the variable prints each match with its box, the point acted
on, every scroll and nudge, and the typed-value read-back counts. It never
prints the typed text or anything derived from it, since a value may be a
password.
