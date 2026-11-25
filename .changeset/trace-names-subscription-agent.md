**A trace recorded on your Claude or ChatGPT subscription says so.** The
trace header's `agent.backend` knew only `anthropic` and `openai-compatible`,
so a recording made through `claude-code` or `codex` was labelled
`openai-compatible`, an API it never called. It is now `claude-code` or
`codex`, and the schema and `docs/trace-format.md` list both. An older
engine cannot read such a header; replay never uses it.
