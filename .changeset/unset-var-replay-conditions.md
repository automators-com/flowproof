**A `when:` or `repeat: until:` whose `${VAR}` is unset at replay now stops the run by name.**
Every other step fails closed on an unset variable, but a condition read the
failed check as "does not hold". A flow recorded where the variable was set
and replayed where it was not (CI, another machine, another environment)
skipped the `when:` block with its checks unrun and passed, and a `repeat:`
ran every pass against the app before reporting that its condition never
held. Replay now names the missing variable before reading the condition.
