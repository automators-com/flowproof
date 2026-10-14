- **The default models now match flowproof desktop.** With no model
  configured, the engine authored with `claude-sonnet-5` or `gpt-5`, while
  flowproof desktop had moved its defaults to `claude-opus-5-5` and
  `gpt-6.1-sol`. The same provider and key gave a different model depending
  on whether the CLI or the app set it up, and the trace header recorded
  whichever one it was. The engine's defaults are now `claude-opus-5-5` for
  Anthropic and `gpt-6.1-sol` for OpenAI, the same as the app's.
