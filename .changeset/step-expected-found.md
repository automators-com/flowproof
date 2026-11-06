**A failed step now says what was expected and what was found, as data.** The
only account of a failure in `result.json` was the `detail` sentence, so a tool
that wanted to show "expected / found" side by side had to parse prose that
changes wording between releases. A failed check now also carries `expected`
and `found` on their own (`element text '8'` and `'Display is 9'`), and a step
whose target never appeared names the target it looked for and the closest
text on screen. Both follow `detail`'s masking: a `${VAR}` stays a reference
and its reading is `<masked>`. The fields are additive and absent when a
failure is not a comparison; database, API and spreadsheet checks keep their
reason in `detail` only for now.
