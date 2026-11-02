**A step with `: ` in its text now says to quote it.** YAML reads
`- Calculate 5+3 using the buttons: click 5, then +` as a key and a value, so
flowproof rejected it as an unknown step key and followed that with every
step form it knows. That was a paragraph of syntax for a typo, and none of it
named the fix. No step keyword has a space in it, so a key that does is now
reported as step text YAML split at its colon: the message quotes the start
of the step and says to put the whole step in quotes. A misspelled keyword
such as `assert_apy:` still lists the recognized forms.
