**Your Claude or ChatGPT subscription can drive authoring instead of an API key (experimental).**
Recording, repair, heal, and drafting from a video, a document or a codebase
all needed an Anthropic or OpenAI API key, so someone who already pays for
Claude or ChatGPT had to set up and pay for API access as well.
`flowproof config ai --provider claude-code` (or `codex`) now uses your own
Claude Code or Codex, through its ACP adapter and under the login it already
has, as the model. flowproof never touches that login: the agent runs in an
empty folder, sees none of flowproof's secrets or API keys, and is refused
any tool, file or terminal it asks for. `flowproof doctor --ai` shows whether
it is installed and signed in. Replay is unchanged and still calls no model.
