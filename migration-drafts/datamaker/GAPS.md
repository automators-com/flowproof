# Vocabulary gaps the migration needs (one flowproof PR each)

Every step marked `# GAP-n` in the specs requires vocabulary that flowproof
does not have yet. This file is the contract for the gap PRs: when all four
land, every spec in `specs/` parses and records with the rules author alone
(zero LLM calls), matching the Playwright suite's coverage.

## GAP-1 — Session setup & navigation

| Feature | Spec syntax | Playwright equivalent |
|---|---|---|
| Env refs in `url:` | `url: ${DM_BASE_URL}/templates` | `baseURL` config |
| Cookie injection | `session.cookies: [{name, value}]` | worker fixture injects `automators.session` JWT |
| localStorage seeding | `session.local_storage: {projectId: ...}` | fixture `addInitScript` |
| Mid-flow navigation | `Go to /settings` | `page.goto("/settings")` |
| Reload | `Reload the page` | `page.reload()` |

`session:` values resolve `${VAR}` at the moment of use (same fail-closed
semantics as typed secrets); cookie values never enter the trace.

## GAP-2 — Action vocabulary

| Feature | Spec syntax | Playwright equivalent |
|---|---|---|
| Named keys | `Press Enter`, `Press Escape` | `keyboard.press("Enter")` |
| Chords | `Press Control+V`, `Press Alt+Shift+Backspace` | `keyboard.press(...)` |
| Clear a field | `Clear the "Field Name" field` | `locator.clear()` / `fill()` replace semantics |
| Type into focused element | `Type First Name` (no target) | `keyboard.type(...)` (react-select) |
| CSS selectors in quoted targets | `Click "css:[data-test='expand']"` — the `css:` prefix works in every quoted-target position (Click/Press/Type/Clear/assert scopes) | `locator(css)` / `data-test` selectors |
| Prefix match fallback for text anchors | `Click "Database"` matches a button whose accessible text *starts with* "Database" when no exact match exists | `getByRole("button", {name: /^Database\b/})` |
| Ordinal disambiguation | `Type email into the 2nd "Field Name" field` | `locator.nth(1)` |

## GAP-3 — Assertion vocabulary (all auto-waiting, timeout recorded in trace)

| Feature | Spec syntax | Playwright equivalent |
|---|---|---|
| Negative | `assert: page does not show TestConnection` | `expect(...).toHaveCount(0)` / `not.toBeVisible()` |
| Element-scoped | `assert: "css:#live_preview" shows Street` | `expect(preview).toContainText(...)` |
| Field value | `assert: the templateName field contains playwrightTemplateRoot` | `expect(input).toHaveValue(...)` |
| Visibility | `assert: "css:#live_preview" is visible` / `is not visible` | `toBeVisible()` / `not.toBeVisible()` |
| Occurrence count | `assert: page shows playwrightTemplateRoot 2 times` | `expect(locator).toHaveCount(2)` |

Negative asserts poll until the text is *absent and stays absent* for one
poll interval, bounded by the recorded timeout — deterministic at replay.

## GAP-4 — Suite runner

`flowproof run specs/` — run every `*.flow.yaml` under a directory,
aggregate pass/fail, one merged `junit.xml`, exit non-zero if any flow
fails. Needed so DataMaker CI invokes one command for the whole suite.

## Deliberately NOT closed (coverage consciously reduced or moved)

- **Clipboard readback** (`navigator.clipboard.readText()` + paste-verify in
  Copy-ID tests): specs assert the visible "Copied" confirmation instead.
- **Download events** (CSV export in demo-generate): specs assert the
  Download control appears; the export *content* contract lives in the API
  suite (`demo-generate.api.test.ts`), which stays in Playwright.
- **Network-level asserts** (`waitForRequest`/response status): replaced by
  asserting the UI outcome the request causes (toast, list update, counter).
- **SSE pipeline asserts** (demo-chat gated test): reduced to the visible
  terminal status; agent output is non-deterministic by nature.
- **XPath-relative targeting** (demo-connect preview chevron
  `following-sibling::button`): needs a `data-test` attribute in the app —
  flagged in the spec as an app-side one-liner, not a framework gap.
