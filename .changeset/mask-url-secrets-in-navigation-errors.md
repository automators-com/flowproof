**A secret in a flow's address no longer shows up in a navigation error.**
When a web flow's `url:` or a `Go to` step held a `${VAR}`, such as
`https://${API_USER}:${API_TOKEN}@host/` or `?token=${TOKEN}`, and the host
couldn't be reached, the `navigating to …` error quoted the resolved
address, secret included, on stdout, in result.json and junit.xml, and in
the record repair loop. Record and replay now write each such value back as
its `${NAME}`, on the first launch and on a mid-flow `Go to`, and so does a
multi-surface flow when it launches a surface.
