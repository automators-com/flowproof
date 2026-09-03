---
status: done
---
# Plan 8 — AI authoring config

Issue #541 names the immediate bug: `ANTHROPIC_API_KEY` can only be supplied
as an environment variable. That is awkward for the same reason SAP/Fiori
credentials were awkward before `flowproof config`: a user can configure their
live app credentials globally, but still has to remember a separate shell
export before model-assisted authoring works.

This plan adds a first-class AI authoring profile to the same global
per-machine config file.

## Problem

Flowproof already has two ways to reach an authoring model:

- Flowproof-native authoring code reads `FLOWPROOF_AI_PROVIDER`,
  `FLOWPROOF_AI_BASE_URL`, `FLOWPROOF_AI_API_KEY`, and
  `FLOWPROOF_AI_MODEL`.
- Compatibility paths also recognize provider-specific variables such as
  `ANTHROPIC_API_KEY` and `OPENAI_API_KEY`.

That split is useful: `FLOWPROOF_AI_*` gives Flowproof a provider-neutral
contract, while provider-specific variables let users reuse keys and tooling
they already have. The problem is that only environment variables can provide
those values today. `flowproof config sap` and `flowproof config fiori` solved
this for application credentials; AI authoring needs the same treatment.

For the first user-facing version, keep the provider choice deliberately simple:
Anthropic or OpenAI. Both have known public API endpoints, so ordinary users
should not be asked for a base URL.

## Decision

Add an explicit `flowproof config ai` command and an `ai:` block in the global
config file.

The normal setup should be small:

```yaml
sap:
  ...

fiori:
  ...

ai:
  provider: anthropic
  api_key: "..."
```

The config file stores the AI secret once, under the provider-neutral
`ai.api_key` field. It does not store duplicate `FLOWPROOF_AI_API_KEY` and
`ANTHROPIC_API_KEY` values.

At runtime, Flowproof seeds environment variables from that one stored value,
fill-gaps-only, following the same precedence rule as SAP/Fiori config:

```text
explicit shell env / CI secret / suite env wins
flowproof config fills only missing values
```

The primary seeded variables are Flowproof's neutral names:

| Config path   | Env var                 |
| ------------- | ----------------------- |
| `ai.provider` | `FLOWPROOF_AI_PROVIDER` |
| `ai.api_key`  | `FLOWPROOF_AI_API_KEY`  |
| `ai.model`    | `FLOWPROOF_AI_MODEL`    |

`ai.model` is an optional advanced field. It should not be part of the ordinary
setup prompt.

Flowproof should supply provider defaults internally instead of asking ordinary
users for endpoints:

| Provider    | Default endpoint              |
| ----------- | ----------------------------- |
| `anthropic` | `https://api.anthropic.com`   |
| `openai`    | `https://api.openai.com/v1`   |

Provider-specific variables are compatibility aliases, not the canonical
stored form:

| Condition             | Compatibility env var |
| --------------------- | --------------------- |
| `ai.provider: anthropic` | `ANTHROPIC_API_KEY` |
| `ai.provider: openai`    | `OPENAI_API_KEY`    |

These aliases are for compatibility only. The stored form remains one neutral
`ai.api_key` value.

## Provider validation

The provider must be a strict enum, not a free-form string. The first
implementation accepts only:

- `anthropic`
- `openai`

A misspelling such as `antropic` should fail before anything is written:

```text
invalid provider 'antropic'
expected one of: anthropic, openai
```

Flag mode should use clap-style enum validation. Interactive mode should use a
numbered choice/select prompt rather than asking the user to type the provider
name by hand.

## Base URL

Do not ask the user for `base_url` in the first implementation.

For now, the product supports two simple provider choices with known defaults:

- `anthropic` uses `https://api.anthropic.com`.
- `openai` uses `https://api.openai.com/v1`.

That is enough for the normal API-key setup path. Custom gateways, local vLLM,
LiteLLM, LM Studio, and corporate proxy endpoints can be handled later as an
advanced extension, but they should not be part of issue #541's first pass.

## Model

`model` is an advanced override, not a normal user prompt.

The model choice is risky because capabilities vary: vision, tool use,
structured output reliability, token budget, and reasoning behavior can all
affect whether assisted authoring works. A user can accidentally choose a model
that has an API key but lacks a capability a Flowproof feature needs.

Therefore, ordinary `flowproof config ai` should not ask for a model. Flowproof
should choose a known-good default internally. Advanced users may still override
it with:

```console
$ flowproof config ai --model claude-sonnet-5
```

or with the existing environment variable:

```console
$ FLOWPROOF_AI_MODEL=claude-sonnet-5 flowproof record flow.flow.yaml --author llm
```

Docs should warn that overriding the model can disable authoring capabilities.

## Backend mapping

The user-facing provider names should be `anthropic` and `openai`.

Internally, OpenAI still uses the existing OpenAI-compatible request shape, but
the user should not have to understand that. `provider: openai` means:

```text
provider = openai
base URL = https://api.openai.com/v1
request shape = OpenAI-compatible chat completions
```

So implementation should update the backend resolver instead of making the CLI
write a `base_url` just to satisfy the current `openai-compatible` code path.
A future custom-endpoint feature can reintroduce an explicit custom provider or
advanced base URL flag.

## Command surface

Add:

```console
$ flowproof config ai
$ flowproof config ai --provider anthropic
$ flowproof config ai --provider anthropic --api-key <key>
$ flowproof config ai --provider anthropic --model claude-sonnet-5
$ flowproof config ai --provider openai
$ flowproof config ai --provider openai --api-key <key>
$ flowproof config ai --clear-api-key
$ flowproof config ai --clear-model
```

Interactive mode should prompt for the ordinary setup only:

- provider, using a strict select prompt and defaulting to current value or
  `anthropic`;
- API key, using masked input and preserving the current key when the user
  presses Enter.

Interactive mode should not ask ordinary users for `model` or `base_url`.

Flag mode should merge like the existing `sap` and `fiori` commands: only flags
that are present overwrite existing fields. Clear flags should explicitly remove
fields from the stored profile.

Do not add AI prompts to `flowproof config sap` or `flowproof config fiori`.
Those commands configure application credentials. AI authoring credentials are
a separate concern and should be opt-in.

## Error messages

When model authoring is requested but no usable backend is configured, the
message should point at both supported setup paths:

```text
no usable model backend configured; run `flowproof config ai` or set
FLOWPROOF_AI_API_KEY / ANTHROPIC_API_KEY / OPENAI_API_KEY
```

For `provider: openai`, Flowproof should use the default OpenAI endpoint unless
a future advanced custom-endpoint feature is added.

For an unsupported or misspelled provider:

```text
invalid provider '<value>'
expected one of: anthropic, openai
```

## Doctor check

Add `flowproof doctor --ai` as a separate validation command.

`flowproof config ai` should write what the user gives it, just like SAP/Fiori
config does. It should not require a live model request before saving. The
separate doctor command can validate the configured backend with a tiny request
when the user wants confirmation.

`doctor --ai` should report:

- resolved provider;
- whether the API key is configured, without printing it;
- which default endpoint will be used;
- whether a tiny model call succeeds;
- the model identity Flowproof will use.

## Security rules

- `flowproof config show` must mask `ai.api_key`, the same way it masks
  SAP/Fiori passwords.
- `flowproof config path` remains safe because it prints only the file path.
- The config file permissions should stay owner-read/write on Unix.
- Explicit environment variables must continue to override config values.
- The stored key must never be written into traces, assist reports, generated
  specs, PR bodies, or diagnostics.
- Coding agents and docs must not ask users to paste API keys into chat.

## Implementation steps

1. Add an `AiProvider` enum with `anthropic` and `openai` values.
2. Add an `AiProfile` to `crates/flowproof-cli/src/config.rs`.
3. Add the `ai` field to `Config`, preserving backwards-compatible
   deserialization for config files that only have `sap` and `fiori`.
4. Extend `Config::env_pairs()` so it seeds `FLOWPROOF_AI_*` values and the
   provider-specific compatibility alias when appropriate.
5. Extend `Config::masked()` so `ai.api_key` is displayed as `********`.
6. Add `flowproof config ai` to the CLI subcommand enum, using strict provider
   validation.
7. Update the model backend resolver so `FLOWPROOF_AI_PROVIDER=openai` uses
   the OpenAI-compatible request path with default base URL
   `https://api.openai.com/v1`.
8. Implement `cmd_ai` with the same merge-on-rerun behavior as `cmd_sap` and
   `cmd_fiori`, plus explicit `--clear-*` flags.
9. Keep model as an advanced flag-only setting in the first implementation.
10. Do not prompt for `base_url` in the first implementation; use provider
    default endpoints.
11. Update model-authoring error messages to mention `flowproof config ai`.
12. Add `flowproof doctor --ai` for optional backend validation.
13. Update the embedded `flowproof-config` Agent Skill so coding agents know how
    to guide users through AI setup without asking for the API key in chat.
14. Update docs that currently tell users only to export `ANTHROPIC_API_KEY` or
    `OPENAI_API_KEY`.

## Tests

- Config serialization/deserialization with only `ai`, and with `sap`,
  `fiori`, and `ai` together.
- `config show` masks `ai.api_key`.
- `flowproof config ai --provider anthropic --api-key ...` writes and later
  merges without deleting omitted fields.
- `flowproof config ai --clear-api-key` and `--clear-model` remove only the
  requested fields.
- Invalid provider values fail before the config file is written.
- Interactive provider selection cannot write a misspelled provider.
- Interactive setup does not prompt for `model` or `base_url`.
- An unset `FLOWPROOF_AI_API_KEY` is filled from config.
- An already-set `FLOWPROOF_AI_API_KEY` wins over config.
- `ai.provider: anthropic` seeds `ANTHROPIC_API_KEY` when unset.
- An already-set `ANTHROPIC_API_KEY` wins over config.
- `ai.provider: openai` seeds `FLOWPROOF_AI_API_KEY`, `FLOWPROOF_AI_PROVIDER`,
  and `OPENAI_API_KEY` when unset.
- `FLOWPROOF_AI_PROVIDER=openai` uses `https://api.openai.com/v1` without a
  configured `FLOWPROOF_AI_BASE_URL`.
- `doctor --ai` succeeds against a working backend and fails clearly for missing
  key/provider errors.
- Existing SAP/Fiori config tests still pass unchanged.

## Resolved decisions

- Add a separate `flowproof config ai` command instead of extending
  `flowproof config sap` or `flowproof config fiori`.
- Store one neutral secret as `ai.api_key`; do not duplicate the same key on
  disk under provider-specific names.
- Seed `FLOWPROOF_AI_API_KEY` as the primary key for Flowproof.
- For `provider: anthropic`, also seed `ANTHROPIC_API_KEY` as a compatibility
  alias when it is unset.
- For `provider: openai`, also seed `OPENAI_API_KEY` as a compatibility alias
  when it is unset.
- Treat provider as a strict enum with only `anthropic` and `openai` at first.
- Default ordinary setup to Anthropic.
- Do not ask users for `base_url` in the first implementation.
- Default `anthropic` to `https://api.anthropic.com`.
- Default `openai` to `https://api.openai.com/v1`.
- Keep `model` as an advanced override, not an ordinary interactive prompt.
- Add explicit clear flags for removing stored AI fields.
- Add `flowproof doctor --ai` for optional live backend validation.

## Implementation notes

- `flowproof doctor --ai` uses the existing doctor flag style, matching
  `flowproof doctor --sap` and `flowproof doctor --fiori`, instead of adding a
  new positional `doctor ai` subcommand.
- The older `FLOWPROOF_AI_PROVIDER=openai-compatible` env spelling remains
  accepted for custom endpoints that also provide `FLOWPROOF_AI_BASE_URL`, but
  `flowproof config ai` writes only the new user-facing `openai` provider.

## Open questions

None for the first implementation.

## What landed

Implemented `flowproof config ai` with a strict `anthropic`/`openai` provider
choice, one stored `ai.api_key`, masked display in `config show`, merge-on-rerun
behavior, and explicit `--clear-api-key` / `--clear-model` flags. Config seeding
now fills `FLOWPROOF_AI_*` first and provider-specific compatibility aliases
(`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) only when those env vars are missing.

Updated the model backend resolver so `FLOWPROOF_AI_PROVIDER=openai` uses the
OpenAI-compatible request path with the default `https://api.openai.com/v1`
endpoint, while Anthropic keeps its existing default endpoint. The only
intentional compatibility divergence from the first-pass plan is that the older
`FLOWPROOF_AI_PROVIDER=openai-compatible` env spelling still works for custom
endpoints that provide `FLOWPROOF_AI_BASE_URL`; `flowproof config ai` does not
write that spelling.

Added `flowproof doctor --ai` for optional backend validation without printing
API keys. Updated the embedded `flowproof-config` skill and user docs to prefer
`flowproof config ai` over manual API-key exports.

Verified with focused config/doctor tests, CLI library tests, flowproof-agent
library tests, the embedded skill e2e tests, `cargo fmt --all --check`, and
`git diff --check`.
