**`flowproof mcp install --command <path>` and `--remove`.** `--command`
writes the given engine path into the client's config in place of
`flowproof`, for an engine that is not on PATH, such as the one Flowproof
Desktop bundles. `--remove` takes the `flowproof` entry out again, keeping
every other server and setting, and leaves an entry it did not write alone
unless you pass `--force`.
