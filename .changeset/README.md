# Changesets

Every pull request that changes how flowproof behaves adds one file here: a
plain Markdown fragment, in the same voice as `CHANGELOG.md`, describing what
was wrong (or missing) and why it matters.

## Why a fragment instead of editing CHANGELOG.md directly

`CHANGELOG.md` has one `## Unreleased` section that every open PR would
otherwise share. Two PRs touching the same few lines conflict on every rebase
— and it gets worse with concurrent loop-authored PRs (see
`scripts/loop/roles/builder.md`): several Builders can have PRs open against
`main` at once, each needing to append to the same section. A fragment per PR
gives each PR its own file, so PRs stop colliding on prose that has nothing to
do with what either of them changed.

## When to add one

Add a fragment when the change is visible to someone using flowproof: new or
changed CLI/SDK behavior, a bug fix, a trace-format change, a changed default.

Skip it for changes with no user-visible effect: internal refactors with no
behavior change, CI/workflow tweaks, doc fixes, adding or adjusting tests,
dependency bumps that change nothing observable. `scripts/gate/ratchets.sh`
mechanically exempts doc-only changes, `.github/workflows/`, `Cargo.lock`, and
files under `tests/` or a crate's own `tests/` directory — everything else
that isn't a `.md` file is asked for a fragment, since a script can't tell a
pure refactor from a behavior change by its file path. A PR that touches
none of those exempt paths but genuinely has no user-visible effect can still
skip a substantive fragment by saying so in one line, or a human reviewer can
wave it through — the gate only withholds the Builder's own automated
approval, never a human merge.

## Format

One file per PR: `.changeset/<short-slug>.md`, containing one paragraph (or a
short bold lead-in plus a paragraph) in the CHANGELOG voice — name what was
wrong, why it mattered, what holds now. No frontmatter and no bump type:
flowproof's crates, the Python wheel, and the npm package all move together as
one version (see "Versions move together" in `CLAUDE.md`), so there is no
per-package bump to record here. Read the last few `CHANGELOG.md` entries
before writing one.

```md
**A stuck CDP connection now fails in seconds, not minutes.** `frame_act`
waited on the vendored transport's own 300s timeout for a single call's
response. Both calls now bound their own wait to 30s from the caller's side,
so a dead connection is retried immediately instead of waited out.
```

## What happens to it

At release time, whoever cuts the release folds every pending fragment into a
new version heading in `CHANGELOG.md` — usually verbatim, since each fragment
was already written in the release voice — then deletes the fragment files in
that same commit. `.changeset/` holds only this README between releases.
