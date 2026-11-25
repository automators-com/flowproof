---
status: ready
---
# Plan 17 - flowproof as an MCP server

Date: 2026-11-24
Product: flowproof engine (primary), FlowProof Desktop (follow-up)
Repos: `automators-com/flowproof` (engine), `automators-com/flowproof-desktop`
Cross-repo copy: `flowproof-desktop/plans/flowproof-mcp.md`

## Where this came from

The ACP work — engine PRs [#853](https://github.com/automators-com/flowproof/pull/853),
[#854](https://github.com/automators-com/flowproof/pull/854),
[#855](https://github.com/automators-com/flowproof/pull/855),
[#857](https://github.com/automators-com/flowproof/pull/857) and desktop
[#384](https://github.com/automators-com/flowproof-desktop/pull/384) /
[#385](https://github.com/automators-com/flowproof-desktop/pull/385) — let someone
point flowproof's authoring model at their own Claude Code or Codex account
instead of pasting an API key: `claude-code` and `codex` join `anthropic` and
`openai` as providers, routed to an ACP-speaking model client. Those citations
are to the unmerged branch, not `main` — `crates/flowproof-cli/src/config.rs:112-138`
and `crates/flowproof-agent/src/llm.rs:508-540` as the branch has them, with the
whole `crates/flowproof-agent/src/acp/` module new. That is flowproof
*consuming* a coding agent.

This plan is the other direction: a coding agent driving flowproof. The team
asked whether that should be MCP or a Claude Code plugin. The answer below is
that it is one server and the plugin is a packaging step on top of it, not a
second route — and that the work is almost entirely in the engine repo.

## What already exists, and why this is not a greenfield build

Three findings change the shape of this plan, all verified in the engine
checkout:

**1. A flowproof MCP server already ships.** `sdk/python/flowproof/_archived_mcp_server.py` (renamed in #877)
exposes `flowproof_record`, `flowproof_run`, `flowproof_get_trace` and
`flowproof_heal` over stdio via FastMCP (`_archived_mcp_server.py:42-111`), installed as
the `flowproof-mcp` console script behind an extra
(`sdk/python/pyproject.toml:24,28`). `internal/design.md:56-58` lists it as
shipped. So this plan is a **replacement of a Python-only surface with a native
one**, not a first implementation — and per `CHARTER.md:104-105` ("No public API
break without a human") retiring the Python entry point is a human decision, not
something the work can do on its way past.

It is also effectively undiscoverable: it is mentioned in exactly two places,
`README.md:302` and `sdk/python/README.md:15`, with no page under `docs/`, while
`docs/self-help.md:9,67` already writes as though an MCP caller is the normal
driver.

**Nothing of ours uses it.** Audited 2026-11-24: no reference anywhere in the
desktop app; none in DataMaker, which uses flowproof the other way round —
flowproof's `app: agent` replay tests DataMaker's agent (`datamaker/CLAUDE.md:86`,
`datamaker/contributing.md:102-103`), so `internal/design.md:56-62` calling MCP
"the integration path by which DataMaker's agent drives flowproof" describes an
intention, not a dependency. Inside the engine the only consumer is its own test,
`sdk/python/tests/test_mcp.py`, which does run in CI (`ci.yml:350` runs
`uv run --group dev pytest`, and the dev group carries `mcp`,
`sdk/python/pyproject.toml:35-40`). Confirmed with the user that no external user
has installed it either. So the Python server is being **archived by rename**
ahead of this work rather than kept as a second surface — see "Resolved
questions" #5.

**2. "MCP" already means something else in this codebase.** flowproof records
an agent's MCP traffic: a flow's `mcp:` block, a hidden `flowproof mcp-stdio`
stand-in the agent spawns as its server command
(`crates/flowproof-cli/src/lib.rs:495-507`), and the JSON-RPC machinery behind
it in `crates/flowproof-adapters/src/mcp_stdio.rs` /
`mcp_core.rs`. That is flowproof as a *proxy between someone else's agent and
their MCP server*. Any new subcommand, doc page and help string has to be
written so the two are not confused.

**3. The protocol plumbing is half-built.** `mcp_stdio.rs:1-35` is already a
newline-delimited JSON-RPC 2.0 stdio implementation with no async runtime ("std
threads, plain pipes, one blocking read loop per direction"), and `mcp_core.rs`
already has the envelope builders (`result_envelope:219`, `error_envelope:226`,
`notification_envelope:237`) and the `tools/call` content shape
(`mock_tool_result:206`). They are `pub(crate)` to `flowproof-adapters`.

## Outcome

Someone with a coding agent — Claude Code, Codex CLI, Cursor, VS Code — runs one
command in their test repo and from then on their agent can record, replay, heal
and audit flows directly, with no API key of flowproof's own and no Python
install:

```bash
flowproof mcp install          # registers the server with the agents it finds
flowproof mcp                  # the server itself, stdio; clients spawn this
```

The server ships inside the same single binary npm, PyPI and `cargo install`
already deliver, so "install flowproof" and "install the MCP server" stop being
two different acts. Desktop users get the same thing from Settings later, pointed
at the engine the app already bundles.

## MCP or a plugin?

Both, in that order, and the plugin is thin.

- **MCP is the product surface.** It is the only one that reaches every client
  the team cares about, and it is what `CHARTER.md:66-68` already names as a
  first-class rendering of the engine.
- **A Claude Code plugin is a distribution wrapper** around that server plus
  skills. It is Claude-Code-only, so building it first would serve one client
  and leave Codex, Cursor and VS Code users with nothing.
- **The "less technical" audience is already served by two other things**: the
  desktop app, and the `flowproof-config` Agent Skill that
  `flowproof config skill` writes into `.claude/skills/` and `.agents/skills/`
  (`crates/flowproof-cli/src/config.rs:718-783`, designed in the engine's
  `plans/003-agent-config-skill.md`). Skills are prose; MCP is capability. A
  skill telling an agent to shell out to `flowproof run` and a tool that runs it
  are not competitors — the skill is how the agent knows *when*, the tool is
  *how*.

So: server first, skills alongside it, plugin last as packaging. The plugin step
is deliberately left at the end of this plan and its manifest shape must be
checked against Claude Code's current plugin docs when it is reached — the
engine's plan 003 records getting exactly this class of assumption wrong on its
first draft about where agents look for skills.

## Where the work goes

**Engine, almost entirely.** The server is a subcommand of the engine binary;
the tools are the engine's operations; the install writes files in the user's own
repo. Nothing about it needs the desktop app.

**Desktop, later and small.** One Settings action that runs `flowproof mcp
install` against the bundled engine at
`<app executable dir>/flowproof-engine` (`flowproof-desktop/src-tauri/src/settings.rs:50-69`), one
new line in `flowproof-desktop/src-tauri/src/engine_contract.rs:44-134` so the bundled engine is
proven to accept that command line, a changeset, and a row on the security page.
That belongs in its own plan and its own branch once the engine side has
shipped.

Still behind a button rather than automatic on first launch — not because it
guards anything (decision 9: there is nothing to guard), but because writing into
someone's repo or home config unasked is the kind of thing an app should ask
about. What the button is genuinely worth is the two facts it knows and a
hand-written config would get wrong: the absolute path of the bundled engine, and
which folder the person's flows are in. So it should register the server
**project-scoped, into the current working folder**, which also means "which
workspace" is answered by which folder the agent is opened in — no switching
needed. One wrinkle for that plan to settle: a project-scoped config file written
into a working folder is a file in the person's repo, and this app shares working
folders through GitHub (`flowproof-desktop/src-tauri/src/github.rs`), so an absolute
`/Applications/flowproof.app/…` path committed by a Mac user breaks for a Windows
teammate. Recommendation to carry into that plan: write the bare `flowproof`
command for anything that may be committed and keep the absolute bundled path for
user-scoped registrations only.

## Design decisions

### 1. The server supervises the CLI; it does not re-implement it

`flowproof mcp` answers a `tools/call` by spawning its own executable with the
matching `--json` command line and parsing the one JSON object that comes back.
It does not call `flowproof-agent` / `flowproof-replay` in-process.

Why this way:

- **stdout purity becomes structural rather than a rule to remember.** A stdio
  MCP server owns stdout absolutely. The engine has 79 stdout writes, and all of
  them are in the CLI's rendering layer (`crates/flowproof-cli/src/lib.rs`); the
  library crates have none outside tests, printing only to stderr
  (`crates/flowproof-replay/src/lib.rs:954`,
  `crates/flowproof-agent/src/recorder.rs:3391,4886`), and the update check
  already routes to stderr for exactly this reason
  (`crates/flowproof-cli/src/update_check.rs:6`). In-process, one future
  `println!` in a library crate silently corrupts every session. Out-of-process,
  the child's stdout is a pipe the server reads.
- **Progress and cancellation come for free.** The child's stderr lines are the
  recorder's own progress output, which is what the desktop already streams as
  `engine:line`; they map straight onto `notifications/progress`. There is no
  observer or callback in `flowproof-agent`'s public API to get them in-process
  (`crates/flowproof-agent/src/lib.rs:23-92`). Likewise `notifications/cancelled`
  becomes "terminate the child", a path the desktop already relies on and tests
  (`flowproof-desktop/src-tauri/src/engine.rs:669-701` and its test at `:821`).
- **The tool surface tracks the CLI automatically** — `--vars`, `--var`,
  `--from/--until`, `--retries`, directory-as-suite, headed/headless — instead of
  being a third hand-maintained copy.
- **Self-exec is already house style**: `agent_flow.rs:287-305` resolves
  `current_exe()` for `mcp-stdio` with a `FLOWPROOF_MCP_EXE` override for
  wrappers and tests. Reuse that, override name included.

What this costs, stated plainly for review: the server is then a thin rendering
over the CLI, which is a thin rendering over the library — one hop more than
`CHARTER.md:66-68` says in its letter. Process startup is immaterial next to a
browser launch. The genuine in-process argument is a different problem worth its
own plan: the structured-result orchestration (load spec → apply suite context →
skip gate → pick driver → serialize) exists **twice** today, in
`crates/flowproof-python/src/lib.rs:83-240` and again in
`crates/flowproof-cli/src/lib.rs`, and lifting it into one `flowproof-api` seam
is the refactor invariant 7 actually wants. This plan does not block on it and
does not add a third copy.

### 2. Hand-roll the protocol, do not take `rmcp`

The surface is five tools over one transport. `mcp_core.rs:206-240` already has
the envelopes; what is missing is `initialize` with capability negotiation,
`tools/list`, `tools/call` dispatch, JSON-RPC error codes and the progress and
cancelled notifications.

The official Rust SDK would add an async runtime boundary and a schema-derive
dependency into a binary whose own code is entirely synchronous, plus new
licence and advisory surface for `deny.toml` and `osv-scanner.toml`. (To be
accurate about the usual objection: tokio is *already* in `Cargo.lock` via
`postgres`, because `flowproof-agent` and `flowproof-replay` enable
`flowproof-driver`'s `oob` feature — `crates/flowproof-agent/Cargo.toml:16`,
`crates/flowproof-replay/Cargo.toml:13`. So it would not be a new runtime in the
tree, but it would be the only async code the project itself writes, against a
codebase that has consistently chosen `ureq` and `tiny_http` over async
frameworks.)

Pin and advertise one protocol revision, assert it in a test, and treat a bump
as its own small PR. **What would flip this decision:** needing resources,
prompts, elicitation, or a second (streamable HTTP) transport. At that point the
SDK is the cheaper answer and this choice should be revisited rather than
defended.

### 3. Five tools, and deliberately not more

| Tool               | Runs                                        | Notes                                                                                                                                                                        |
| ------------------ | ------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `flowproof_record` | `record --json`                             | `--var/--vars`, `--author`, `--reuse`, `--resume`, `--no-repair`, `--headless`. Returns the clarification payload as data, as the Python server does (`_archived_mcp_server.py:43-66`) |
| `flowproof_run`    | `run --json`                                | `--var/--vars`, `--from/--until`, `--retries`, `--headless`; a directory is a suite. A failing test is a result, never a tool error                                          |
| `flowproof_heal`   | `heal --json`                               | `--apply` default false, `--apply-proposal`, `--from-run`. Invariant 4: the proposal is the output                                                                           |
| `flowproof_doctor` | `doctor --ai --json`, `doctor --sap --json` | `--ai` and `--sap` only                                                                                                                                                      |
| `flowproof_audit`  | `audit --json`                              | The control-coverage report, so an agent can answer "prove it"                                                                                                               |

Every tool also takes an optional `cwd` and an optional `allow_exec` — see
decisions 4 and 8. `flowproof_run` accepts a directory as a suite, with the
likely duration named in its description so a client can set its timeout
accordingly.

**No file tools.** No `list_flows`, no `read_flow`, no `write_flow`, and — a
departure from the Python server — no `get_trace`. Every MCP client in scope is a
coding agent that already has better file tools than flowproof would ship, and a
trace is a documented JSONL file (`docs/trace-format.md`) sitting next to the
spec. The server exposes only what flowproof alone can do.

**No config tools.** Secrets must never travel through a tool result into a
client's model context, and `flowproof config` is how they are set. Configuration
stays with the human, walked through by the `flowproof-config` skill. The server
should say so in its `tools/list` descriptions so an agent stops rather than
improvises.

**Not yet:** `doctor --fiori` and `doctor --agent`, `export`, and the three
`author-from-*` drafting commands. `--fiori` performs a real login against a live
system and its own help text says it must never run on a repeated trigger
(`crates/flowproof-cli/src/lib.rs:573-578`); `--agent` runs a command the caller
supplies. Both are refusals with a reason, not omissions. The drafting commands
are experimental and need an authoring model; they are a good v2 once the server
is proven.

### 4. Exec consent is not inherited from the Python server

This is the decision that most needs getting right, and the existing MCP server
gets it wrong.

`flowproof_record` in the Python server reaches `_native.record`, which calls
`flowproof_cli::allow_suite_exec()` unconditionally
(`crates/flowproof-python/src/lib.rs:100`) on the stated grounds that "an
embedder drives its own, trusted suite". That one call sets a process-wide flag
(`crates/flowproof-cli/src/lib.rs:2566-2575`) which is the *only* gate on three
different execution surfaces: a `suite.yaml`'s `env_from` / `before_each` /
`after_each` shell (`guard_suite_exec:2615`), a spec-authored `agent.command` or
`mcp.*.command` (`guard_spec_exec:2641`), and a Windows flow's `app:` launch
command (`guard_launch_exec:2655`).

A coding agent pointed at a repository it cloned a minute ago is not an embedder
driving its own trusted suite. So:

- **The native server never calls `allow_suite_exec()`.** The default is the
  CLI's default: refuse, with the message that names the command and the file it
  came from (`suite_exec_refusal:2601`). That refusal text reaching the agent as
  tool-result data is the right outcome — the agent can show the human exactly
  what wants to run.
- **Consent is explicit and per call**: an `allow_exec` tool argument, off by
  default, that adds `--allow-exec` to the child command line. An agent that
  passes it has to have decided to.
- **`flowproof mcp` grows no blanket "trust this folder" flag in v1.** If one is
  ever wanted it belongs on the install step, written into the client's config
  where a human can see it, not inferred at call time.

Related, and to surface rather than fix here: the desktop security page says "Without
the app, the engine refuses to run these unless you pass `--allow-exec`"
(`flowproof-desktop/docs/security.md:48`). That is not true of
`flowproof[mcp]` or the Python API today, for the reason above. Per the desktop `AGENTS.md`
this plan proposes wording and asks before editing that page — see "Security
docs" below.

### 5. What reaches the client's model

Worth writing down because it is the question an enterprise reviewer will ask,
and the answer is mostly reassuring.

Secrets are safe by construction: a trace stores `${VAR}` references and
resolves them only at the moment of use
(`crates/flowproof-trace/src/secret.rs:1-13`), so nothing a tool returns about a
trace can carry a value. What *does* flow outward is live-screen material in two
places — a `needs_clarification` scene inventory (the whole point of
`docs/self-help.md`) and a failure's `FailureScreen` (`lib.rs:829-848`) — and it
goes to whichever model the MCP client is using. That is the client's provider,
not flowproof's, and flowproof cannot see or choose it.

The rule to state in the docs: recording through an MCP client sends screen
content to that client's model, exactly as recording with an API key sends it to
the configured provider. Use test accounts and test data. No new mechanism, one
new destination to be honest about.

### 6. Naming

`flowproof mcp` (serve) and `flowproof mcp install`, alongside the existing
hidden `flowproof mcp-stdio`. The adjacency is unfortunate and renaming
`mcp-stdio` is not on the table: it is a documented env-var contract
(`FLOWPROOF_MCP_SERVER_<NAME>` → `"<exe>" mcp-stdio --server <name>`,
`agent_flow.rs:301-305`) and a public-surface change needs a human
(`CHARTER.md:104-105`). Mitigate in prose instead: one disambiguating line in
each help text, and a docs page that opens by distinguishing "flowproof as your
agent's tool server" from "flowproof standing in for your agent's tool server".

### 7. Install mirrors `flowproof config skill`

Same shape, same ergonomics, same embedded-content approach as
`config.rs:718-783`: write unless the file is already byte-identical, `--force`
to overwrite a different one, write every convention it finds by default, and a
`--print` that emits the snippet for a client it does not know.

Each client's config path and key must be verified against that client's current
docs when the step is implemented, not taken from this plan — they drift, the
shapes genuinely differ (the `mcpServers` object several clients share is not the
key VS Code uses), and plan 003's own text records this exact assumption being
wrong first time round.

### 8. Which folder a call runs in: a per-call `cwd`, not a workspace to switch

The server inherits the client's working directory, and every tool takes an
optional `cwd` that overrides it **for that call only**. There is no
`flowproof_set_workspace` and no session state.

This started as "inherit the cwd" plus a worry about what happens when someone
wants to work in a different folder, and a stateful switching tool as the answer.
The per-call argument answers the same need better, and three things in the code
say why the stateful version would be a mistake:

- **The workspace is an app concept, and the engine deliberately has none.** The
  desktop app does have a working folder and a switcher for it — the sidebar
  dropdown of recent folders plus "Choose a folder"
  (`flowproof-desktop/src/components/Sidebar.tsx:53-61`), persisted as `workspace` /
  `recent_workspaces` in the app's own settings
  (`flowproof-desktop/src-tauri/src/settings.rs:19-20`), with a per-workspace sidecar
  (`flowproof-desktop/src-tauri/src/paths.rs:15-18`) and GitHub sign-in keyed by folder path
  (`flowproof-desktop/src-tauri/src/github.rs:221`). That is app-only state, and the engine never
  reads any of it — by design, per the desktop `AGENTS.md`'s rule that the engine owns the
  data and app-only state lives in sidecars. In the engine, suite context,
  `${VAR}` seeding and `env_from` are discovered by walking up from *the spec's
  own path* — `apply_suite_context(spec_path)` →
  `SuiteManifest::discover(spec_path)` (`crates/flowproof-cli/src/lib.rs:2744-2752`).
  So the thing a switcher would switch does not exist on the side of the product
  this server lives in, and a path argument already selects it. Putting one in
  the engine would mean the engine growing persistent per-user state it has
  never had, to mirror a concept that belongs to the app.
- **cwd is load-bearing for a security control, not cosmetic.** For `app: agent`
  flows the workspace root that `assert_no_side_effects` scans *is* the process
  cwd (`crates/flowproof-cli/src/agent_flow.rs:2298-2301`, feeding
  `check_side_effects(…, workspace)` at `:1339-1344`). A tool that lets the agent
  move that root between calls lets the agent change what a control is judging.
  Per call, it is an input visible in the request — and the result must echo the
  root that was actually judged, so a reviewer can see it.
- **It costs nothing, because of decision 1.** Each call is already a child
  process, so `cwd` is one field on the child's `Command`. The stateful version
  would mean `set_current_dir` on the server process, which is process-global and
  races the concurrent tool calls clients are free to make.

So: cwd is inherited by default (which is also the trust signal — the human
started their agent in that folder), overridable per call, never remembered.

**The app's switcher does raise a real coherence problem**, which is the thing to
design for instead. Once the desktop registers the server, "the current folder"
has two owners: the app's selected working folder and the folder the agent was
started in. They can disagree, and nothing would tell the person which one a
tool call used. Two consequences, both for the desktop follow-up plan rather than
the engine: the registration should be **project-scoped into the selected working
folder**, so switching folders in the app means a different folder with its own
registration and there is nothing to keep in sync; and switching to a folder that
has no registration is the moment for the app to offer one, rather than leaving
the person to notice their agent cannot see the flows they are looking at. The
engine side needs none of this — the agent passes a path, and
`flowproof_doctor` reports the folder it resolved (step 6) so a disagreement is
visible rather than silent.

### 9. There is no auth, and nothing to bypass

Worth settling because "will the desktop registering the server bypass auth?" has
a short answer: there is no auth to bypass.

A stdio MCP server is a child process. It runs as the user who started the
client, with that user's file permissions, and it has no network listener for
anyone else to reach. MCP's auth story (OAuth 2.1) is a remote-HTTP-transport
concern and does not apply. Adding a token here would be theatre: whoever could
present it already has the ability to run `flowproof` directly.

What the security boundary actually is, in order: the OS user; the exec-consent
gate in decision 4; and the paths the client passes. That is the whole list.

**The thing that genuinely looks like auth is the authoring model**, and it is
worth separating out because it is the one case where a tool call can fail for a
credential reason. `flowproof_run` needs nothing — replay makes zero model calls
(`CHARTER.md:49-55`). `flowproof_record` wants an authoring model, and gets it
from the engine's own `config.yaml` (`flowproof config ai`, including the
`claude-code` / `codex` subscription providers from #855) — not from the MCP
client, because sampling is not available (see Out of scope). With nothing
configured it does not hard-fail: plain steps fall back to deterministic grammar
with a visible warning (`crates/flowproof-cli/src/lib.rs:1055-1060`,
`:4538-4543`), and only a fallback that cannot author says so plainly (`:2869`).

That failure message is the handoff: the tool result tells the agent no authoring
model is configured, and the `flowproof-config` skill is what tells the agent how
to walk the human through fixing it. Tool returns the problem, skill supplies the
procedure, human supplies the secret. No credential ever passes through a tool
argument or a tool result.

**Activation** is therefore just: an entry in the client's config pointing at the
binary, and a client restart. `flowproof mcp install` writes the entry;
`--print` emits it for a client it does not know. No accounts, no sign-in, no
activation state for anything to get out of sync with.

## Implementation plan

Engine PRs, each small enough for `scripts/gate/ratchets.sh` to accept it
(`CLAUDE.md:99` refuses over ~400 changed lines), each with the tests
that keep it fixed:

1. **File the plan in the engine repo.** This file. The cross-repo view, with
   the desktop follow-up, lives in `flowproof-desktop/plans/flowproof-mcp.md`
   ([flowproof-desktop#393](https://github.com/automators-com/flowproof-desktop/pull/393)).
2. **Widen the envelope helpers.** Make `result_envelope`, `error_envelope` and
   `notification_envelope` reachable from the new module (`pub` within
   `flowproof-adapters`, or put the server in that crate). Mechanical.
3. **The server, with one tool.** `flowproof mcp`: `initialize` with an
   advertised protocol revision and `tools` capability, `tools/list`,
   `tools/call` dispatch, JSON-RPC errors, and `flowproof_run` over
   `run --json` — including the per-call `cwd` (decision 8), set on the child
   and echoed in the result, with a test that it never mutates the server's own
   working directory. An integration test that drives the real binary over stdio,
   modelled on `crates/flowproof-cli/tests/mcp_stdio_e2e.rs` (637 lines of
   precedent for exactly this kind of test) and on the Python server's
   `sdk/python/tests/test_mcp.py`. Docs page in the same PR — `documented_flags.rs`
   fails the build if a user-facing flag is absent from `docs/**/*.md` or
   `README.md` (`tests/documented_flags.rs:1-14`).
4. **`flowproof_record` and the clarification path.** Including the test that a
   `needs_clarification` result is tool-result data, not a JSON-RPC error.
5. **Exec consent.** The `allow_exec` argument, defaulting off, with a test that
   a suite-hook flow recorded through MCP is *refused* by default and that the
   refusal names the command. This PR is the one to review hardest.
6. **`flowproof_heal`, `flowproof_doctor`, `flowproof_audit`.** Plus the test
   that `--fiori` and `--agent` are refused with a reason. `flowproof_doctor`
   reports the working directory it resolved alongside the model-config verdict,
   so an agent that is looking in the wrong folder, or whose record just fell
   back to deterministic grammar, can see both reasons without a file tool.
7. **Progress and cancellation.** Child stderr lines as
   `notifications/progress` when the client supplied a progress token;
   `notifications/cancelled` terminates the child. A test that a cancelled
   record leaves no orphaned process.
8. **`flowproof mcp install`.** Per-client writers, `--print`, `--force`, and
   the write-unless-identical behaviour. Tests against temp directories.
9. **Docs and positioning.** A `docs/` page for the server, `README.md:302`
   updated to lead with the native command, `docs/self-help.md` pointed at it,
   `internal/design.md:56-58` corrected (it describes the Python server as *the*
   MCP surface), and a changeset in the repo's voice. Decide with a human what
   happens to `flowproof-mcp` and the `mcp` extra: this plan recommends keeping
   both working, documenting them as the Python-embedder path, and not
   deprecating anything in the same release.
10. **A flowproof skill.** `flowproof config skill` installs a config skill; the
    equivalent for the MCP tools — when to record versus heal, that a failing
    run is data, that `--apply` needs a human — is what makes an agent use the
    server well. Possibly an extension of the existing skill rather than a new
    one.
11. **The Claude Code plugin.** Last. Bundles the server registration and the
    skills, manifest shape verified against current docs at that point.

Then, separately, in this repo: the desktop Settings action, its
`engine_contract.rs` line, its changeset and its security-page row — its own
plan, its own branch.

## Security docs

Engine: `docs/threat-model.md` gains the MCP server as a trust boundary — who
spawns it, what consent it does and does not carry, and what crosses to the
client's model.

Desktop: `docs/security.md:48` currently reads "Without the app, the engine
refuses to run these unless you pass `--allow-exec`." That is accurate for the
CLI and inaccurate for the Python API today. **Approved wording (user, 2026-11-24
— no need to ask again):**

> Without the app, the engine refuses to run these unless you pass
> `--allow-exec`. A coding agent driving Flowproof through its MCP server gets
> the same refusal, and has to ask you before it can pass it.

**It goes in with implementation step 5, not before.** The sentence describes a
native MCP server that refuses by default; until step 5 lands there is no such
server, so publishing it early would replace one inaccurate claim with a
different one. Archiving the Python server (resolved question #5) narrows the
current gap to the Python API alone but does not close it — `_native.record`
still calls `allow_suite_exec()` — so the existing sentence stays as the lesser
inaccuracy in the meantime.

No row in the fixed-issues log: with the audit showing no users of the Python MCP
server, its exec-consent behaviour is a documented-embedder choice that nobody
was exposed to, not a shipped defect. Revisit only if that audit turns out to be
wrong.

## Out of scope

- **A streamable HTTP transport.** stdio covers every client in scope and avoids
  a listening socket on a tester's machine.
- **`flowproof-api`, the shared structured-result crate.** The right fix for the
  `flowproof-python` / `flowproof-cli` duplication, and a prerequisite for
  nothing here.
- **Deleting `flowproof-mcp` / the `mcp` extra.** Decided to archive by rename
  first, not delete — resolved question #5. The deletion itself stays a separate,
  later human decision (`CHARTER.md:104-105`), tracked against engine issue
  [#378](https://github.com/automators-com/flowproof/issues/378).
- **MCP sampling as the authoring model.** `sampling/createMessage` would let the
  server borrow the client's model and need no key at all — architecturally the
  right answer to "where does the authoring model come from", and the natural
  successor to the ACP work. Client support is too thin to build on today. Worth
  re-checking each time this plan is amended; until then the ACP providers from
  #853-855 are the no-API-key path.
- **The desktop Settings action.** Named above, deferred to its own plan.

## Resolved questions

Settled 2026-11-24. Kept as a record of what was decided and why, since the
reasons are what a later amendment needs.

1. **Working directory: inherit the client's, or take an argument?** Inherit, and
   take an optional per-call override. A stateful workspace-switching tool was
   considered and rejected — see decision 8 for the three reasons (the workspace
   is an app concept the engine deliberately has no state for, and the spec path
   already selects it; cwd is an input to the `assert_no_side_effects` control;
   a per-call argument is free and race-free in the supervisor design, a mutable
   process cwd is neither). The app's own folder switcher is real, and what it
   argues for is in the same decision: a project-scoped registration per folder,
   so there is no second "current folder" to keep in sync.
2. **Should `flowproof_run` accept a directory as a suite in v1?** Yes, with the
   likely duration named in the tool description so a client can set its timeout.
3. **Five tools, or one tool with an `operation` argument?** Five, keeping the
   `flowproof_*` prefix so the names survive next to a dozen other servers.
4. **Does the desktop register the server automatically on first launch?** No,
   behind a button — but not for access-control reasons: there is no auth to
   bypass, and none that would mean anything (decision 9). The button earns its
   place by knowing the bundled engine's absolute path and the person's working
   folder. The one credential-shaped failure, "no authoring model configured",
   is a tool result that hands off to the `flowproof-config` skill.
5. **What happens to the Python MCP server?** Archived by rename in
   [#877](https://github.com/automators-com/flowproof/pull/877)
   (`flowproof/mcp_server.py` → `flowproof/_archived_mcp_server.py`, console
   script and test following it), on the usage audit above: nothing of ours uses
   it and no external user has installed it. The `flowproof-mcp` command and the
   `mcp` extra keep working and the test keeps running, so this is a marker and
   not a removal — reversible with one `git mv`, and it changes nothing a user
   can observe. Whether it is deleted outright is a later call, once the native
   server has shipped and been used in anger.
6. **The security page wording.** Approved by the user 2026-11-24, and queued
   rather than applied — see "Security docs" for why it cannot go in yet.

## Open questions

One, inherited from resolving #4 above and belonging to the desktop follow-up
plan rather than to the engine work:

1. **Does the desktop's registration go project-scoped (into the working folder,
   where it may be committed and shared) or user-scoped (per machine)?**
   Project-scoped is what makes "which workspace" answer itself, but a committed
   absolute path to a macOS app bundle breaks for a Windows teammate.
   Recommendation in "Where the work goes": project-scoped using the bare
   `flowproof` command, absolute bundled path for user-scoped only. Needs a
   decision once the engine's `mcp install` exists and its per-client writers are
   real.

## What this plan does not know

- The current MCP protocol revision to advertise, and each client's current
  config path and key shape. Both change; both must be read from the clients'
  own docs during steps 3 and 8 rather than from here.
- Whether Claude Code's plugin manifest can carry an MCP server entry in the
  form step 11 assumes. Unverified, and deliberately the last step so that being
  wrong about it costs nothing already shipped.

