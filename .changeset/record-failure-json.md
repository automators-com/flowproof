**`record --json` says where a recording failed and why.** A failed recording
printed JSON only when a step needed clarifying. Every other failure left a
single line on stderr, with no step and none of the app's own words, so a
caller had to scrape it. Now every failure prints one object. `error` is the
message, and `failure` gives its `kind`, the `step` that was running (number,
the `sNNNN` id the progress line printed, and its intent), what the app was
showing as `screen`, and a `hint` saying what to try. On SAP, `screen` names
the window title, the transaction, the program and screen number, the status
bar message with its severity, and any popup open over the screen. A step SAP
refused with an error on its status bar is reported as `rejected_by_app`, and
the hint quotes SAP's message. Failures before recording starts (the flow does
not load, or the driver cannot start) use the same shape.
