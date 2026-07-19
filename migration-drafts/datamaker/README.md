# DataMaker UI suite → flowproof migration (draft specs)

Source: `apps/datamaker/tests/playwright/` (41 tests, 9 files).
Target: one flowproof spec per user-visible flow, in `specs/`. Playwright
tests stay in place (parallel operation) — these specs are the migration
surface. Steps marked `# GAP-n` need vocabulary from the gap PRs listed in
[GAPS.md](GAPS.md); everything else records with today's rules author.

## Coverage accounting (41 Playwright tests)

| Disposition | Count | Which |
|---|---|---|
| Migrated 1:1 | 35 | all specs under `specs/` except the gated two |
| Migrated, gated (like the original) | 2 | `demo/chat-agent-pipeline` (agent key), `demo/connect-postgres` (reachable DB) |
| Not migrated: `test.skip`'d broken app features | 3 | avatar upload, user-info update, member role change |
| Not migrated: API test in UI clothing | 1 | connections :: "Testing database providers" (posts to `/connections/test`, no UI) |

The Playwright `beforeEach` create-if-empty fallbacks and `toPass` re-query
loops have no spec equivalent on purpose — per-spec seeding (below) makes
them dead code.

Also NOT migrated, by design: `apps/datamaker-api/tests/playwright/**`
(34 files of browser-free HTTP API tests using Playwright `request` only) —
integration tests, not E2E UI; they stay in Playwright.

## Environment (mirrors the Playwright fixtures)

| Variable | Meaning |
|---|---|
| `DM_BASE_URL` | app under test, `http://localhost:3000` locally |
| `DM_SESSION_COOKIE` | JWT minted via `POST $AUTH_URL/api/auth/tests`, injected as cookie `automators.session` |
| `DM_PROJECT_ID` / `DM_TEAM_ID` | from the seed step's ids file, seeded into localStorage |
| `DM_INVITEE_EMAIL` | `playwright-invite@datamaker.test` (from `data/users.json`) |
| `E2E_TEST_DB_URL` | reachable Postgres for the gated live-connection demo |

All `${VAR}` references resolve at the moment of use and never enter the
trace (flowproof's secret indirection).

## Seeding: one deliberate difference from Playwright

The Playwright suite seeds **once per worker** and lets tests mutate shared
state, which forced order-dependent defensive code (create-if-empty
fallbacks, `.first()` on unknown lists, re-query `toPass` loops). The specs
instead assume a **fresh seed per spec** (`seedWorker.ts` — unchanged — run
before each flow, cheap: a handful of Prisma inserts). That is what lets
every spec address seeded entities by literal name (`playwrightTemplate`,
`TestConnection`, `PlaywrightTeam`) and assert exact counters
(`2 templates found`) deterministically.

## Assertion translation policy

Playwright's network-level asserts (`waitForRequest`, response status,
response body) are replaced by asserting the **UI outcome** the request
causes: toasts ("Team updated successfully"), list updates, counters,
form state. Where the app gives no feedback (connection export), the spec
says so. Clipboard readback and download events are consciously reduced —
see the end of GAPS.md.
