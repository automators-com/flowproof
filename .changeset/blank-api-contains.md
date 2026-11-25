**An API check's `body_contains` or `header_contains` no longer passes on a blank `${VAR}`.**
A value that arrived empty left the check looking for blank text, which
every response contains, so only the status could fail the step: a 200 with
the wrong body passed. Recording and replay now fail the check, naming the
variable, as they already do for a blank `shows` or `url contains`.
