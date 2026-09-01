---
status: proposed
---
# Plan 4 — closing the loop on single-flow shareability

[Issue #534](https://github.com/automators-com/flowproof/issues/534):
"Running a single flow example shouldn't require a suite.yaml." Filed while
doing real-Windows verification of plan 1 (`flowproof config sap`/`fiori`) —
the concern was that handing someone a single `*.flow.yaml` and saying "run
`flowproof config fiori` then `flowproof run`" still had rough edges, even
though plan 1's whole point was to make credentials a personal-machine
default rather than a suite-level concern.

## What's already true, verified against the actual code

This plan started as an assumption that real engineering was missing. It
mostly isn't. `apply_suite_context` (`crates/flowproof-cli/src/lib.rs:1088`)
calls `config::seed_env()` **unconditionally, before** `SuiteManifest::discover`
runs and before the "no `suite.yaml` found" early return
(`lib.rs:1092-1096`) — the doc comment on that function already says why:
"a bare single-flow run with no suite at all still has something to fall
back on." That's not a gap this plan needs to close; it's plan 1's design
working as intended.

Confirmed empirically, not just by reading: copied `login-smoke.flow.yaml`
and its committed `login-smoke.trace.jsonl` into an isolated directory with
no `suite.yaml` anywhere above it, set only `SAP_USER`/`SAP_PASSWORD`/
`SAP_CLIENT`/`SAP_LANGUAGE`/`FIORI_BASE_URL` (dummy values — no live Fiori
access exists in this environment), ran `flowproof run login-smoke.flow.yaml`.
It resolved the URL, started driving the browser, and failed only on the
dummy hostname not existing (`net::ERR_NAME_NOT_RESOLVED`) — never on a
missing var, never on a missing suite. The credential half of the issue's
goal is already met for exactly the flow the issue names as "closest to
self-contained."

`examples/sap/*.flow.yaml` (`view-order`, `session-status`,
`stock-overview`, `create-order`, `standard-text`) confirm the same thing
for the other adapter: none of them sit next to a `suite.yaml` at all, and
all five use plain `SAP_*` names consistently. They already satisfy the
issue's goal today. This plan doesn't do further work there — the two real
gaps are both on the Fiori side.

## Gap 1: `login-smoke` isn't the only stale-naming exception anymore

Plan 1 renamed two of the three then-existing Fiori examples from `SAP_*` to
`FIORI_*` (`manage-info-records.flow.yaml`, `purchase-info-records-report.flow.yaml`),
deliberately leaving `login-smoke.flow.yaml` on `SAP_*` because it's the one
Fiori example with a committed trace (`login-smoke.trace.jsonl`) that stores
step text verbatim — renaming without re-recording would desync the flow
from its own cassette, and re-recording needs live Fiori access nobody
running this plan has.

A fourth example has landed since (`d037017`, "add a working
display-info-record-by-supplier example"):
`display-info-record-by-supplier.flow.yaml`. It's in the exact same
position — still `${SAP_USER}`/`${SAP_PASSWORD}` (never touched by plan 1,
since it didn't exist yet), and it has its own committed trace
(`display-info-record-by-supplier.trace.jsonl`) with the same
text-stored-verbatim property. Issue #534's own text names `login-smoke` as
"the one" exception; that's now stale.

**Decision: leave both as documented debt**, not renamed. Re-recording either
needs a live Fiori system this environment cannot reach; hand-renaming the
flow text without re-recording the trace would silently desync a cassette
from the spec it's supposed to prove, which is the wrong trade even though
it's not literally "modifying a committed cassette" (CHARTER invariant 8) —
it undermines the same property. A fallback that tries `FIORI_USER` then
falls back to `SAP_USER` was considered and rejected: it directly undoes
plan 1's "two independent profiles, not one shared identity" decision (a
process has one value for `SAP_USER`, not two, so an alias can't serve both
profiles independently without silently picking one).

What this plan actually does about it:
- Amend `plans/001-credential-config.md`'s "Divergence from this plan"
  section to name both flows, not just `login-smoke` — it's a living
  document per `plans/README.md`, and leaving it saying "one exception"
  when there are two is exactly the kind of stale prose CLAUDE.md calls a
  defect.
- Add a code comment to `display-info-record-by-supplier.flow.yaml`
  matching `login-smoke.flow.yaml`'s existing one, explaining the same
  trace-desync reasoning, so a reader hitting either file gets the same
  answer without cross-referencing a plan doc.
- Track the actual rename as a standing follow-up in both comments and this
  plan's Next section — blocked on live Fiori access, not on design.

This means someone handed `login-smoke.flow.yaml` or
`display-info-record-by-supplier.flow.yaml` alone genuinely needs **both**
`flowproof config sap` and `flowproof config fiori` to run it — surprising
for a flow that's `app: web`, since nothing about that app id suggests a
SAP-GUI-named config profile is also required. Gap 2 is what makes that
surprise legible instead of a bare unset-var error.

## Gap 2: the missing-var error doesn't say what to do

`MissingSecret` (`crates/flowproof-trace/src/secret.rs:16-19`) renders as
`secret ${VAR} is not set in the environment` — accurate, but silent on the
one thing a person handed a bare flow file actually needs: which
`flowproof config` command fixes it. `flowproof doctor --fiori` already
gives this exact class of guidance today (`crates/flowproof-cli/src/doctor.rs:75`,
`:140`) — but `doctor` is an opt-in preflight check, not something `run`
forces anyone through, so someone who skips straight to `flowproof run`
still hits the bare message.

**Where the fix actually lives, worked out from the real call graph, not
assumed:** `MissingSecret` does **not** bubble up as a typed error to a
single CLI-level call site. `resolve_refs` is called from ~20 separate
match sites deep inside `flowproof-replay` (`crates/flowproof-replay/src/lib.rs`,
e.g. line 550: `Err(e) => return Ok((Err(e.to_string()), Some(rung)))`),
and every one of them stringifies the error immediately, landing as free
text in `StepReport.detail: Option<String>` (`crates/flowproof-replay/src/report.rs:26`)
— there's no structured error-kind field to match on instead. Adding one
would touch the report schema across `flowproof-replay` and
`flowproof-trace` for a much bigger diff than this issue calls for.

**Decision: string-match the stable message, per step, in `cmd_run`'s
existing human-output rendering loop** (`crates/flowproof-cli/src/lib.rs`,
the `for step in &report.steps` block that already prints
`step.detail` for `Failed`/`Errored` steps). `secret.rs`'s
`#[error("secret ${{{var}}} is not set in the environment")]` is a
`thiserror`-derived, stable message contract, not incidental prose — the
same category of thing this codebase already string-matches on elsewhere
(e.g. the SAP driver's own specific auth-failure text,
`sap_com.rs:1149-1163`, per plan 1). When a failed/errored step's `detail`
matches that shape, append one line naming the concrete command:

- var starts with `SAP_` → suggest `flowproof config sap`
- var starts with `FIORI_` → suggest `flowproof config fiori`
- anything else (e.g. `${MATERIAL}`, `${SUPPLIER}`) → no suggestion; those
  are suite-minted data, not something `flowproof config` has ever
  addressed, and a wrong suggestion is worse than none.

The mapping is on the **variable name**, not `spec.app.id()` — deliberately.
An `app: web` flow (Fiori) can still need `flowproof config sap` (exactly
`login-smoke`'s and `display-info-record-by-supplier`'s situation from Gap
1); mapping off app id alone would silently produce the wrong suggestion for
precisely the two flows this plan is trying to make legible. Scoped to
human stdout only — `--json` output keeps `step.detail` as the literal
report text flowproof-replay actually produced, consistent with JSON being
the machine/report surface (CHARTER invariant 7) rather than a place the CLI
layers narrative onto.

## Testing

- A test proving the already-working mechanism stays working: a fixture
  directory with a flow referencing `${SAP_USER}`-style vars and no
  `suite.yaml` anywhere above it, asserting `apply_suite_context` seeds from
  a fixture config file and the run does not fail on suite discovery. This
  is the automated version of the manual check this plan already ran by
  hand — it should have been a test from plan 1, and its absence is why
  this needed re-verifying by hand here.
- A test for the new suggestion line: a step whose `detail` matches the
  `MissingSecret` shape for a `SAP_*` var renders with the `flowproof config
  sap` suggestion; a `FIORI_*` var renders with `flowproof config fiori`; an
  unrelated var (`${MATERIAL}`) renders with no suggestion appended.
- A test that `--json` output is unchanged by this — `step.detail` in the
  JSON payload stays exactly what `flowproof-replay` produced, no
  suggestion text injected.

## Docs

- `docs/getting-started.md`'s credentials section already documents
  `flowproof config` (landed with plan 1); add one line stating plainly
  that a single flow with no suite.yaml already works once `flowproof
  config` is set up — the thing this plan spent most of its time
  *confirming* rather than building, worth saying explicitly so the next
  reader doesn't re-litigate it.
- `plans/001-credential-config.md`'s "Divergence from this plan" section:
  amended per Gap 1 above.
- `plans/README.md`'s table: add this plan's row.

## Out of scope

- Re-recording `login-smoke` or `display-info-record-by-supplier` onto
  `FIORI_*` names — needs live Fiori access nobody running this plan has.
  Tracked as a standing follow-up, not this plan's work.
- Any change to `examples/sap/*` — already satisfies the issue's goal,
  confirmed above; no gap to close there.
- A structured `error_kind` field on `StepReport` — real long-term
  improvement, materially bigger than this issue, not bundled in here.
- Anything about suite-level data minting (`env_from`, `mint-test-data.sh`)
  for `manage-info-records`/`purchase-info-records-report`/
  `display-info-record-by-supplier`'s `${MATERIAL}`/`${SUPPLIER}`/etc. The
  issue itself frames this as inherently suite-shaped and not something a
  credentials file can or should solve; nothing found while researching
  this plan contradicts that.

## Next

- [ ] Implement the per-step suggestion in `cmd_run`'s rendering loop,
  `crates/flowproof-cli/src/lib.rs`.
- [ ] Add the fixture test proving suite-less single-flow credential
  resolution (codifying the manual check this plan already ran).
- [ ] Add the suggestion-line tests (SAP_*, FIORI_*, unrelated var, JSON
  untouched).
- [ ] Add the matching code comment to
  `display-info-record-by-supplier.flow.yaml`.
- [ ] Amend `plans/001-credential-config.md` and `plans/README.md` as above.
- [ ] `docs/getting-started.md` one-liner.
