---
status: open
---
# Plan 3 — an agent skill that drives `flowproof config`

[Issue #529](https://github.com/automators-com/flowproof/issues/529): "Add
an agent skill that teaches an agent to help users with configuration."
Plan 1 shipped the thing to configure — `flowproof config sap` / `flowproof
config fiori`, merged to `main` in PR #531 (`94bb2b7`), documented in
`docs/getting-started.md:1185-1236`. This plan is the other half, aimed at
flowproof's actual end users — people automating test cases against their
own SAP or Fiori system, not this repo's contributors (see "Who this is
for" below): a packaged skill so that when someone is sitting in a coding
agent — Claude Code, or any of the broader group of harnesses that have
converged on a shared skills convention (Codex CLI, GitHub Copilot, Cursor,
Gemini CLI, and others — see "What a 'skill' actually is here" below) — in
their own test-automation project rather than a bare terminal, the agent can
walk them through configuring flowproof, instead of them reading the docs
section by hand.

## What a "skill" actually is here

Not a flowproof concept — an external one, worth grounding rather than
assuming. The **Agent Skills** format (`agentskills.io`, opened by Anthropic
in December 2025) is a filesystem convention: a directory containing a
`SKILL.md` — YAML frontmatter (`name`, `description`) plus a Markdown body of
instructions — that an agent reads to decide when and how to act. The
frontmatter is discovery metadata (the agent scans it to decide relevance);
the body is what the agent actually follows once invoked. This part is
identical across agents. What is **not** identical is where each agent looks
for one — and this is worth getting right rather than assumed, since an
earlier draft of this plan got it wrong: **there is no single `.codex/`
convention.** Per OpenAI's own Codex CLI docs
(`developers.openai.com/codex/skills`), Codex scans `.agents/skills` from
the working directory up to the repository root — the same directory a wide
group of other harnesses has converged on (GitHub Copilot, Cursor, Gemini
CLI, and others). Claude Code is the one holdout with its own convention:

- Claude Code scans `.claude/skills/<name>/SKILL.md` — project-scoped,
  checked from the working directory up to the repo root, plus nested
  `.claude/skills/` below it.
- Codex CLI, GitHub Copilot, Cursor, Gemini CLI, and the rest of the
  converged group scan `.agents/skills/<name>/SKILL.md` — same file shape,
  one shared directory, checked the same way (working directory up to the
  repo root).

So "an agent skill that works broadly" is not one file, it's one *body of
instructions* delivered at two paths — `.claude/skills/` covers Claude Code
alone, `.agents/skills/` covers the rest of the named group in one write.
Neither path is under `CLAUDE.md`'s constitution-protected list (`CLAUDE.md`,
"The autonomous loops": `CHARTER.md`, `CODEOWNERS`, `scripts/gate/`,
`scripts/loop/`, `.github/workflows/`, `CLAUDE.md` itself) — a loop can add
both.

## Who this is for

**Decided (clarified after this plan's first draft): the primary target is
flowproof's end users** — people who `npm install`ed, `pip install`ed, or
`cargo install`ed flowproof to automate test cases against their own SAP or
Fiori system, working in *their own* repo, not this one. A flowproof
contributor working inside `flowproof` itself and wanting the same help with
`examples/fiori/*.flow.yaml` benefits from the identical mechanism, but
that's a side effect of building this correctly, not the thing being
designed for.

That single fact changes the design materially from this plan's first
draft. A skill only helps if the agent sitting in the end user's own
project — a repo this codebase has never seen, with no relationship to
`flowproof`'s own `.claude/`/`.agents/` directories — can find it. Committing
`SKILL.md` files to *this* repo would help nobody outside it. The skill has
to travel with the thing the end user actually installs: the `flowproof`
binary, via all three of its shipped distributions (npm, PyPI, `cargo
install` — CHARTER §3's "flowproof ships to npm and PyPI," now with a
concrete consequence for this plan). See "Getting the skill into the end
user's project" below for the mechanism this plan proposes for that.

## What the skill needs to know that isn't obvious from reading the CLI cold

The reason this is worth a skill and not just "point the agent at
`--help`": `flowproof config sap`/`fiori` was built with two genuinely
different modes, and picking the wrong one from inside an agent's shell tool
is either a hang or a silently incomplete write.

- **All-or-nothing per invocation, not per-field.** `cmd_sap`
  (`crates/flowproof-cli/src/config.rs:317-358`) checks `any_flag` once — if
  *any* flag is set, the whole call is flag-driven and nothing prompts;
  `SharedArgs::apply_to` (`config.rs:291-310`) then merges only the fields
  actually passed, leaving the rest of the stored profile untouched. An
  agent that runs `flowproof config sap --user alice` expecting to be asked
  for the password next gets silence — the command already returned,
  password unset. The skill has to gather every field it intends to set
  *before* invoking the command once, not incrementally.
- **No TTY means no prompting, on purpose, not as a bug.** `require_tty`
  (`config.rs:409-419`) checks `stdin().is_terminal()` and fails fast,
  naming the flags to pass instead, rather than blocking on a `read_line`
  that will never resolve — proven by
  `config_sap_without_flags_or_a_tty_fails_fast_naming_the_flags`
  (`crates/flowproof-cli/tests/config_cli_e2e.rs:122`). An agent's `Bash`-style
  tool call is exactly this case: no TTY attached. This is precisely the
  mechanism plan 1 built for a non-interactive caller
  (`001-credential-config.md`, "Command surface": *"Flags... should exist
  alongside the prompts for anyone scripting a machine's setup"*) — the
  skill is that caller, not a workaround for a gap.
- **Verification never touches the secret.** `flowproof config show`
  (`config.rs:422-431`, dispatched `lib.rs:2451`) masks the password field
  unconditionally; `flowproof config path` (`config.rs:433`) prints only the
  resolved path. Both are safe for the agent to run and echo back to the
  user for confirmation — no masking discipline needed on the skill's side
  for these two.
- **The env var names, and which command sets which.** `sap` writes
  `SAP_USER`/`SAP_PASSWORD`/`SAP_CLIENT`/`SAP_LANGUAGE`/`SAP_CONNECTION`;
  `fiori` writes `FIORI_USER`/`FIORI_PASSWORD`/`FIORI_CLIENT`/
  `FIORI_LANGUAGE`/`FIORI_BASE_URL` — deliberately not shared
  (`001-credential-config.md`, "Two profiles, not one identity"). A skill
  that doesn't know this will either ask for the wrong fields or conflate
  the two profiles into one set of prompts.
- **This seeds env vars, it doesn't override them.** `seed_env`
  (`config.rs:210-223`) only fills a variable that's currently unset — an
  explicit shell export or a suite's own `env:`/`env_from` always wins. If a
  user asks the agent "I configured this, why is my flow still using the
  wrong password," the skill should know to check for an existing shell
  export or `suite.yaml` `env:` block before assuming the config file is
  wrong.

None of this is discoverable from `flowproof config --help` alone (clap's
generated help doesn't explain *why* partial-flag calls don't prompt, or
that no-TTY is deliberate) — it's exactly the kind of operational knowledge
a skill exists to package.

## The password question — decided, but stated plainly

This is the one place this plan changes something rather than just
packaging existing behavior, and it deserves the same directness plan 1 gave
"The secret sitting on disk."

`--password` is a plain CLI flag (`config.rs:143,146,159-161`) — there is no
`--password-stdin` or `--password-env` alternative today. If the skill
collects a password conversationally and runs
`flowproof config sap --password "$THE_PASSWORD"` on the user's behalf, that
password briefly exists in the process list (`ps`) and, on many shells, in
shell history — a strictly worse exposure than the interactive
`rpassword`-masked prompt (`prompt_password`, `config.rs:253-266`) that
`flowproof config sap` already offers a human typing directly into their own
terminal.

**Decided: the skill does not handle the password itself.** For every
non-secret field (`user`, `client`, `language`, `connection`/`base_url`) the
skill gathers the value conversationally and passes it as a flag — nothing
sensitive there, and doing so in one shot is exactly what "all-or-nothing
per invocation" above requires anyway. For the password, the skill's
instructions are to tell the user to run `flowproof config sap` (or
`fiori`) themselves, interactively, in their own terminal — the masked-input
path that already exists — and then the agent verifies the result with
`flowproof config show`/`path`, which never touch the secret. This keeps a
real credential from ever entering the agent's context window, a tool-call
transcript, or shell history, at the cost of one manual step the agent talks
the user through rather than performs. Given `CLAUDE.md`'s "be careful not
to introduce security vulnerabilities" posture and this codebase's existing
discipline around where a secret is allowed to sit (`CHARTER.md` invariant
9, plan 1's own `0600`-on-write reasoning), narrower-by-default is the right
starting point for a v1 skill; a `--password`-collecting mode is a strict
relaxation that's easy to add later if this ever proves too much friction in
practice, not a decision this plan should default to.

## Getting the skill into the end user's project

The skill's content has to originate from this repo (it's the thing that
knows `flowproof config`'s real behavior, see above) but has to land inside
a repo this codebase has never seen. **Recommendation: a new
`flowproof config skill` subcommand** that writes the skill files into the
current working directory — i.e., the end user runs it from inside their
own test-automation project, the same place they already run `flowproof
record`/`flowproof run`.

```
$ cd my-sap-tests/          # the end user's own repo, not flowproof's
$ flowproof config skill
wrote .claude/skills/flowproof-config/SKILL.md
wrote .agents/skills/flowproof-config/SKILL.md
```

Mechanics:

- **One canonical source, embedded at compile time.** The `SKILL.md`
  content lives once in this repo — proposed at
  `crates/flowproof-cli/skills/flowproof-config/SKILL.md` — and is pulled
  into the binary with `include_str!`, the same technique already available
  in this dependency tree for embedding a static asset at build time. This
  is what makes the earlier draft's "two committed copies, kept in sync by a
  test" problem disappear: there is exactly one file to edit, and the
  command is what produces both on-disk copies, every time it runs, from
  that one source. It also means the skill ships through **all three**
  distributions for free — npm and PyPI both wrap the same compiled binary
  (or bind to the same Rust code, for the Python wheel), so there's no
  separate packaging step to keep in sync with the Rust source, matching
  CHARTER invariant 7 ("thin renderings over the same code paths").
- **Both targets, by default.** Writes `.claude/skills/flowproof-config/SKILL.md`
  and `.agents/skills/flowproof-config/SKILL.md` under the cwd
  unconditionally — the second one alone already covers Codex CLI, GitHub
  Copilot, Cursor, and Gemini CLI, so two writes is broad coverage, not a
  compromise. An end user doesn't have to know in advance which convention
  their agent reads, and writing an unread file nobody asked for costs
  nothing. `--claude` or `--agents` alone if someone wants only one; a
  generic `--dir <path>` for any harness outside both conventions (e.g. VS
  Code's own `.github/skills/`), so a harness this plan didn't name is a
  flag away rather than a future code change.
- **Idempotent, not silently destructive.** Re-running with no changes is a
  no-op (content already matches, nothing written, exit 0 same as a
  successful write). If a target file exists and differs — most likely
  because a newer flowproof version shipped an updated skill body — the
  command refuses and names `--force`, rather than overwriting a file the
  user might have hand-edited without telling them. Same discipline
  `flowproof config sap`/`fiori` already applies to merging rather than
  blanking a stored profile, translated to "don't clobber a file without
  being asked."
- **Fits under the existing `Config` subcommand**, since v1 ships exactly
  one skill and it is specifically the config skill: `ConfigAction::Skill`
  alongside `Sap`/`Fiori`/`Show`/`Path`
  (`crates/flowproof-cli/src/lib.rs:139-174`). If a second skill is ever
  built (a `doctor` skill once plan 2 ships is the obvious candidate), a
  more general `flowproof skill install <name>` surface would likely
  replace this rather than `config` growing an unrelated skill catalog —
  not designed now, since CHARTER's scope discipline argues against building
  a multi-skill registry surface to serve a single skill.

## The skill's shape

Content is unchanged from the mechanism above — it's a static file, written
verbatim to both target paths, so the design question is just what it says.
Sketch, not final copy:

```yaml
---
name: flowproof-config
description: >
  Configure flowproof's SAP GUI and Fiori credentials by walking the user
  through `flowproof config sap` / `flowproof config fiori`. Use when the
  user wants to set up, change, or check their SAP or Fiori login for
  flowproof, or when a flow run fails because SAP_USER, SAP_PASSWORD,
  FIORI_USER, or FIORI_PASSWORD aren't set.
---
```

Body covers, in order: (1) ask which profile — SAP GUI (`sap`, Windows-only)
or Fiori (`fiori`, cross-platform) — if not already clear from context; (2)
gather the non-secret fields for that profile conversationally; (3) run the
single flag-driven command with everything gathered *except* the password,
against the end user's own globally-installed `flowproof` binary — the same
one `flowproof config skill` itself was invoked from, nothing this plan
introduces changes what that binary does; (4) tell the user to run the bare
`flowproof config sap`/`fiori` command themselves for the password step, and
explain what each prompt is asking for; (5) verify with `flowproof config
show` and report the path from `flowproof config path`; (6) if the user
reports a flow still failing after this, check for a shell-exported var or a
`suite.yaml` `env:` block that would take precedence per `seed_env`'s
fill-gaps-only rule, before assuming the config file itself is wrong.

## Non-goals

- **No live validation.** Same posture as `flowproof config` itself
  (`001-credential-config.md`, "The shape the team landed on") — the skill
  never claims a credential is correct, only that it was written or that the
  user was walked through writing it.
- **No password collection by the agent**, per "The password question"
  above — not a gap, a decision.
- **No `flowproof doctor` integration.** Plan 2 (`002-sap-fiori-doctor.md`,
  status busy) is the connectivity-check half; this plan is configuration
  only. Once plan 2 ships, a natural follow-up is the skill offering to run
  `doctor --sap`/`--fiori` after writing the config, but that's a dependency
  on work that hasn't landed yet, not part of this plan.
- **No `agentskills.io` registry publish in v1.** `flowproof config skill`
  is the distribution mechanism this plan ships — the end user already has
  `flowproof` installed by the time they'd run it, so a registry listing
  would be a second, parallel distribution path solving the same problem a
  different way, not a prerequisite for this one to work. Worth revisiting
  once the command itself has been used in practice, not before.
- **No general multi-skill catalog surface.** `flowproof skill install
  <name>` for a future roster of skills is a real shape but a decision for
  whenever a second skill actually exists — see "Getting the skill into the
  end user's project" above.

## Phasing

1. Write the canonical `crates/flowproof-cli/skills/flowproof-config/SKILL.md`,
   grounded in the behavior documented above.
2. `ConfigAction::Skill` (`crates/flowproof-cli/src/lib.rs:139-174`):
   `include_str!` the canonical file, write it to
   `.claude/skills/flowproof-config/SKILL.md` and
   `.agents/skills/flowproof-config/SKILL.md` under the cwd, `--claude`/
   `--agents` to select one, `--dir <path>` for any other convention,
   `--force` to overwrite a differing existing file. Tests (natural home:
   `crates/flowproof-cli/tests/`, alongside the existing
   `config_cli_e2e.rs`): fresh write, idempotent re-run is a no-op, a
   differing existing file is refused without `--force` and accepted with
   it.
3. Manually exercise the whole path end to end: run `flowproof config
   skill` in a **scratch directory standing in for an end user's own
   project** — not this repo — then open that directory in a live Claude
   Code session (and, if available, an `.agents/skills`-reading harness such
   as Codex CLI) and confirm the agent actually asks for the right fields,
   invokes the right flags, and hands the password step back to the user
   rather than collecting it. This is the "start the dev server and use the
   feature" discipline `CLAUDE.md` asks of UI work, applied to an
   agent-facing surface — and it specifically has to happen outside this
   repo, since the whole point is that the skill works for a project
   flowproof has never seen.
4. One line in `docs/getting-started.md`'s `flowproof config` section
   (after `docs/getting-started.md:1236`) documenting `flowproof config
   skill` itself, so a reader who isn't already inside an agent session
   knows the command exists.

## Open questions

- **Whether a future `--password-stdin`/`--password-env` flag on
  `flowproof config` changes the answer in "The password question."** If
  one is ever added (for CI-provisioning use cases, independent of this
  plan), it would let the skill pass a password through an env var the
  agent sets and immediately unsets, without a literal value on the command
  line — meaningfully safer than today's `--password`, and worth revisiting
  the "agent never touches the password" decision against at that point.
  Not proposed by this plan; noted so it isn't rediscovered from scratch
  later.
- **Whether `flowproof config skill` should also work when there's no
  `flowproof.exe`/binary co-located** — i.e., does anything about how npm's
  wrapper or the PyPI wheel resolves to the underlying binary change what
  `include_str!`'s embedded content actually is at runtime for those two
  distributions versus a `cargo install`? Expected to be a non-issue (all
  three distributions run the same compiled Rust binary or bind to the same
  crate), but worth a concrete check against the actual npm/PyPI packaging
  scripts during implementation rather than assumed here.
