---
status: draft
---
# Plan 1 — `flowproof config`: one file, two SAP surfaces, credentials off the command line

The proposal that started this: an interactive login flow, Infisical-style —
`flowproof login sap`, prompt for credentials, validate them against a live
system on the spot, store a session. The team pushed back on the validation
step, not the interactivity: SAP already knows how to reject a bad
user/password/client combination, and re-implementing that check is work
flowproof doesn't need to own. What's wanted is narrower — `flowproof config
sap`, an interactive prompt that writes values to a file and defers
correctness to the moment the value is actually used, which for this codebase
means record or run time, not config time. This plan works out what that file
is, where it lives, and how a value written to it becomes the same `${VAR}`
a flow already resolves.

## What's actually true about credentials today

There is exactly one place in the entire codebase that resolves a `${VAR}`
reference: `resolve_refs` (`crates/flowproof-trace/src/secret.rs:47`) calls
`std::env::var(name)` and fails closed on the first unset variable. Every
consumer — flow steps, `session:` cookies, suite `env:`, `login:` blocks —
goes through this one function or its JSON-walking twin
(`resolve_refs_in_json`, `secret.rs:76`). Its own doc comment states the
contract plainly: "the engine resolves the reference from the environment at
the moment of use (recording AND every replay)" (`secret.rs:7`). That
sentence is the thing this plan must not break — a config file is a new
*source* for a value, never a new way of storing or resolving one.

Two adapters read SAP credentials today, both straight from the process
environment, both by convention rather than by any shared schema:

- `sap-com` (`crates/flowproof-adapters/src/sap_com.rs`) reads `SAP_USER` and
  `SAP_PASSWORD` as a pair, `SAP_CLIENT` and `SAP_LANGUAGE` optionally
  (`login_for`, `sap_com.rs:101-125`), and `SAP_LOGON_EXE` for a non-default
  SAP Logon install path (`sap_com.rs:803`). A flow's own `login:` block
  overrides all of it (`sap_com.rs:106-107`) — env vars are the fallback, not
  the only path.
- The Fiori example (`examples/fiori/manage-info-records.flow.yaml:19-24`)
  types `${SAP_USER}`/`${SAP_PASSWORD}` into the launchpad's own login form
  and builds the URL as `${FIORI_BASE_URL}?sap-client=${SAP_CLIENT}&sap-language=${SAP_LANGUAGE}`
  — confirmed live, per the flow's own comment, that the shell's bootstrap
  path 404s without client/language on the request. **Same four names as
  SAP GUI.** Fiori and SAP GUI aren't two credential systems that happen to
  share a backend; today they're one identity (`SAP_USER`, `SAP_PASSWORD`,
  `SAP_CLIENT`, `SAP_LANGUAGE`) plus one surface-specific extra —
  `SAP_CONNECTION` (a flow's `connection:` field, the SAP Logon entry name)
  for GUI, `FIORI_BASE_URL` for Fiori.

Nothing today persists any of this. The repo's own root `.env` (gitignored,
`.gitignore:33-34`) holds exactly this set —
`FIORI_BASE_URL`/`SAP_USER`/`SAP_PASSWORD`/`SAP_CLIENT`/`SAP_LANGUAGE`/
`SAP_ODATA_BASE`/`SAP_ODATA_VERIFY_SSL` — but flowproof has no dotenv loader
anywhere in its dependency tree; that file is sourced by whatever the
developer's shell does before invoking `flowproof`, not by flowproof itself.
It's also per-checkout: this machine alone has four flowproof working
copies (the primary checkout plus `flowproof-489`/`-490`/`-491` worktrees),
and under today's model each one needs its own populated `.env`. A global,
flowproof-owned file is additive to that, not a replacement for suite-level
`env:`/`env_from` — it answers "what are MY default SAP credentials on this
machine," not "what does THIS suite need," which stays exactly as `suite.yaml`
already does it (`docs/getting-started.md:721-731`: "Real credentials and
tokens always go through `${VAR}` references... the trace stores only the
reference, never the value").

## The shape the team landed on

`flowproof config sap` (and `flowproof config fiori`) prompts interactively
and writes the answers to a file. Nothing is checked against a live SAP
system at that point — the value is written whether or not it would work.
This isn't a gap: `sap_com.rs` already turns a wrong value into a specific,
readable error the first time it's actually used (`sap_com.rs:1149-1163`,
e.g. "SAP credentials were submitted but no logged-in session appeared;
check the client and credentials..."). Config-time validation would mean
flowproof re-implementing SAP's own auth logic just to fail a few seconds
earlier; runtime validation is a message that already exists. This also
keeps the invariant this codebase already lives by (§2 of `CHARTER.md`,
"replay makes zero LLM calls / needs no credentials") intact for the free
path — `config` only matters for a *record* leg against a live system, never
for `run`.

## Where the file lives

flowproof ships to npm and PyPI and as a Cargo-installed binary (`CHARTER.md`
§3, "no public API break... flowproof ships to npm and PyPI") — end users
install it globally, so the config file has to be a per-user machine default,
not a per-project one (that's what `.env` + `suite.yaml`'s `env:` already
are). That means a real platform directory, not a hardcoded `~/.flowproof`:
`%APPDATA%\flowproof\config.yaml` on Windows, `~/Library/Application
Support/flowproof/config.yaml` on macOS, `$XDG_CONFIG_HOME/flowproof/config.yaml`
(falling back to `~/.config/flowproof/config.yaml`) on Linux. Nothing in the
dependency tree resolves these today (`Cargo.lock` has no `dirs` or
`directories` crate) — this plan adds one rather than hand-rolling platform
detection, the same call this codebase already made for `ctrlc` in
`flowproof-cli` (small, single-purpose, worth a dependency rather than
reinventing it). **Recommendation: `dirs`, not `directories`.** `directories`
is built around `ProjectDirs::from(qualifier, organization, application)` —
reverse-DNS-style three-part namespacing meant for an app that's one of many
a vendor ships. flowproof is a single, self-contained CLI with no sibling
apps to namespace against; `dirs::config_dir()` gives the same per-platform
path with none of that machinery. Both are MIT/Apache-2.0, so licensing
doesn't distinguish them.

Worth noting: `sap-com` is Windows-only (`driver_for`, `crates/flowproof-cli/src/lib.rs:363-371`,
`#[cfg(not(windows))]` refuses it elsewhere), so `flowproof config sap`'s
primary audience is Windows machines, while `flowproof config fiori` (backed
by the `web` adapter, CDP, cross-platform) has no such restriction. The path
resolution needs to be right on all three, but Windows correctness matters
most for the `sap` half.

YAML, not TOML or JSON: every other user-facing file in this repo already is
(`*.flow.yaml`, `suite.yaml`), `serde_yaml` is already a `flowproof-cli`
dependency (used today for `audit`'s default output,
`crates/flowproof-cli/Cargo.toml:29`), and a flowproof user already knows the
syntax before they ever type `flowproof config`.

## Two profiles, not one identity

**Decided, against the shared-identity model the flow-level evidence above
pointed toward:** `sap` and `fiori` are configured as two separate profiles,
each with its own full set of fields — because flowproof already treats
`app: sap` and `app: web` as different apps (`driver_for`,
`crates/flowproof-cli/src/lib.rs:358-405`), and a real deployment's values
for each can genuinely differ (SSO-fronted Fiori sitting in front of a
password-auth SAP GUI backend is exactly the case the earlier draft only
raised as a risk).

That has a concrete consequence worth stating rather than glossing over:
every Fiori example flow that exists today —
`examples/fiori/manage-info-records.flow.yaml:27-28`,
`login-smoke.flow.yaml:30-31`, `purchase-info-records-report.flow.yaml:44-45`
— types `${SAP_USER}`/`${SAP_PASSWORD}` into the launchpad's login form, the
exact same variable names an `app: sap` flow uses. Two independent config
profiles can't both feed the same environment variable at once — a process
has one value for `SAP_USER`, not two — so keeping the profiles genuinely
separate means the `fiori` profile has to seed *different* names,
`FIORI_USER`/`FIORI_PASSWORD`/`FIORI_CLIENT`/`FIORI_LANGUAGE`, rather than
reuse `SAP_*`. That in turn means the three example flows above need a
one-line rename each (`${SAP_USER}` → `${FIORI_USER}`, etc.) to actually
pick up an independently-configured Fiori identity; until that lands, a
Fiori flow written the old way keeps reading whatever `SAP_USER` resolves
to, config file or not. This plan ships that rename alongside the feature
(see Phasing) rather than leave the shipped example contradicting its own
docs.

```yaml
# %APPDATA%\flowproof\config.yaml / ~/Library/Application Support/flowproof/config.yaml / ~/.config/flowproof/config.yaml
sap:
  user: johndoe
  password: "..."          # see "The secret sitting on disk" below
  client: "50"
  language: EN
  connection: TS3

fiori:
  user: johndoe
  password: "..."
  client: "50"
  language: EN
  base_url: https://my-launchpad.example.com/
```

mapped to env vars — the left column existing and unchanged (SAP GUI), the
right column new (Fiori gets its own names instead of borrowing SAP GUI's):

| Config path       | Env var                  | Required by                   |
| ------------------ | ------------------------- | ------------------------------ |
| `sap.user`        | `SAP_USER`                | SAP GUI flows                 |
| `sap.password`    | `SAP_PASSWORD`            | SAP GUI flows                 |
| `sap.client`      | `SAP_CLIENT`              | SAP GUI flows (optional)      |
| `sap.language`    | `SAP_LANGUAGE`            | SAP GUI flows (optional)      |
| `sap.connection`  | `SAP_CONNECTION`          | SAP GUI flows' `connection:`  |
| `fiori.user`      | `FIORI_USER` *(new)*      | Fiori flows                   |
| `fiori.password`  | `FIORI_PASSWORD` *(new)*  | Fiori flows                   |
| `fiori.client`    | `FIORI_CLIENT` *(new)*    | Fiori flows (optional)        |
| `fiori.language`  | `FIORI_LANGUAGE` *(new)*  | Fiori flows (optional)        |
| `fiori.base_url`  | `FIORI_BASE_URL`          | Fiori flows' `url:`           |

`flowproof config sap` prompts for and writes the whole `sap.*` block;
`flowproof config fiori` prompts for and writes the whole `fiori.*` block —
each a complete, independent set, no "only ask if the other one hasn't
already set it" logic. That also makes the prompting code simpler than the
shared-identity draft would have needed.

Password entry needs masked stdin, which `std` doesn't have; a small crate
(`rpassword` or equivalent) is the natural addition, same category of
decision as `dirs` above.

## How it reaches the flow

The file is a *source of defaults*, so it has to sit at the bottom of the
precedence stack, and there's already a precedent for the opposite end of it:
`apply_suite_env` (`crates/flowproof-cli/src/lib.rs:875-885`) calls
`std::env::set_var` unconditionally for every suite `env:` entry — a suite's
own declared environment always wins, no check for "is this already set."
The new config file should do the mirror-image thing: seed a variable only
when `std::env::var(name)` is currently `Err` (fill gaps, never override), so
an explicit shell export, CI secret injection, or a suite's `env:`/`env_from`
all still win exactly as they do today. Concretely, that lands as one small
function (load config, `set_var` for each unset mapped key) called from the
very first line of `apply_suite_context`
(`crates/flowproof-cli/src/lib.rs:987`) — *before* its early return for "no
`suite.yaml` found" (`lib.rs:990-994`), because a bare single-flow `run`
with no suite is exactly the case with nothing else to fall back on. That
function is both `run_cli`'s and Python's chokepoint: `flowproof-python`
calls `flowproof_cli::apply_suite_context` directly on its record/run fast
paths (`crates/flowproof-python/src/lib.rs:94,147`), so putting the seed
there — rather than duplicating it in `main.rs` and in `flowproof-python` —
covers the CLI binary and the Python SDK from one place, which is what
`CHARTER.md` invariant 7 ("every operation is a library call... the CLI and
[bindings] are thin renderings over the same code paths") actually asks for
here.

Nothing in `secret.rs`, `spec.rs`, `sap_com.rs`, or `web.rs` needs to change.
They keep reading `std::env::var` exactly as they do now; the config file
just means that variable is very likely already set by the time any of them
look.

## The secret sitting on disk

This is the one place this plan changes the codebase's actual security
posture rather than just its convenience, and it deserves to be said
plainly rather than assumed. Today a credential exists in exactly two forms:
a shell environment variable (gone when the shell exits) or a CI secret
(injected, never written to a file the runner keeps). `CHARTER.md` invariant
9 — "no secret ever reaches a trace" — is about the *trace*; it says nothing
about a config file, because no such file exists yet. Writing
`sap.password` to a YAML file on disk is a genuinely new risk surface: a
laptop backup, a misconfigured cloud-sync folder, or a second local account
can now read it long after the session that typed it is gone.

**Decided: yes, the password belongs in the file.** A config file that only
stores a *username* would be closer to what exists today but wouldn't be the
feature that was asked for, and the reasoning behind this feature — SAP
validates, not us — only holds together if the file has enough in it to
actually attempt a login. Given that, the minimum this plan ships with: the file is created `0600` (owner read/write only) on
Unix at write time, and `flowproof config show` (see below) never echoes the
password back — it prints `********` for that one field, the same
discipline invariant 9 already applies to traces, applied here to a
terminal instead. Windows ACL tightening is real but harder to do without
another dependency; it's listed below as a gap rather than solved by
assumption. An OS keychain (`keyring`-style) is the sharper answer long
term but is a materially bigger scope than "write a YAML file," so it's
listed as a later option, not this plan's MVP.

## Command surface

```
flowproof config sap      # prompt: user, password, client, language, connection
flowproof config fiori    # prompt: user, password, client, language, base_url — its own identity
flowproof config show     # print the file's path and contents, password masked
flowproof config path     # print the resolved file path only (for scripting/editing)
```

`Config` becomes the first `Command` variant in `crates/flowproof-cli/src/lib.rs`
with a nested `#[command(subcommand)]` (clap's derive supports this cleanly;
today's `Command` enum, `lib.rs:137-340`, is flat, so this is the first
nesting — worth a look at the rendered `--help` once it exists, but not
expected to be an issue). Prompting needs a real TTY; run non-interactively
(piped stdin, CI) it should fail with a clear message rather than hang —
the same failure shape `record`'s agent-boundary work already treats as a
first-class case (`CHARTER.md` §4, #188: "an agent that fails to start says
so"). Flags (`--user`, `--password`, `--client`, `--language`, `--connection`,
`--base-url`) should exist alongside the prompts for anyone scripting a
machine's setup rather than typing it by hand — same reasoning as `--json`
existing next to the human output on every other command.

## Other apps, later

`CHARTER.md` §3 is explicit: "no new `app:` target... a seventh is a
product decision, not a gap fix." This plan doesn't need one — Fiori is
`app: web` today, and `flowproof config fiori` only ever seeds env vars an
existing `web`-driven flow already reads, exactly as `flowproof config sap`
seeds env vars an existing `sap`-driven flow already reads. Extending this
to a third, SAP-unrelated app later (the user's own framing: "flowproof is
not mainly centered for SAP") is additive under this design — another
top-level key in the same file, another subcommand — and arguably more
naturally so now that `sap` and `fiori` are already two independent,
same-shaped profiles rather than one identity with two attachment points.

## Phasing

1. **Schema and file I/O.** The `sap:`/`fiori:` shapes above, `dirs`-based
   path resolution, `0600` on write, load-or-default on read. No CLI yet —
   this phase is a library module (`crates/flowproof-cli/src/config.rs` is
   the natural home, alongside `agent_flow.rs` and `capture.rs`) with tests
   against a temp `HOME`/`XDG_CONFIG_HOME`, not the real one.
2. **The seed call.** Wire it into `apply_suite_context`'s first line,
   fill-gaps-only, with a test proving an already-set env var is left alone
   and an unset one is filled from a fixture config file.
3. **`flowproof config sap` / `fiori`.** Interactive prompts plus the flag
   form, `show`, `path`. Same commit renames `${SAP_USER}`/`${SAP_PASSWORD}`/
   `${SAP_CLIENT}`/`${SAP_LANGUAGE}` to `${FIORI_USER}`/`${FIORI_PASSWORD}`/
   `${FIORI_CLIENT}`/`${FIORI_LANGUAGE}` in the three existing Fiori example
   flows (`manage-info-records.flow.yaml`, `login-smoke.flow.yaml`,
   `purchase-info-records-report.flow.yaml`) — landing the new env var names
   without this would leave the shipped examples unable to benefit from the
   thing this plan built.
4. **Docs.** `docs/getting-started.md`'s credentials section
   (`docs/getting-started.md:721-731`) currently describes only the
   `${VAR}`-from-environment path; it needs the config file as the
   "how do I set that variable on my own machine" answer, and
   `docs/multi-surface.md`'s open item ("nothing stages a surface's
   credentials yet," line 15) should cross-reference this once it lands,
   since a staged `login:` on a multi-surface flow will eventually want the
   same source.

## Decisions

Everything below was an open question in the previous draft. All five are
now decided; nothing in this section is still open.

- **The password belongs in the file.** Confirmed. See "The secret sitting
  on disk" above for what that commits this plan to (`0600` on write,
  never echoed back by `config show`).
- **`sap` and `fiori` are separate profiles, not one shared identity.**
  Confirmed — reasoning: flowproof already treats `app: sap` and `app: web`
  as different apps, and what needs configuring for each can genuinely
  differ. See "Two profiles, not one identity" above for the schema this
  produces and the `FIORI_*` env var consequence it carries.
- **Path-resolution crate: `dirs`, not `directories`.** See "Where the file
  lives" above for the reasoning (no multi-app namespacing to justify
  `directories`' extra structure).
- **Multiple SAP systems (dev/qa/prod) on one machine: out of scope for v1.**
  Stated plainly, since the original phrasing of this question didn't land:
  the config file holds exactly one value per field, so if you work against
  a dev system on Monday and a QA system on Tuesday, `flowproof config sap`
  only remembers whichever you typed last — switching means re-running it
  each time. A suite that needs a *specific* system regardless of your
  personal default already has an escape hatch today: `suite.yaml`'s own
  `env:` overrides the global config unconditionally (per the precedence
  rule above), so a QA-specific suite can already pin `SAP_USER`/etc.
  without touching your machine's default. Decision: ship v1 without named
  profiles (`--profile qa`) and revisit only if that escape hatch turns out
  not to be enough in practice — this is a reversible, low-cost thing to add
  later, not a shape that has to be right on day one.
- **Windows file permissions: known gap, shipped anyway.** Stated plainly:
  on macOS/Linux, a file can be marked so that only your own login can read
  it — this plan sets that (`0600`) automatically the moment the password is
  written. Windows has an equivalent (NTFS permissions/ACLs) but setting it
  from Rust needs another dependency this plan hasn't picked yet. Decision:
  ship v1 without the Windows-side restriction rather than block on it, but
  say so in the docs rather than imply parity that doesn't exist — anyone on
  a shared Windows machine should know the file is only as protected as
  Windows' default per-user file permissions make it.

## Follow-up, deliberately not in this plan

Whether `flowproof doctor` should grow a SAP/Fiori connectivity check that
reads this config (today `doctor` only checks agent-boundary connectivity,
`lib.rs:295-306`) is a real, good idea and explicitly **not** this plan's
scope — this plan is the write path only. Tracked instead in
[002-sap-fiori-doctor.md](002-sap-fiori-doctor.md), left as a stub until
this plan ships.

## Next

- [ ] Spike phase 1 (schema + path resolution + tests against a fake
  `HOME`) to confirm the platform paths are actually right on Windows,
  not just plausible
- [ ] Pick the masked-input crate (`rpassword` or equivalent) and confirm it
  and `dirs` both clear `CHARTER.md`'s Apache-2.0 licensing expectations
- [ ] Write the `docs/getting-started.md` update alongside phase 3, not
  after — CLAUDE.md's own rule ("prose describing code that no longer
  exists is a defect") cuts the same way for prose describing a command
  that doesn't exist yet
- [ ] Land the three example-flow renames (`${SAP_USER}` → `${FIORI_USER}`,
  etc.) in the same phase-3 commit that introduces the `FIORI_*` names, not
  as a separate follow-up that can be forgotten

