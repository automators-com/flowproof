# Release checklist

Run through this after `publish.yml`/`publish-npm.yml` have shipped a version
to PyPI and npm, before announcing the release anywhere. It exists because
none of CI's other gates run *after* publishing — they all prove the engine
or the build, not what a stranger typing an install command actually gets.

1. **Run `release-smoke.yml`** (workflow dispatch, exact version, e.g.
   `0.22.0` — not `latest`) and confirm every matrix leg is green: PyPI and
   npm, on Linux x64, Windows x64, and macOS ARM64. A red run means the
   release is not ready to announce, regardless of what `publish.yml`
   reported at build time.
2. **darwin-x64 (Intel macOS) is not covered by step 1.** It is checked for
   the right architecture at publish time (`publish.yml`'s wheel build), but
   there is no schedulable Intel macOS GitHub-hosted runner today, so it is
   never *executed* in CI (see the comment in `npx-smoke.yml`). Verify it
   manually on real Intel Mac hardware if one is available, or accept this
   as a known, documented gap until GitHub offers a schedulable runner.
3. **Confirm the public install paths still match what's live.** The
   website's install instructions and this repo's README should both still
   name a supported `pip install flowproof` / `npx flowproof` invocation —
   this is a manual check across two properties this repo doesn't own end
   to end, not something `release-smoke.yml` can assert on its own.

Only announce the release once all three are done.
