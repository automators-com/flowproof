---
title: "Secrets and configuration"
description: "Keeping secret values out of the trace, flowproof config, business data values, and flowproof doctor."
---

Traces are reviewable, diffable artifacts — so sensitive values must never
be written into them. Write `${VAR}` references in the spec instead:

```yaml
steps:
  - Type ${LOGIN_PASSWORD} into the password field
  - Press the login button
  - assert: page shows Welcome, ${LOGIN_USER}
```

The engine resolves references from the environment **at the moment of
use** — during recording and again on every replay. The trace (and every
artifact rendered from it) stores only the literal `${LOGIN_PASSWORD}`
reference. A reference to an unset variable fails closed: recording is
refused, and a replay step fails with an error naming the variable —
flowproof never types the literal reference into the app. Failure messages
mask live text whenever the expectation contained a reference.

This covers the *trace text*; the *pixels* of secret fields are covered by
the recording layer (`redact:` rules and always-on password-field masking,
see [docs/recording.md](recording.md)).

### `flowproof config`: credentials without hand-exporting env vars

`${VAR}` still resolves from the environment exactly as above — `flowproof
config` just gives SAP GUI and Fiori a place to put those variables once per
machine instead of re-exporting them in every shell. It writes a config file
under a per-user directory (`%APPDATA%\flowproof\` on Windows, `~/Library/
Application Support/flowproof/` on macOS, `$XDG_CONFIG_HOME/flowproof/` or
`~/.config/flowproof/` on Linux) and seeds any of its variables that aren't
already set before `record`/`run` resolve `${VAR}`s — an explicit shell
export, a CI secret, or a suite's own `env:`/`env_from` always wins over it.
Nothing is checked against a live system when you run it: SAP already gives a
specific error the first time a wrong value is actually used, so there is
nothing to duplicate here.

```console
$ flowproof config sap
SAP user: training-user
SAP password: ****************
SAP client (optional): 100
SAP language (optional): EN
SAP Logon connection name (optional): S/4HANA Development
wrote /Users/you/Library/Application Support/flowproof/config.yaml

$ flowproof config fiori
Fiori user: training-user
Fiori password: ****************
Fiori client (optional): 100
Fiori language (optional): EN
Fiori launchpad base URL (optional): https://my-launchpad.example.com/
wrote /Users/you/Library/Application Support/flowproof/config.yaml
```

`sap` and `fiori` are independent profiles, not one shared identity — a real
deployment's Fiori login can differ from its SAP GUI one (an SSO-fronted
launchpad in front of a password-auth backend is common), so `sap` seeds
`SAP_USER`/`SAP_PASSWORD`/`SAP_CLIENT`/`SAP_LANGUAGE`/`SAP_CONNECTION` and
`fiori` seeds its own `FIORI_USER`/`FIORI_PASSWORD`/`FIORI_CLIENT`/
`FIORI_LANGUAGE`/`FIORI_BASE_URL` — a Fiori flow's steps reference the
`FIORI_*` names, same as `examples/fiori/manage-info-records.flow.yaml`.
Re-running either command merges into what's already stored rather than
blanking fields you didn't mention — pressing Enter at a prompt keeps the
current value (the password prompt never shows what's currently stored, so
"leave blank to keep it" is the only way to change one field without
retyping the rest).

Flags exist for scripting a machine's setup instead of typing it by hand:
`--user`, `--password`, `--client`, `--language`, `--connection` (`sap`) or
`--base-url` (`fiori`). `flowproof config show` prints the file's path and
contents with the password masked; `flowproof config path` prints just the
resolved path. Design rationale, including what's deliberately out of scope
for now (multiple SAP systems on one machine, Windows file permissions),
lives in `plans/001-credential-config.md`.

`flowproof config skill` installs a packaged Agent Skill into the current
project — `.claude/skills/flowproof-config/` (Claude Code) and
`.agents/skills/flowproof-config/` (Codex CLI, GitHub Copilot, Cursor,
Gemini CLI, and the rest of that shared convention) by default — so a
coding agent working in your test-automation project can walk you through
`sap`/`fiori` itself instead of you reading this section by hand. `--claude`
or `--agents` narrows to just one, `--dir <path>` adds an arbitrary
skills-root for any other harness, and `--force` overwrites a target that
already exists with different content. The skill deliberately never handles
your password itself — see `plans/003-agent-config-skill.md`.

`flowproof config ai` stores the model key used for `--author llm`/`auto`
authoring the same way, instead of exporting `ANTHROPIC_API_KEY` or
`OPENAI_API_KEY` by hand every shell:

```console
$ flowproof config ai
AI provider:
  1) anthropic
  2) openai
Choose provider [default: anthropic]:
AI API key: ****************
wrote /Users/you/Library/Application Support/flowproof/config.yaml
```

Only `anthropic` and `openai` are accepted — a misspelled provider fails
before anything is written, rather than being stored and failing later at
record time. One neutral `ai.api_key` is stored; at runtime it seeds
`FLOWPROOF_AI_API_KEY` first, then the provider-specific compatibility
variable (`ANTHROPIC_API_KEY` or `OPENAI_API_KEY`) if that is still unset —
same fill-gaps-only precedence as `sap`/`fiori`. `--provider`, `--api-key`,
and the advanced-only `--model` script the same setup non-interactively;
`--clear-api-key` and `--clear-model` remove just that field from the
stored profile without touching the rest (there's no interactive path for
clearing — the flags are it). Pairing a clear flag with its own setter is
rejected outright (`--clear-api-key` with `--api-key`, `--clear-model` with
`--model`) rather than silently picking one. `flowproof config show` masks
`ai.api_key` the same way it masks SAP/Fiori passwords. Design rationale
lives in `plans/008-ai-authoring-config.md`.

### Business data values

Credentials belong in `flowproof config`; test-case business data belongs
beside the flow. When a flow references arbitrary non-secret values such as
`${MATERIAL}`, `${SUPPLIER}`, `${PLANT}`, or `${ORDER_ID}`, `record` and
`run` automatically load a sibling values file:

```text
display-info-record.flow.yaml
display-info-record.trace.jsonl
display-info-record.values.yaml
```

```yaml
MATERIAL: M-10092
SUPPLIER: "45000031"
PLANT: "1000"
```

The values file is resolved relative to the flow file, so
`flowproof run /path/to/display-info-record.flow.yaml` looks for
`/path/to/display-info-record.values.yaml` no matter where the command is
run from. Use `--vars <path>` to supply a different file, and repeat
`--var KEY=VALUE` for one-off overrides:

```console
$ flowproof run display-info-record.flow.yaml --vars qa.values.yaml
$ flowproof run display-info-record.flow.yaml --var MATERIAL=M-99999
```

### `flowproof doctor --sap` / `--fiori`: is any of this reachable?

Writing a config profile doesn't tell you it actually points at something
live. `flowproof doctor --sap` and `flowproof doctor --fiori` read whichever
of the above is already seeded into your environment and report what they
can reach — the SAP/Fiori equivalent of `flowproof doctor --agent` at the
model boundary (see [Wiring a real agent](agent-testing.md#wiring-a-real-agent-env-handles-and-the-record-upstream)).

```console
$ flowproof doctor --sap
SAP_CONNECTION=S/4HANA Development
attached to SAP GUI scripting.
connection 'S/4HANA Development': open
session logged in as TRAINING-USER

$ flowproof doctor --fiori
GET https://my-launchpad.example.com/?sap-client=100&sap-language=EN
reachable: HTTP 200 in 0.31s
attempting a real login as training-user - this submits a real credential to
a live system. Never run --fiori from CI or on a loop: a wrong password is a
real failed logon.
login succeeded: the shell loaded ("Home" is showing).
```

The two behave differently on purpose. `--sap` only ever OBSERVES — is SAP
Logon reachable, is the configured connection open, who (if anyone) is
logged in — and never submits a credential, because SAP already rejects a
bad one on its own and a doctor that kept retrying a stale password would
risk locking the account. `--fiori` checks reachability the same
observation-only way first, but Fiori has no equivalent non-UI login channel
to observe instead: if `fiori.user`/`fiori.password` are configured, it goes
further and attempts a REAL login through the launchpad's own form. Treat
`--fiori` as a manual, occasional check, never something wired into CI or run
on a schedule — that real login attempt is exactly the kind of repeated,
unattended credential submission that risks an account lockout. `--sap` is
Windows-only, like `app: sap` itself; `--fiori` runs anywhere `app: web`
does. `--timeout <seconds>` (default `60`) bounds how long `--fiori` waits on
that login attempt before giving up — `--sap` never waits on anything
external, so it ignores `--timeout` entirely. Design rationale lives in
`plans/002-sap-fiori-doctor.md`.
