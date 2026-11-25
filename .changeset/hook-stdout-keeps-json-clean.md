**A suite hook that prints no longer breaks `--json`.** `before_each` and
`after_each` wrote to the same stdout as `flowproof run --json`'s report, so a
single `echo` in a hook put text in front of the JSON. The desktop app,
`flowproof mcp` and CI scripts then could not parse the run's result, and a run
that passed could be reported as an error. Hook output now goes to stderr,
where it is still shown, and stdout carries only the report.
