**A captured value is no longer read for `${VAR}` references.** A value a
flow remembered from the app under test (`Remember … as`, an `assert_api`
`expect.capture`, or a downloaded file's name) was put into a step and the
result was then scanned for `${VAR}`. A page or API that returned
`${SOME_SECRET}` had that environment variable's value, secrets included,
typed into the app or sent in a later `assert_api` request, and captured
text that legitimately held `${…}`, such as `report-${date}.xlsx`, failed as
an unset secret. Typed text, `assert_api` requests, spreadsheet paths,
`exports:` and a surface's launch command now fill in captures and `${VAR}`
in one pass, on record and replay, so a captured value is used as written.
An agent block refuses a captured value holding a `${VAR}` reference rather
than passing it on.
