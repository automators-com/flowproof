---
status: done
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

## Post-plan refinement: business data without `suite.yaml`

After this plan landed, the remaining shareability gap is clearer:
`flowproof config` handles credentials and connection defaults, but it does
not and should not handle arbitrary business data such as `${MATERIAL}`,
`${SUPPLIER}`, `${PLANT}`, `${CUSTOMER}`, `${ORDER_ID}`, or any other
document-specific input a test case needs. Those values are why the current
examples still lean on `suite.yaml` plus `env_from`/`mint-test-data.sh` for
some flows.

The direction to carry into the next plan: introduce a values-file layer for
business data, keeping secrets in `flowproof config` or the caller's secret
environment. This fits the real user workflow better than requiring a
person to hand-type many `--var KEY=value` flags, because these users start
from a test-case document. `author-from-doc` can read that document, extract
the business inputs it needs, generate placeholders in the flow, and write
the sibling values file automatically.

Proposed artifact shape:

```text
display-info-record-by-supplier.flow.yaml
display-info-record-by-supplier.trace.jsonl
display-info-record-by-supplier.values.yaml
```

The flow keeps reusable placeholders:

```yaml
steps:
  - Type ${SUPPLIER} into the "Supplier" field
  - Type ${MATERIAL} into the "Material" field
```

The sibling values file carries non-secret document data:

```yaml
SUPPLIER: "45000031"
MATERIAL: "M-10092"
PLANT: "1000"
```

Default discovery should mirror the existing trace convention and resolve
relative to the flow file, not the current working directory:

- `x.flow.yaml` → `x.trace.jsonl`
- `x.flow.yaml` → `x.values.yaml`

So `flowproof run /path/to/x.flow.yaml` should look for
`/path/to/x.values.yaml` by default. Moving the three sibling files together
keeps the test case runnable from any directory.

Explicit overrides still matter for technical users and CI:

```bash
flowproof run x.flow.yaml --vars qa.values.yaml
flowproof run x.flow.yaml --vars qa.values.yaml --var MATERIAL=M-99999
```

Likely precedence, still to be finalized in the next plan:

1. `--var KEY=value`
2. `--vars path.yaml`
3. sibling `<flow-stem>.values.yaml`
4. suite `env` / `env_from`
5. `flowproof config` for credential-profile names
6. ambient shell env, subject to the current fill-gaps-only semantics

The product workflow this enables:

1. User starts with the same test-case document they already follow manually.
2. `flowproof author-from-doc` extracts both the screen steps and the
   business inputs.
3. Flowproof writes `*.flow.yaml`, `*.trace.jsonl`, and `*.values.yaml`.
4. User runs `flowproof config sap` or `flowproof config fiori` once for
   credentials.
5. User replays with `flowproof run x.flow.yaml`; the sibling values file is
   loaded automatically.

Open design questions for the follow-up plan:

- Should `author-from-doc` always generate a values file when it extracts
  business data, or only when a value appears more than once / is marked as
  case data?
- Should `run` print a short note when it auto-loads `x.values.yaml`, or stay
  quiet unless a referenced var is missing?
- Should values files be allowed to contain only strings, or should JSON/YAML
  scalars be accepted and converted at resolution time?
- Should flow-level `env_from` or `values_from` exist later for generated
  data, replacing suite-level `mint-test-data.sh` for single-flow examples?

## Next

- [x] Implement the per-step suggestion in `cmd_run`'s rendering loop,
  `crates/flowproof-cli/src/lib.rs`.
- [x] Add the fixture test proving suite-less single-flow credential
  resolution (codifying the manual check this plan already ran).
- [x] Add the suggestion-line tests (SAP_*, FIORI_*, unrelated var, JSON
  untouched).
- [x] Add the matching code comment to
  `display-info-record-by-supplier.flow.yaml`.
- [x] Amend `plans/001-credential-config.md` and verify `plans/README.md`'s
  plan 4 row. Implementation note: the row already existed, so no table edit
  was needed.
- [x] `docs/getting-started.md` one-liner.

## What landed

- `cmd_run`'s human step rendering now tells users to run
  `flowproof config sap` for `SAP_*` missing-secret details, and the
  equivalent `flowproof config fiori` line for `FIORI_*`. Other missing
  vars, such as suite-minted `${MATERIAL}`, stay untouched.
- `--json` still emits the original `StepReport.detail` without the human
  suggestion text.
- `config_seed_e2e` now records and runs a suite-less single `app: api` flow
  whose `${SAP_USER}` value comes only from a fixture `flowproof config` file.
- `display-info-record-by-supplier.flow.yaml`,
  `plans/001-credential-config.md`, and `docs/getting-started.md` now carry
  the plan's documentation updates.

Verified with:

- `cargo fmt`
- `CARGO_INCREMENTAL=0 cargo test -p flowproof-cli --lib secret_detail -- --nocapture`
- `CARGO_INCREMENTAL=0 cargo test -p flowproof-cli --test config_seed_e2e -- --nocapture --test-threads=1`
- `CARGO_INCREMENTAL=0 cargo test -p flowproof-cli --test api_pipeline missing_secret -- --nocapture --test-threads=1`

The first broad filtered cargo run attempted to link every `flowproof-cli`
test binary and failed with `No space left on device`; after `cargo clean`,
the target-scoped runs above passed. The `config_seed_e2e` target needed to
run outside the sandbox because its local 127.0.0.1 server fixture was
blocked from binding inside the sandbox.
