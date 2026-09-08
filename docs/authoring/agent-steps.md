---
title: "Agent flows and app sugar"
description: "The app: agent step grammar and shorthand sugar for common app configurations."
---

An `app: agent` flow tests an AI agent at the model boundary rather than a
UI, so it has its own small step vocabulary, documented in full in
[agent-testing.md](agent-testing.md). Unlike the forms above, these are
structured steps that either parse or error; they do NOT fall back to the
LLM author. The step forms:

| Step | Meaning |
|---|---|
| `prompt: <text>` | the task handed to the agent; several `prompt:` steps are joined into one turn |
| `assert_tool_call: <tool> [where <path> <matcher> <value> [and …]]` | a tool call the agent must make. Matchers: `equals` (alias `is`), `contains`, `matches` (regex), `exists`, `is absent` |
| `assert_no_tool_call: <tool> [where …]` | a tool the agent must NOT call anywhere in the trajectory |
| `assert: reply contains <text>` | the final assistant message contains `<text>` |

`agent:` (command/env), `tools:` (the boundary mocks), and `strict:` are
spec-level config, like `mock:` and `browser:` above.

## App sugar

Sugar is an alias layer, not a cage: on every UIA-driven app (`calc`,
`notepad`, and the `app:` mapping form) the full shared action grammar
applies too — `Press the "<label>" button`, `Click "<text>"`, `Type <text>
into the "<label>" field`, `Press Ctrl+S`, `id:` targets and ordinals all
act on any control the app shows, menus and dialogs included. Sugar wins
where it matches; everything else falls through to the shared forms.

- **calc**: `Type <digits>` (one press per digit), `Press
  plus|minus|times|divided by|equals`, `assert: display shows <number>`.
  Keys the sugar never named are shared-grammar presses: `Press the
  "Square root" button`, `Click "History"`.
- **notepad**: `Type <text>` types into the *document*; the targeted form
  `Type <text> into the "<label>" field` addresses a dialog's field (Find,
  Replace, Save As) instead. `assert: document contains <text>` (plus the
  shared grammar).
