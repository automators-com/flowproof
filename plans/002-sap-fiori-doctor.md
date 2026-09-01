---
status: done
---
# Plan 2 — `flowproof doctor` for SAP/Fiori

Split out of [001-credential-config.md](001-credential-config.md)'s Open
Questions on 2026-08-31: whether `flowproof doctor` (today, agent-boundary
connectivity only) should grow an equivalent check for SAP GUI / Fiori — read
the config plan 1 writes, and report what it can actually reach, before
someone spends time writing a flow against it. Good idea, explicitly
deferred there because plan 1 is the write path only; this is the read-time
half.

**Status of the dependency:** plan 1 has since merged to `main` — PR #531,
merge commit `94bb2b7`. This plan's own branch (`docs/plan-sap-fiori-doctor`)
forked before that landed, so it doesn't have `crates/flowproof-cli/src/config.rs`
in its own tree yet; every citation below into `config.rs` and every line
number quoted from `crates/flowproof-cli/src/lib.rs` was re-checked directly
against `origin/main` (not the stale fork point), so they're accurate as of
now. Whoever picks up implementation still needs to merge/rebase `main` into
this branch first — the design is no longer blocked, only the branch is
behind.

## What `doctor` checks today

`Doctor` is one flat command — `agent`, `timeout`, `prompt`, no subcommands
(`crates/flowproof-cli/src/lib.rs:345-355`), dispatched to
`agent_flow::cmd_doctor` (`lib.rs:2530-2534`). It answers one question: does
this agent's model traffic reach flowproof's proxy, using a synthetic
one-turn cassette built in Rust — `Cassette { turns: vec![...] }`
(`agent_flow.rs:1130-1151`) — rather than loaded from disk, so the check
costs no key and needs no spec file at all. Its doc comment states the
governing philosophy plainly and it matters for everything below: *"It
deliberately does NOT print a verdict like 'your wiring is correct'... An
agent with two clients can reach the proxy with one and the real provider
with the other, so a request arriving proves that A client found the proxy —
not that all of them did. Reporting the observation and letting the reader
judge is the honest shape"* (`agent_flow.rs:1118-1123`). A SAP/Fiori doctor
has the same shape of ambiguity — a session that exists might belong to
someone else, a host that answers might be a login page that immediately
404s post-auth — so it should inherit the same discipline: report what was
observed, not a pass/fail verdict.

It's documented, working CLI surface, not just an internal helper:
`docs/agent-testing.md:139` and `:157` both show `flowproof doctor --agent
"./start-agent" [--prompt "..."]` as the primary onboarding example. Whatever
this plan adds must not change what that invocation does.

## What plan 1 hands this plan to read

`crates/flowproof-cli/src/config.rs` (now on `main`) defines
`Config { sap: Option<SapProfile>, fiori: Option<FioriProfile> }`
(`config.rs:104-109`), `config::load() -> Result<Config, String>`
(`config.rs:160`), and `config::seed_env()` (`config.rs:210`) —
fill-gaps-only: it sets an env var from the config file **only** when
`std::env::var(name)` is currently unset, so an explicit shell export or a
suite's own `env:` still wins. That function is called from the first line
of `apply_suite_context`'s body (`lib.rs:1044-1047`), which is both
`run_cli`'s and `flowproof-python`'s shared chokepoint.

That precedence detail is the reason `doctor` should call `config::seed_env()`
too, rather than `config::load()` directly and read the profile's fields off
the struct. Reading the struct would show what's *in the file*; calling
`seed_env()` first and then reading `std::env::var` shows what a real
`record`/`run` would actually see — the same effective value, arrived at the
same way, including a shell export overriding a stale config-file password.
A doctor that disagreed with the thing it's meant to diagnose because it
computed the answer a different way would be worse than not existing.

## The command surface: flags on `Doctor`, not a nested subcommand

Plan 1 made `Config` the first nested subcommand in this file's `Command`
enum, explicitly noting today's enum is otherwise flat
(`001-credential-config.md`, "Command surface"). The same move for
`Doctor` — `flowproof doctor agent --agent ...` / `doctor sap` / `doctor
fiori` — is the obvious follow-on shape, and it's wrong here for a reason
`Config` didn't have to weigh: `Config` had no prior flat form to break, and
`Doctor` does. `docs/agent-testing.md` shows `flowproof doctor --agent
"./start-agent"` as the front door for onboarding; nesting it under `doctor
agent` either breaks that command or means keeping the flat form as an
undocumented compatibility alias forever, which is worse than not nesting at
all.

Instead: add `--sap` and `--fiori` as boolean flags on the existing `Doctor`
command, mutually exclusive with `--agent` and with each other via
`conflicts_with_all` — the exact mechanism this file already uses for
`Record`'s `--headed`/`--headless`/`--keep-open` combination
(`lib.rs:213-222`) and `Audit`'s `--run`/`--since` (`lib.rs:331-335`).
`agent` moves from a required field to `Option<String>`, gated by a manual
"exactly one of `--agent`, `--sap`, `--fiori`" check at the top of the
handler (clap's derive doesn't have a three-way "exactly one of, and one of
them takes a value" group primitive cleanly, so one explicit check is
simpler than fighting the derive macro for it).
`flowproof doctor --agent "./start-agent"` keeps working, unexamined, exactly
as documented; `--sap` and `--fiori` are new arms in the same match.

`--timeout` (already present, default 60s, `lib.rs:350-351`) stays a single
shared flag across all three checks rather than three new ones — see
"Timeout defaults" below for why one number can serve all three.

## The SAP check: observe only — decided, not just recommended

**Decided:** `doctor --sap` never authenticates. The team's own reasoning,
given while resolving this plan's open questions, is the same one plan 1
already used to justify *not* validating credentials at `config` time: SAP
already rejects a bad login on its own, with a message this codebase already
surfaces — re-implementing that check is work flowproof doesn't need to own,
and here it also carries a real cost a config-time check doesn't: a wrong
password fires a real failed-logon attempt against a live system, and SAP
commonly locks an account after N consecutive failures. `doctor --sap` stays
strictly an observation.

`driver_for("sap")` already refuses cleanly off Windows: *"app 'sap' needs
SAP GUI Scripting (COM), which exists only on Windows"* (`lib.rs:420`).
`doctor --sap` gives the identical message on the identical condition,
first, before touching anything else — a doctor that fails with a confusing
COM error on macOS/Linux instead of this one-line refusal would be worse
than the status quo of no check at all.

On Windows, the actual connectivity logic already exists and is far richer
than a boolean: `ComEngine::connect` (`crates/flowproof-adapters/src/sap_com.rs:996-1181`)
distinguishes, as *named states* it already tracks — attach found vs. no
`SAPGUI` in the Running Object Table (`attach_to_sapgui`, `sap_com.rs:950-984`),
SAP Logon not running and needing a start attempt (`start_sap_logon`,
`sap_com.rs:801-844`), a login screen reached but not yet authenticated
(`saw_login_screen`), and a session logged in as the wrong user
(`wrong_user`, `sap_com.rs:1095-1102` — deliberately left alone rather than
hijacked, per its own comment: *"Driving it would run the flow as the wrong
identity and pass — leave it alone and keep looking"*). `doctor --sap`
reuses this vocabulary for its report — attach state, connection found,
existing session's `Info.User` if any (`sap_com.rs:1090-1093`), login screen
present or not — but stops there rather than calling `try_auto_login`
(`sap_com.rs:868-908`), which is the one piece of `connect()` this plan
deliberately does not reuse, because it is the piece that submits
credentials.

One consequence worth naming rather than leaving implicit: SAP GUI already
*has* a working login-submission mechanism sitting right there in
`try_auto_login`. If SAP-side credential validation is ever wanted for
parity with the Fiori check below, it is not new engineering — it is
flipping the observe-only default off and calling code that already exists.
That is a materially different, cheaper proposition than what building
Fiori's equivalent required (see below), and it's why this plan can commit
to "no" for SAP now without that being a permanent architectural wall.

## The Fiori check: reachability first, then a real login attempt

Fiori has no equivalent shortcut. SAP GUI's `stage_credentials`/`LoginCredentials`
staged-login mechanism (`crates/flowproof-driver/src/app.rs:287-299,850-855`)
is explicitly SAP-only — the trait's default implementation errors with
*"this app does not log in at launch: `user:`/`password:` are for `app:
sap`"* (`app.rs:854`), and `flowproof-adapters/src/web.rs` never overrides
it. That's not an oversight; it matches what every existing Fiori example
flow already does instead — types credentials straight into the launchpad's
own on-screen fields as ordinary steps: `Type ${SAP_USER} into the "User"
field` (`examples/fiori/manage-info-records.flow.yaml:26`, pre-plan-1
naming — plan 1 renames this to `${FIORI_USER}` in the two flows without a
committed cassette). For Fiori, there is no non-UI login channel to check —
"does the credential work" and "does the UI login flow work" are the same
question, whether this plan wants them to be or not.

**Decided (reversing this plan's earlier draft):** `doctor --fiori` does
validate `FIORI_USER`/`FIORI_PASSWORD` for real, because — as raised while
resolving the open questions — a doctor that only proves a URL answers,
without proving the one credential most real-world failures actually involve
(a wrong or expired password), leaves the single most common failure
undiagnosed. Two stages, run in order:

**Stage 1 — reachability, unauthenticated, cheap.** A plain HTTP GET against
`${FIORI_BASE_URL}?sap-client=${FIORI_CLIENT}&sap-language=${FIORI_LANGUAGE}`
— the exact query shape the example flows already build
(`examples/fiori/manage-info-records.flow.yaml:19-24`, whose own comment
notes the launchpad's bootstrap path 404s without both params on the
request). This needs no new dependency: `ureq` is already resolved in this
workspace's graph, reached transitively via `flowproof-cli`'s `Cargo.toml:25`
(`flowproof-adapters` with the `agent`/`vision` features, both gating
`dep:ureq`, `flowproof-adapters/Cargo.toml:16,22,36`), and `flowproof-adapters`
already uses it this exact way — `agent_proxy.rs:570-575`'s `upstream_agent()`
builds an agent with `.http_status_as_error(false)` specifically so a
non-2xx response becomes data to report rather than a `Result::Err`, and
`vision.rs:451` does a bare `ureq::get(&url).call()`. This stage reports
status code, elapsed time, whether `FIORI_CLIENT`/`FIORI_LANGUAGE` are even
set (the known, already-documented 404 cause above), and — **decided,
prints unconditionally** — the final URL after redirects. Plan 1 raised
"SSO-fronted Fiori sitting in front of a password-auth SAP GUI backend" as
exactly the reason `sap`/`fiori` got separate config profiles
(`001-credential-config.md`, "Two profiles, not one identity"); a redirect
to an external SSO host is that risk made observable before anyone writes a
flow, and printing the hostname is not a disclosure — it's the same host
the person running `doctor` already configured and whose browser already
reaches it daily.

**Stage 2 — an actual login attempt, only if `FIORI_USER`/`FIORI_PASSWORD`
are set; skipped with a named reason otherwise.** This is real UI
automation, not an HTTP call, and it should be built by reusing the pieces
that already exist for exactly this, not by hand-rolling a new one:
`flowproof_agent::rules::resolve_step("web", &SpecStep::Plain(...))`
(`crates/flowproof-agent/src/rules.rs:821`) already turns plain-English
steps like `Type ${FIORI_USER} into the "User" field` into `ResolvedAction`s
with the same label-matching tolerance real flows rely on (its own tests
already cover loosely-worded matches, e.g. `rules.rs:3581`,
`"Type Ada into the name field"`), and `flowproof_agent::recorder::record`
(`recorder.rs:2124`) already dispatches those actions against any
`AppDriver` (`ResolvedAction::TypeText` handling at `recorder.rs:2427`).
Rather than a `.flow.yaml` on disk, `doctor --fiori` constructs a
`flowproof_agent::FlowSpec` (`crates/flowproof-agent/src/spec.rs:409` —
`app` at `spec.rs:417`, `url` at `spec.rs:428`, `steps: Vec<SpecStep>` at
`spec.rs:519`, all public fields) in Rust directly,
with two or three canned steps (type user, type password, submit), and runs
it against `driver_for("web")`. This is the same trick `cmd_doctor` already
uses for the agent check — build the synthetic artifact in code instead of
on disk (`agent_flow.rs:1130`, the in-memory `Cassette`) — applied a second
time to a synthetic `FlowSpec` instead of a synthetic cassette.

What counts as "logged in" for this stage — the shell having rendered vs.
the login form still showing, most likely read via `driver.surface_text()`
(already used this way at `recorder.rs:2218,2559`) — is an implementation
detail to settle during build, not a design question; flagged here so it
isn't silently skipped.

**This stage is genuinely more expensive and higher-blast-radius than
anything else `doctor` does today, and that's worth stating as plainly as
plan 1 stated its own known gaps.** It boots a real headless Chromium
(`web.rs:1062` on), loads a real page, and submits real credentials to a
real system — a wrong password here is a real failed logon, same
lockout-risk shape this plan just used to justify *not* doing this for SAP.
The difference is that this was an explicit, informed choice for Fiori
rather than an oversight: the team's reasoning was that "is everything
configured and working" has to include the credential, or the check isn't
answering the question it's named for. One practical consequence that
follows directly: **`doctor --fiori` must never be wired into a CI job or
any other repeated/automated trigger** — a human running it occasionally is
a healthy pattern; a scheduled job re-attempting a stale password every five
minutes is exactly the lockout scenario this plan is accepting the risk of
for a one-off manual check, not for a loop.

## Timeout defaults

No number is derivable from the code, but a shared `--timeout` (the existing
flag, default 60s, `lib.rs:350-351`) can reasonably cover all three checks
without new flags, because the three have very different natural durations
and only one of them needs most of the budget:

- **SAP observe** and **Fiori Stage 1** should both self-limit internally to
  a handful of seconds regardless of `--timeout` — neither legitimately
  needs longer than a local network round-trip or a ROT lookup, in the same
  spirit as a `curl --connect-timeout 5`-style health check. `--timeout`
  mostly exists as an outer safety net if something hangs, not as the
  expected duration.
- **Fiori Stage 2** is the one check that can plausibly use most of the
  default 60s — headless Chromium boot plus a real page load plus form
  fill plus waiting for the shell to render is comparable to what a real
  flow's own login block already takes. 60s is also already this exact
  flag's existing default for a comparable "an external process might
  legitimately take a while and that's not itself a failure" case: the
  agent doctor treats hitting its own `--timeout` as "an observation, not a
  wiring failure" rather than an error (`agent_flow.rs:1171-1176`).

No new flag, in other words: reuse `--timeout`, give the fast checks their
own small internal ceiling, and let the slow one use the shared budget.

## Non-goals

- **No new `app:` target.** Fiori doctoring stays `app: web`'s concern; this
  adds a diagnostic helper and a synthetic in-code `FlowSpec`, not a driver
  (CHARTER §3).
- **No OData/backend probe.** `SAP_ODATA_BASE`/`SAP_ODATA_VERIFY_SSL` exist
  in the repo's own `.env` (per `001-credential-config.md`) but nothing in
  this plan's scope claims to validate them — that's a materially different
  surface (Basic Auth against a REST endpoint, not a launchpad) and isn't
  named in the deferred question this plan split from.
- **No write path.** `doctor` only reads; writing config values stays
  `flowproof config`'s job, unchanged.
- **`doctor --sap` never authenticates, full stop** — no flag reintroduces
  this later without a fresh decision (see "SAP check" above). Whatever it
  reports never includes a password, matching `config show`'s masking
  discipline (`config.rs:130`, `masked()`).
- **No CLI credential override flags** (`--user`/`--password`) on `doctor`
  itself — it reads whatever `record`/`run` would read, via
  `config::seed_env()` plus the environment, exactly like every other
  consumer of these variables.
- **`doctor --fiori` is not CI-safe** and this plan does not attempt to make
  it so — see the lockout-risk paragraph above.

## Phasing

1. Rebase/merge `main` into this branch — plan 1 is there now
   (`config::seed_env()`, `Config`/`SapProfile`/`FioriProfile`).
2. `flowproof-adapters`: the Fiori Stage 1 HTTP probe as a small free
   function (name TBD, e.g. `fiori_reachability(url: &str) -> FioriReachability`,
   carrying status/elapsed/final-url/error), using the existing `ureq` idiom
   from `agent_proxy.rs`/`vision.rs`; unit tests against a local `tiny_http`
   server (already a dev-dependency, `flowproof-cli/Cargo.toml:41`) for the
   200/404/redirect/timeout cases, no network access required.
3. `Doctor`'s `agent` field becomes optional, `--sap`/`--fiori` added with
   `conflicts_with_all`, dispatch split into `cmd_doctor_agent` (today's
   `cmd_doctor`, renamed, behaviour untouched), `cmd_doctor_sap`,
   `cmd_doctor_fiori`. A test asserting `flowproof doctor --agent X --sap`
   is rejected by clap before either handler runs.
4. `cmd_doctor_sap`: Windows-only observation path described above, gated
   `#[cfg(windows)]` with the same one-line refusal as `driver_for` off
   Windows, reusing `ComEngine`'s attach/session-lookup logic but never
   calling `try_auto_login`.
5. `cmd_doctor_fiori` Stage 1: wire the probe, seed env via
   `config::seed_env()` first, report reachability + final URL +
   client/language presence.
6. `cmd_doctor_fiori` Stage 2: construct the synthetic `FlowSpec`, drive it
   through `resolve_step`/`record`'s existing action dispatch against
   `driver_for("web")`, only when both `FIORI_USER` and `FIORI_PASSWORD`
   resolve; report shell-loaded vs. still-on-login vs. driver error, and
   skip with a named reason ("FIORI_PASSWORD is not configured — run
   `flowproof config fiori`") when credentials are absent.
7. Docs: `docs/agent-testing.md`'s doctor section gets a `--sap`/`--fiori`
   subsection, explicitly including the CI-safety warning for Stage 2;
   cross-link from `docs/getting-started.md`'s `flowproof config` section.

## Open Questions

- **SAP-parity revisit.** Not blocking this plan, and the team was explicit
  that observe-only is right *for now* — but flagged above that turning on
  real SAP login validation later is comparatively cheap (`try_auto_login`
  already exists) if the asymmetry with Fiori's real validation ever becomes
  a problem in practice. Revisit only if that friction actually shows up.
  **Still open** — nothing in the implementation below resolves it.
- ~~The exact "logged in" signal for Fiori Stage 2~~ — **resolved during
  implementation**: reused `examples/fiori/login-smoke.flow.yaml`'s own
  already-proven last two steps verbatim, `Wait until page shows Home within
  Ns` + `assert: page shows Home` — that flow's comment confirms live,
  against a real system, that "Home" resolves from the shell's own tab
  label once authenticated. Not a new signal invented for this plan; the
  one the codebase had already validated.

## What landed

All seven Phasing steps, against `main` post-#531 (merged via
`git merge --no-commit --no-ff origin/main`, so plan 1's work is present in
the working tree without a merge commit — nothing here was committed, per
instruction):

- **`crates/flowproof-adapters/src/fiori.rs`** (new): `fiori_reachability(url)
  -> FioriReachability` — Stage 1, a single unauthenticated GET via `ureq`
  (`http_status_as_error(false)`, 5s connect / 10s total timeout), reporting
  status, elapsed time, the post-redirect URL (`ResponseExt::get_uri`), and
  any transport error. 3 unit tests against a bare `TcpListener` (no
  `tiny_http` dependency needed in this crate — a one-shot raw listener gets
  the same no-network property). `flowproof-adapters/Cargo.toml`'s `web`
  feature now also gates `dep:ureq` (was `agent`/`vision` only) — no new
  external dependency, just a feature-gate correction so `web`'s own
  reachability concern doesn't ride on two unrelated features happening to
  be enabled together in this workspace's one real consumer.
- **`crates/flowproof-adapters/src/sap_com.rs`**: `pub fn observe(connection)
  -> Result<SapObservation, DriverError>` inside the existing `mod com`
  (Windows-only), re-exported at `sap_com::{observe, SapObservation,
  SapSessionState}`. A single pass over `attach_to_sapgui` +
  `GetScriptingEngine`/`Children`/`ElementAt` — the same COM calls
  `connect()` already makes — stopping short of `connect()`'s retry loop,
  `start_sap_logon`, `OpenConnection`, and `try_auto_login`. Verified with
  `cargo check -p flowproof-adapters --target x86_64-pc-windows-msvc
  --features sap-com` (target added via `rustup target add`), both before
  and after this change — genuine Windows type-checking, not a guess.
- **`crates/flowproof-cli/src/doctor.rs`** (new): `cmd_doctor_sap()` (the
  Windows/non-Windows split, mirroring `driver_for`'s own refusal message
  off Windows verbatim) and `cmd_doctor_fiori(timeout_secs)` (Stage 1 via
  `fiori_reachability`, then Stage 2 — a synthetic `flowproof_agent::FlowSpec`
  built as a YAML string via `FlowSpec::parse`, driven through
  `record_with_author(..., Author::Rules)` against `driver_for("web")` into
  a throwaway temp trace file, deleted after). URL/name values going into
  the hand-built YAML are escaped through `serde_yaml::to_string` first, so
  a URL with YAML-significant characters can't misparse.
- **`crates/flowproof-cli/src/agent_flow.rs`**: `cmd_doctor` renamed to
  `cmd_doctor_agent`, behaviour untouched.
- **`crates/flowproof-cli/src/lib.rs`**: `Doctor { agent: Option<String>,
  sap: bool, fiori: bool, timeout, prompt }` with `conflicts_with_all`
  covering every pair; dispatch is a `match (agent, sap, fiori)` with an
  explicit "specify one of" error for the all-`None`/`false` case and an
  `unreachable!()` for the combinations clap already rejects. 7 new tests:
  the three pairwise conflicts, the three single-flag-parses-fine cases, and
  `doctor` with none of the three parsing but `run_cli` returning
  `EXIT_ERROR` with a named reason.
- **`docs/getting-started.md`**: a new `### flowproof doctor --sap /
  --fiori: is any of this reachable?` subsection immediately after plan 1's
  `flowproof config` subsection — see Divergences below for why not
  `docs/agent-testing.md` as originally phased.
- **`docs/agent-testing.md`**: one cross-reference sentence in the existing
  `--agent` doctor prose, pointing at the getting-started section instead of
  duplicating SAP/Fiori content in a document about the agent boundary.

Verified: `cargo fmt --check`, `cargo build`/`cargo clippy --workspace
--all-targets -- -D warnings`/`cargo test --workspace` all green, **except**
`flowproof-python` excluded throughout — its pyo3/CPython linking fails on
this machine with or without any change from this plan (reproduced against
the bare post-merge tree before writing a line of this plan's code; a
pre-existing environment issue, not a regression). `flowproof-cli` (86 unit
+ all integration tests) and `flowproof-adapters` (134 tests, including the
new `fiori` module) both fully green. Also incidentally found, and left
alone as out of scope: `cargo clippy --target x86_64-pc-windows-msvc
--features agent` fails on an unrelated pre-existing lint in
`flowproof-driver/src/window.rs:180` (`unnecessary_lazy_evaluations`) —
untouched by this plan, reproducible on the bare merged tree.

## Divergences from this plan

- **Fiori doctor docs landed in `docs/getting-started.md`, not
  `docs/agent-testing.md`.** Phasing step 7 named agent-testing.md, written
  before actually re-reading that file: it is entirely about `app: agent`
  LLM-boundary testing, and a SAP/Fiori subsection there would be a topical
  mismatch for a reader who came for agent testing. The `flowproof config`
  subsection plan 1 already added to getting-started.md is the real match —
  same audience (SAP GUI/Fiori credential setup), same page. Landed there
  instead, with a one-line pointer left in agent-testing.md's own doctor
  section so a reader of the `--agent` doctor knows the SAP/Fiori one exists
  and where.
- **`fiori_reachability` lives in its own `fiori.rs` module**, not literally
  next to `agent_proxy.rs`/`vision.rs` as "next to its three existing `ureq`
  call sites" suggested. Reused their `ureq` idiom
  (`http_status_as_error(false)`, `Config::builder()`) exactly; put the code
  itself where a reader looking for Fiori-specific logic would look first.
  The plan itself flagged the name and location as "TBD" — this is that
  decision, not a reversal of one already made.
- **`doctor --sap`'s `observe()` never calls `start_sap_logon()`**, even
  though the plan's own prose lists "SAP Logon not running and needing a
  start attempt" among the states `connect()` tracks. Starting a process is
  an action with a side effect (a new SAP Logon window opens on the user's
  machine), not an observation, so a stricter reading than the plan's prose
  literally required won out: "SAP Logon is not reachable" is reported and
  `observe()` stops there rather than starting it to look further.
- **No `--login` flag exists on `Doctor` at all** (plan text mentions it as
  a "recommendation, not foreclosed" while still theoretically open before
  the open-questions pass). The team's answer while resolving open questions
  was more direct than "recommended": SAP already validates credentials on
  its own, so `doctor --sap` has no business submitting one, full stop. What
  shipped has no flag or code path that could ever authenticate for SAP —
  removing it later is a bigger change than adding it would have been, and
  that is deliberate.
- **Windows verification is genuinely partial, and the gap is worth being
  precise about.** `cargo check --target x86_64-pc-windows-msvc` DID verify
  `flowproof-adapters --features sap-com` (the actual COM-calling code in
  `observe()`) compiles for real on Windows — that target had to be added
  with `rustup target add` first, then it worked cleanly. It could NOT do
  the same for `flowproof-cli` itself (which is where `doctor.rs`'s
  `#[cfg(windows)]` block lives): `flowproof-cli` unconditionally enables
  the `agent`/`vision` features too, both of which pull `ureq` → `ring`, and
  `ring`'s C code fails to cross-compile from this macOS sandbox for lack of
  a Windows SDK (`fatal error: 'assert.h' file not found` — a `cc`/clang
  cross-compilation gap, not a Rust one). This is not new: the identical
  failure reproduces on the bare merged tree, before any code from this plan
  existed, for the exact same reason CHARTER's own comment in
  `flowproof-adapters/src/lib.rs` already names (`agent` pulling `ureq`
  makes a plain cross-`check` impossible; only `sap-com` alone survives it).
  So: `doctor.rs`'s Windows-only block is type-consistent with an
  already-verified API by careful reading, not by an independent compiler
  pass — a real, named gap, same category as the plan's own
  "real-system verification blocked" item, not a new one this
  implementation introduced.
