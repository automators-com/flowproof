---
status: done
---
# Plan 9 — Docs/product alignment audit: closing the gap between what ships and what's written down

Triggered by a direct complaint, not a filed issue: `flowproof config` (plans
1, 3, 8) replaced hand-exported `SAP_USER`/`ANTHROPIC_API_KEY`-style env vars
as the recommended way to hold credentials, but the top-level README quick
start still shows the old pattern. The ask was to look at the product as it
actually behaves today, look at the docs as they actually read today, and
write down every place the two have drifted apart — not just the one example
that prompted this.

## What's actually true about the docs today

Before listing gaps, it's worth being precise about how good the coverage
already is, because the honest picture is "mostly current, with specific
holes" — not "undocumented." `docs/getting-started.md`'s
[`flowproof config`](../docs/getting-started.md#flowproof-config-credentials-without-hand-exporting-env-vars)
section is comprehensive and correct against the code as of this audit:
`sap`/`fiori`/`ai`/`show`/`path`/`skill`, every flag on each, the seed-env
precedence (`FLOWPROOF_AI_API_KEY` first, then the provider-specific
compatibility variable), and the file location per OS all match
`crates/flowproof-cli/src/config.rs` and `crates/flowproof-cli/src/lib.rs`'s
`ConfigAction` enum exactly. `docs/design.md` and `CHANGELOG.md`'s 0.21.0
entry are likewise accurate. This was verified by reading the clap
`Command`/`ConfigAction` definitions directly
(`crates/flowproof-cli/src/lib.rs:139-466`) and diffing them against every
doc page, not by assuming the docs are stale because one example is.

## The mechanism that explains why a gap like this can exist at all

`crates/flowproof-cli/tests/documented_flags.rs` already is a doc-vs-code
ratchet: `every_visible_cli_flag_is_mentioned_in_the_docs` and
`every_visible_subcommand_is_mentioned_in_the_docs` fail CI if a non-hidden
flag or subcommand string doesn't appear anywhere in `docs/*.md` +
`README.md`. That's exactly why nothing in this audit turned up a command or
top-level flag that's *absent* from the docs — the ratchet already forbids
that class of gap, and reading the whole CLI surface against the whole docs
corpus confirmed it holds.

But the ratchet's own doc comment says what it doesn't claim: "this catches
absent, not badly explained." Concretely, it has four blind spots, and the
README gap below lives in the first one:

1. **Mentioned once, anywhere, is enough.** It doesn't check that the
   *first* example a reader hits uses the current recommended path — only
   that the string appears *somewhere* in the corpus. README.md mentions
   `flowproof config` at line 279 (the "Reach" section), which is enough to
   satisfy the test, while the Quick Start section above it (line ~163) still
   shows the pattern `flowproof config` was written to replace.
2. **It only walks one level of subcommand.** `cli.get_subcommands()`
   enumerates `config`, `record`, `run`, `capture`, `audit`, `doctor`,
   `author-from-doc`, `heal` — but never descends into `ConfigAction`'s own
   six actions (`sap`, `fiori`, `ai`, `show`, `path`, `skill`) or their
   flags. Today those are all documented anyway (verified by hand above),
   but nothing enforces that going forward; a new flag added under `config
   ai`, say, could ship silently.
3. **The same flag name on two commands shares one satisfied/unsatisfied
   verdict.** The check is "does the string `--author` appear anywhere in
   the corpus," not "is `heal`'s `--author` explained." `record` and `run`
   both document `--author` at length; `heal` has the identical flag
   (`lib.rs:462-464`) and the test passes without a single doc page ever
   saying `heal --author` exists, because the substring is already
   satisfied by its siblings. Confirmed by reading `docs/getting-started.md`'s
   entire "Healing a stale trace" section (lines 1346-1372): it documents
   `--apply` and `--json` precisely (down to exit codes) and never mentions
   `--author` once. Same root cause as blind spot 1, one level more subtle:
   presence-anywhere doesn't imply presence-in-context even at the top
   level, not just under nested subcommands.
4. **A doc page can be silent on a topic without contradicting anything**,
   and the ratchet has no opinion on silence — only on a flag string being
   physically absent from the whole corpus. `docs/adopting.md` (gap 2 below)
   is this case: it never says `export ANTHROPIC_API_KEY`, so it isn't
   *wrong*, but it also never says `flowproof config ai`, so the reader it's
   aimed at — someone onboarding a whole new repo — gets no pointer to the
   feature that exists specifically to save them from the thing the doc does
   tell them to do ("Recording needs [an API key], once per flow").

None of this is a criticism of the ratchet — a mechanical presence check is
the right size for CI to enforce; "is this the recommended path" is a
judgment call, which is what this audit (a human/agent pass, not a test) is
for.

## Confirmed gap 1: README Quick Start still shows the pre-`config` pattern

`README.md`'s top-level "Quick start" section (the second, shorter quickstart
in the file — `docs/getting-started.md`'s is the "complete version," per
README's own line pointing there) reads:

```bash
npm install openai
export ANTHROPIC_API_KEY=...        # or OPENAI_API_KEY
npx flowproof record examples/agent-demo/weather-node.flow.yaml
```

`docs/getting-started.md`'s quickstart — the doc the README explicitly calls
the complete version of this same section — was already updated (commit
`b325401`, "docs: sync product docs with flowproof config, doctor, and ai
authoring") to:

```bash
npm install openai
npx flowproof config ai             # stores the model API key with a masked prompt
npx flowproof record examples/agent-demo/weather-node.flow.yaml
```

So the fix already exists, one file over — this is a sync miss, not new
design work. `b325401` updated `docs/getting-started.md` and (per its
CHANGELOG entry) `doctor`/`config ai` prose generally, but didn't touch
`README.md`'s own copy of the same three-line snippet.

**Why this is the highest-priority item in this audit and not a minor
wording nit:** the README is the first thing anyone reads — before
`getting-started.md`, before `npm install`. A brand-new user who copies these
three lines verbatim gets a working result (env-var export is still a valid
fallback path, per `apply_suite_context`'s fill-gaps-only precedence), so
this doesn't break anyone — but it teaches the exact habit `flowproof config`
was built to retire, and it's the reason the person who filed this complaint
"can't see [config] in the quick start."

**Fix**: replace README's Quick Start snippet with the same
`npx flowproof config ai` line `getting-started.md` already uses. Keep it a
one-line swap, not a rewrite — README's quickstart is deliberately terser
than getting-started.md's ("the complete version of this section" language
already tells the reader where to go for the full explanation), so it should
gain the one line that matters, not the whole credentials section.

## Confirmed gap 2: `docs/adopting.md` never mentions `flowproof config`

`docs/adopting.md` — explicitly "written to be handed to a coding agent" for
onboarding flowproof into an existing repo — has a "Step 0: install" section
that says:

> Replay needs no API key. Recording needs one, once per flow.

...and stops there. No mention of `flowproof config ai`, no link to
`getting-started.md`'s credentials section, no mention of `export
ANTHROPIC_API_KEY` either — it's silent, not wrong, which is why the doc-flag
ratchet has nothing to say about it (gap-type 4 above). But this is the exact
reader — someone bringing flowproof into a new codebase for the first time —
who most needs the pointer, and it's a two-sentence fix:

> Replay needs no API key. Recording needs one, once per flow — `flowproof
> config ai` stores it once per machine instead of exporting it in every
> shell (see [getting-started.md](getting-started.md#flowproof-config-credentials-without-hand-exporting-env-vars)).

## Confirmed gap 3: the config file's Windows permission gap isn't in the threat model

`config.rs` sets the config file to `0600` on Unix at write time and says so
in its own code comment: "No Windows-side equivalent yet — a stated gap, not
a silent one" (`crates/flowproof-cli/src/config.rs:266-271`). That file can
hold a plaintext SAP password, a Fiori password, and an AI provider API key.
`docs/threat-model.md` already treats "customer SAP GUI / Fiori credentials"
as a named asset and has a whole section on the model-proxy credential
boundary — but it never mentions `flowproof config`'s file at all (confirmed:
zero hits for `config.yaml` or `flowproof config` in that doc). A doc whose
job is to say where credentials sit exposed should say that this specific
at-rest store is unhardened on Windows, since the code already knows it and
says so to whoever reads the source.

**Fix**: add one entry to `docs/threat-model.md`'s asset/gap inventory
naming the config file, its Unix permission, and the stated Windows gap —
matching the doc's existing "Not covered — real gap" pattern used elsewhere
in that file (e.g. the `cargo-deny` gap at line 304).

## Smaller precision gaps (lower priority, bundled rather than sectioned)

Found in the same pass, all real but each small enough not to warrant its
own section:

- **`doctor --timeout` also bounds `--fiori`'s login-attempt wait**
  (`doctor.rs:73`, `lib.rs:423-425`), not just `--agent`'s. The doctor
  section of `docs/getting-started.md` (lines 1308-1344) never mentions
  `--timeout` in the `--fiori` context — only `docs/agent-testing.md`
  discusses `--timeout`, and only for `--agent`.
- **`run --author` only has an effect when combined with `--record-missing`**
  (`lib.rs:305-306`'s doc comment: "used only when `--record-missing`
  records a suite flow") — a bare `flowproof run --author llm` on a spec
  that already has a trace does nothing with that flag. The docs discuss
  `--author` at length for `record` but don't call out this scoping for
  `run`'s copy of the same flag.
- **`config ai`'s mutual-exclusion checks** (`--clear-api-key` rejects
  `--api-key`, `--clear-model` rejects `--model`; `config.rs:528-533`)
  aren't spelled out in prose, though each flag is individually documented
  and `--help` would show the conflict. Lowest priority of the three —
  worth a mention only if someone is already editing that section for
  another reason.

These are candidates for the same PR as gaps 1–3 if the diff stays small, or
a fast follow if it doesn't — none of them is the kind of thing a user hits
on the golden path.

## Not gaps — checked and confirmed correct, so they don't get re-litigated later

- **`examples/sap/*.flow.yaml` and `examples/fiori/*.flow.yaml` still
  reference `${SAP_USER}`/`${FIORI_USER}` etc. directly.** This is correct,
  not stale: `flowproof config` seeds environment variables as a
  fill-gaps-only fallback (`apply_suite_context`, `lib.rs`) — it doesn't
  change what `${VAR}` resolution reads from, so flows referencing the same
  env var names by hand are exactly right. `examples/fiori/manage-info-records.flow.yaml`
  and `examples/release-notes/README.md` both already say this explicitly
  ("credentials and connection defaults live in `flowproof config` — never
  in this repo, never in a `.env` file").
- **`FLOWPROOF_AI_BASE_URL` / `FLOWPROOF_AI_PROVIDER` are documented as raw
  env vars, not as a `flowproof config ai` field.** Also correct:
  `config::AiProfile` (`config.rs`) only stores `provider`, `api_key`, and
  `model` — there's no `base_url` field on the AI profile (unlike
  `FioriProfile`, which does have one) — so a vLLM/openai-compatible user is
  meant to export `FLOWPROOF_AI_BASE_URL` directly, and `getting-started.md`
  / `design.md` already say exactly that.
- **Every top-level command and flag** (`config`, `record`, `run`, `capture`,
  `audit`, `doctor` and its `--agent`/`--sap`/`--fiori`/`--ai`/`--timeout`/
  `--prompt`, `author-from-doc`, `heal`) **is mentioned somewhere in the
  docs.** Confirmed by reading `Command`/`ConfigAction` in `lib.rs` end to
  end against the docs corpus, which is the same check
  `documented_flags.rs` runs in CI — nothing here needed a code archaeology
  the ratchet wouldn't already have caught.
- **`docs/explore-mode.md` describes a whole `flowproof explore` command —
  `--author-prompts`, `--promote`, `--samples`, `--budget-requests` — that
  has no implementation anywhere in the Rust source.** Read in isolation
  that looks like the worst kind of drift (a fully-specified command that
  doesn't exist), but line 3 of the doc says `Status: proposed, nothing
  built`, opened from issue #281. This is what correctly-labeled forward-
  looking design prose looks like, and it's the right pattern for
  `plans/007-assisted-production-authoring.md`'s "draft" work to follow too
  — noted here so it isn't mistaken for a gap on a future pass.
- **`mcp-stdio`, `FLOWPROOF_MCP_DIR`, `FLOWPROOF_MCP_MODE`** are undocumented,
  correctly: `mcp-stdio` is `#[command(hide = true)]` and spawned by
  flowproof itself, never typed by a user, and the two env vars only exist
  to hand it context. `documented_flags.rs` already exempts hidden
  subcommands from the ratchet for exactly this reason.

## Fix

Small and mechanical — this is a docs-only PR, not a feature:

1. **`README.md`**: swap the Quick Start snippet's `export
   ANTHROPIC_API_KEY=... # or OPENAI_API_KEY` line for `npx flowproof config
   ai             # stores the model API key with a masked prompt`, matching
   `getting-started.md`'s wording exactly so the two quickstarts stay in
   lockstep.
2. **`docs/adopting.md`**: add the one-sentence pointer to `flowproof config
   ai` in Step 0, linking to `getting-started.md`'s credentials anchor.
3. **`docs/threat-model.md`**: add the config-file asset entry naming the
   Unix `0600` permission and the stated Windows gap (gap 3 above).
4. **`docs/getting-started.md`**: add one line to the "Healing a stale
   trace" section documenting `heal --author`, and one line to the
   `--fiori` doctor example noting `--timeout` applies there too (bundled
   with the smaller precision gaps above if they fit the same small PR).
5. **`CHANGELOG.md`**: no entry — this is a docs correction to something
   already shipped and already changelogged in 0.21.0, not new behavior.

## Strengthening the ratchet (separate, smaller, optional)

Blind spot 2 above (`documented_flags.rs` not recursing into `config`'s own
subcommands) is a real hole in the mechanism, independent of whether today's
docs happen to cover it. Two options, not decided here:

- **Recurse one more level**: for every subcommand that itself has
  subcommands (today, only `config`), walk those too, with the same
  hide-flag exemption. Small, additive change to an existing test — low
  risk, and it converts a manual "I checked by hand" claim (gap-hunting
  section above) into something CI enforces the same way it already does for
  the top level.
- **Leave it as a documented limitation** and rely on this kind of periodic
  audit to catch drift under `config`'s subcommands, on the reasoning that
  `config` is the only nested command today and the blast radius of a silent
  gap there is small.

Recommend the first option as a fast follow, but it's separable from the
docs fix above and doesn't block it.

Note this only closes blind spot 2. Blind spot 3 (`heal --author` riding on
`record`/`run`'s coverage of the same flag name) needs a different fix:
checking presence *near the owning subcommand's name* rather than anywhere
in the corpus, which is a meaningfully bigger change to how the test greps
and has a real false-positive risk (a doc page can legitimately explain one
command's flag while cross-referencing another by name). Left as a noted
limitation rather than a proposed test change here.

## What this plan deliberately doesn't do

- **It doesn't try to mechanize "is this the FIRST/recommended example"** —
  gap-type 1 above. That's a judgment call a human or an agent doing exactly
  this kind of pass has to make; no test was designed against it, and this
  plan isn't proposing one. Silence-detection (gap-type 4) is even harder to
  automate without a lot of false positives (plenty of docs are legitimately
  silent on a topic outside their scope). Both stay a periodic-audit
  problem, not a CI problem.
- **It doesn't re-litigate whether `flowproof config` is the right design.**
  Plans 1, 3, and 8 already made and shipped that decision; this plan is
  purely about the docs catching up to it.
- **It doesn't audit every doc page line-by-line against every code path.**
  The scope here was credential/API-key handling specifically, because
  that's what prompted the complaint, plus the command-surface-wide check
  `documented_flags.rs` already automates. A wider "audit everything" pass
  is explicitly out of scope — see Open questions.

## Open questions

- **Should this become a recurring pass rather than a one-time fix?** The
  README/adopting.md drift happened because a sync commit (`b325401`) landed
  correctly on one file and missed a sibling with the same content. That's a
  process gap as much as a docs gap — nothing currently prompts "you just
  changed the recommended credential flow; did you check README's own copy
  of the quickstart?" Worth deciding whether a future `flowproof config`-style
  change gets a checklist item, or whether occasional audits like this one
  are the intended cadence.
- **Plan numbering**: this is filed as plan 9 because 7
  (`007-assisted-production-authoring.md`, draft, on
  `docs/plan-assisted-production-authoring`) and 8
  (`008-ai-authoring-config.md`, done, already on `main`) are both claimed on
  branches that hadn't merged as of this audit — `main` itself is missing
  006 and 007 entirely. Renumber if either lands first and creates a
  collision; nothing in this plan's content depends on the number.
- **Should `documented_flags.rs` recurse into nested subcommands now, as
  part of this PR, or as a separate follow-up?** Left separate — not done in
  this pass; see "What landed."

## What landed

All of the "Fix" and "Smaller precision gaps" items, none of the ratchet
strengthening (left as a follow-up, per the open question above):

- **`README.md`**: Quick Start's `export ANTHROPIC_API_KEY=... # or
  OPENAI_API_KEY` line replaced with `npx flowproof config ai`, wording
  matched verbatim to `docs/getting-started.md`'s copy.
- **`docs/adopting.md`**: Step 0 now names `flowproof config ai` and links
  to `getting-started.md`'s credentials anchor, instead of just saying an
  API key is needed with no pointer to how to store it.
- **`docs/threat-model.md`**: new "`flowproof config` file at rest"
  subsection under Trust boundaries, stating the Unix `0600` guarantee and
  the stated Windows gap; a matching 8th entry added to Known limitations.
- **`docs/getting-started.md`**: four additions — `heal --author` documented
  in "Healing a stale trace"; `--record-missing`'s bullet in the suite
  section now says `run --author` only matters combined with it; `--fiori`'s
  doctor section now states `--timeout` bounds its login wait (`--sap`
  ignores it); `config ai`'s section now states the `--clear-api-key`/
  `--api-key` and `--clear-model`/`--model` mutual-exclusion.
- **Not touched**: `documented_flags.rs` itself (recursing into `config`'s
  own subcommands, and the same-flag-shared-across-siblings blind spot) —
  both remain open per the questions above, deliberately left as a smaller,
  separate, test-code change rather than folded into a docs PR.

Verified: `cargo test -p flowproof-cli --test documented_flags` (both tests
still pass — this was a strictly-additive doc change) and `cargo fmt
--check`. No Rust source changed, so `cargo clippy`/`cargo test --workspace`
were not re-run in full; nothing in this PR touches code paths they cover.
