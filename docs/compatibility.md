# Compatibility, support and deprecation contract

**Status: agreed, pending the follow-up work it names.** This answers
[#378](https://github.com/automators-com/flowproof/issues/378), a
`needs-human` issue: flowproof's current status is "early, interfaces may
still change between minor versions" (README), and this document replaces
that hedge with a written contract. The decisions below were made
deliberately, not derived from the code — where a section describes
something the code already does, it says so; where it commits to a policy,
that policy is new as of this document.

This document is the v1 gate for the compatibility contract itself. It is
**not** a gate on the five related readiness issues linked in §8 — those
track unfinished surfaces that ship v1 labeled experimental, not blockers
on this doc.

Once merged, this supersedes the "interfaces may change" line in the
README's Status section, and `flowproof --version` / the docs site should
link here.

## Why this exists

flowproof's pitch is "record once, replay forever" (docs/comparison.md).
That promise is about the *recording* — a `.trace.jsonl` file — surviving
time. It said nothing about what "forever" means when the engine that
reads the trace is several versions newer than the one that wrote it.
Enterprises treating a recording as durable evidence need that gap closed.
This document defines, surface by surface, what changes are safe across a
version bump and what is not.

## 1. Support matrix

Lists only what CI actually verifies today — not a broader claim the CI
doesn't back yet (see §10 for why, and what widening this later looks
like).

| Surface | Support |
|---|---|
| OS | Linux (`ubuntu-latest`) and Windows (`windows-latest`), verified on every push. macOS builds (stub backend) but is not otherwise exercised — **not supported**, best-effort only |
| Architecture | x86_64, CI-verified. `npm`'s optional deps also publish `darwin-arm64` — **not CI-verified**, best-effort only |
| Python | Declared floor `>=3.9` (`sdk/python/pyproject.toml`, enforced by packaging). CI-verified against **3.12** only (`.github/workflows/publish.yml`). Versions between the floor and 3.12 are not excluded, but are unverified |
| Node | Declared floor `>=18` (`sdk/js/package.json`, enforced by `engines`). CI-verified against **20 and 22** (`npx-smoke.yml`, `ci.yml`, `publish-npm.yml`). Versions between the floor and 20 are not excluded, but are unverified |
| Browser | Real Chromium via CDP, Linux |
| SAP GUI | **Not version-pinned.** Real SAP GUI Scripting runs nightly on a self-hosted runner ([#32](https://github.com/automators-com/flowproof/issues/32)) against whatever version is installed there; no specific version is certified. This is the honest current state, not a target — narrowing it to a certified version is follow-up work, not blocking this document |

## 2. Stable vs. experimental at v1

**"Stable" means covered by a deprecation window (§5), not frozen
forever.** Nothing in this project has enough real-world mileage yet to
promise it will never change; a deprecation window is the promise that a
consumer sees a change coming before it lands.

**Stable at v1** (real and tested in CI today):
- record→replay spine (`flowproof record`, `flowproof run`)
- trace format v1 (`docs/trace-format.md` — already marked "shipped")
- the `web`, `sap`, `vision`, `api` adapters and the desktop (UIA) adapter
- security-control surface (`flowproof audit`, `assert_no_tool_call`,
  `assert_no_egress`)
- CLI exit codes (0 pass / 1 test failure / 2 error — `crates/flowproof-cli/src/lib.rs`)
- the Python API and MCP server (bundled in the one wheel)

**Explicitly experimental, not covered by any v1 guarantee — and not a v1
blocker (see §8):**
- `agent.url` services and the MCP boundary over streamable HTTP (README
  says "thinner coverage" today)
- multi-turn agent conversations — a v1 agent flow is one turn, not a
  conversation ([#375](https://github.com/automators-com/flowproof/issues/375))
- egress containment outside Linux (macOS, Windows, kernels <5.6 report
  "not contained" rather than enforcing —
  [#303](https://github.com/automators-com/flowproof/issues/303))
- the `v3.4` server-initiated MCP REQUEST slice (`id`/`answer` fields
  reserved in the trace schema but inert — `docs/trace-format.md`)

## 3. Backward/forward compatibility

What the trace format already does (`docs/trace-format.md`):
- every trace's header line carries `format` and `version`; a reader
  **must reject** a file with an unrecognized `format` or unsupported
  `version` rather than guess
- new fields are additive and skipped when empty, so old traces stay
  byte-identical when read by a newer engine that doesn't use the new field
  (e.g. the MCP `mcp` lane, the reserved `id`/`answer` fields)

Committed policy:

- A trace written by engine version `N` is readable by every engine `>= N`
  within the same trace format major version (currently `1`). A trace
  format major bump (`format` version 2) is the only sanctioned breaking
  change, and ships with the `flowproof migrate` path described in §4.
- `.flow.yaml` specs follow the same rule — a spec written against
  flowproof `N.x` parses on every `flowproof >= N.x` until the next
  spec-schema major version.
- Run records (`result.json`, JUnit, HTML, audit output) are an *output*
  format, not an input one, so they only need forward stability within a
  major version: a script parsing `result.json` from version `N` should
  not need to change for `N+1` unless the major version changes.
- Cassettes (recorded model/tool responses inside `app: agent` traces)
  follow the trace format rule above — they are trace content, not a
  separate format.

## 4. Migration behavior

- **Newer engine reads an older trace within the same format major
  version:** replays cleanly, applying additive defaults for fields the
  old trace never wrote.
- **A trace predates a breaking format-major bump:** `flowproof run`
  refuses outright, with a named error pointing at `flowproof migrate`.
  It does **not** attempt a silent best-effort read — a replay that
  quietly reinterprets old evidence is worse than one that stops and says
  so.
- **Older engine reads a newer trace:** refuses, citing the unsupported
  `version` field. This already happens per the "must reject" rule in §3;
  §7's fixtures are what makes this refusal (message and exit code)
  something a test actually pins instead of an implied behavior.
- **`flowproof migrate` does not exist yet.** Format version 2 hasn't
  shipped, so there is nothing to migrate from today. This document
  commits to the refuse-then-migrate shape; building the command is
  tracked against whichever future PR bumps the format major version, not
  against this document.

## 5. Semantic versioning and deprecation windows

flowproof is pre-1.0 (`0.19.0`); `Cargo.toml`, the Python wheel, the npm
package, and the platform packages all move together (`versions agree` CI
job, six file locations checked in `.github/workflows/ci.yml`). The rules
below take effect at 1.0.0.

- **MAJOR:** any change to a surface marked stable in §2 that isn't
  backward-compatible per §3 — trace format major bump, a removed CLI
  flag, a Python API signature change, an MCP tool renamed or removed.
- **MINOR:** new capability, new optional trace field, new CLI subcommand.
- **PATCH:** bug fixes with no interface change.
- **Deprecation window: one full MINOR release cycle, minimum.** A
  deprecated CLI flag, Python function, or MCP tool ships at least one
  release emitting a deprecation warning before it can be removed, and
  removal only happens in the next MAJOR after that. Measured in release
  count, not calendar time — flowproof's release cadence isn't fixed
  enough yet to make a calendar promise meaningful.

## 6. Release lines and security backports

**Policy: latest release line, plus one prior line, get security
backports.** This is explicitly **contingent on
[#379](https://github.com/automators-com/flowproof/issues/379)**
("remove single-person release and security-response dependencies before
v1"): a two-line backport promise is only real if more than one person can
execute it. Until #379 lands, treat backports as best-effort, not a
committed SLA — publishing the target now is what makes #379 a tracked
prerequisite instead of a vague aspiration.

How this interacts with [#376](https://github.com/automators-com/flowproof/issues/376)'s
security reporting policy and threat model: #376 should define the
disclosure timeline; this backport window needs to be at least as long as
that timeline, or a reported vulnerability could outlive the window meant
to fix it. Whoever writes #376 should check the number here still holds.

## 7. Compatibility fixtures in CI

Pinned now, pre-1.0 — the trace format itself is already "shipped" per
`docs/trace-format.md`, so it's the thing under test, not the workspace
version number. Waiting for 1.0.0 risks a compat break landing before the
ratchet that would catch it exists.

Two releases selected from the CHANGELOG, each representing a genuine
additive trace-format change (not just any two arbitrary old versions):

- **0.4.0** — introduced the agent-boundary trace shape and the additive
  `mcp` lane (stdio/streamable-HTTP servers, the egress audit lane). The
  oldest release whose traces have a materially different shape from
  today's.
- **0.14.0** — introduced the additive `apps` header map (multi-surface
  attribution: which step ran on which surface). The most recent release
  that added a new top-level additive field, making it a good "did the
  engine stay silently readable" checkpoint distinct from 0.4.0's.

Shape of the CI job, consistent with how the rest of the suite works
(committed traces replay for free, per CLAUDE.md):

- commit one `.trace.jsonl` recorded at each pinned version, unmodified
  (traces are human-only to edit — CLAUDE.md);
- a CI job runs current `flowproof run` against each fixture and asserts
  it still passes, so a silent backward-compat break fails the `adversary`
  ratchet instead of shipping;
- a second job runs a synthetic trace with a `version` newer than
  currently supported, and asserts the §4 refusal fires with the right
  message and exit code — the test that currently doesn't exist for the
  "must reject" rule.

This CI job is follow-up engineering work this document identifies but
does not itself implement.

## 8. v1 exit criteria

**v1 means this compatibility contract is written, honest, and enforced —
not that every feature is finished.** The experimental/stable split in §2
exists precisely so an unfinished surface can ship v1 correctly labeled
instead of blocking the release. None of the issues below gate v1; they
are tracked separately and ship (or don't) on their own timeline:

- live-SAP GUI Scripting attach verified outside the fake engine on a
  licensed host — [#479](https://github.com/automators-com/flowproof/issues/479)
  (already labeled experimental via §2's Linux-only/SAP caveats where
  applicable)
- egress containment beyond Linux-only —
  [#303](https://github.com/automators-com/flowproof/issues/303)
  (already labeled experimental in §2)
- multi-turn agent conversations —
  [#375](https://github.com/automators-com/flowproof/issues/375)
  (already labeled experimental in §2; v1 ships single-turn)
- security reporting policy and threat model —
  [#376](https://github.com/automators-com/flowproof/issues/376)
  (feeds §6's backport window, doesn't block it)
- release/security-response process no longer single-person —
  [#379](https://github.com/automators-com/flowproof/issues/379)
  (blocks §6's backport promise specifically, not v1 as a whole)

What **does** gate v1, because it's this document's own commitment:
- [ ] this document merged and linked from the README Status section
- [ ] §7's compatibility fixtures landed and green in CI
- [ ] §4's refusal path (unsupported trace version → named error, correct
      exit code) has a test pinning it

## 9. Support matrix and CI breadth

§1 publishes a narrower matrix than the floors already declared in
packaging (Python `>=3.9`, Node `>=18`) because CI only verifies single
pinned versions today. Widening CI to matrix-test the full declared range
is real, separate engineering work (multiple Python/Node versions × OSes)
and is not a prerequisite for landing this document — the matrix in §1 is
allowed to be narrow and honest rather than wide and unverified.

## 10. "Replay forever" vs. the actual guarantee

The phrase, from `docs/comparison.md`: *"Record once with a model in the
loop; replay forever with zero model calls."* Read plainly, "forever" is a
claim about *this* engine version replaying *this* trace deterministically
— true and tested today. It is not, and should not be read as, a claim
that `flowproof run` on next year's release replays a trace from today
without change.

**Decision: keep the phrase, add a qualifier.** The zero-model-calls claim
is real and worth keeping memorable. Wherever it appears in marketing copy
(`docs/comparison.md`, README), append a qualifier along the lines of
*"forever, within the compatibility window this document defines"* —
follow-up copy work, not blocking this document's merge.
