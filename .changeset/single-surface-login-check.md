- **A replay checks a single-surface flow's login user.** A multi-surface
  flow already stopped when a surface's `login.user` no longer matched the
  recording. A single-surface flow didn't check at all: change `login.user`
  from `clerk` to `approver` and `run` replayed a clerk's walkthrough as the
  approver, passing or failing on that user's authorisations with nothing to
  say the identity had changed. It now stops with "The login changed. Record
  this flow again." when the user differs, or when the `login:` block was
  removed. A new password needs no new recording, and a recording whose
  header names no user, made without a login or before 0.14, is not checked.
