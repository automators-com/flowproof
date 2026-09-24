**`author-from-doc` can be driven by a tool.** `--check` reads the PDF and
reports how many steps it holds without a model call, so a caller can reject
an unreadable export before spending anything. `--json` reports the written
files and every drafted step with its kind, so a flagged or out-of-scope step
can be shown to a person without reading the `# TODO` comments back. Progress
(`drafting step N of M`) now goes to stderr. It also now sees a model saved
with `flowproof config ai`; before, only environment variables worked.
