# Vocabulary gaps the migration needed — ALL CLOSED

Every gap this migration surfaced has shipped as a flowproof PR. With these
merged, every spec in `specs/` parses and records with the rules author
alone (zero LLM calls), matching the Playwright suite's coverage.

## Shipped in PR #21 — action vocabulary

| Feature | Spec syntax |
|---|---|
| Named keys | `Press Enter`, `Press Escape` |
| Chords | `Press Control+V`, `Press Alt+Shift+Backspace` |
| Clear a field (fill semantics, React-safe) | `Clear the "Field Name" field` |
| Type into focused element | `Type First Name` (no target) |
| CSS selectors in quoted targets | `Click "css:[data-test='expand']"` — works in every quoted-target position |
| Prefix match fallback for text anchors | `Click "Database"` matches text *starting with* "Database" when no exact match exists |
| Ordinal disambiguation | `Type email into the 2nd "Field Name" field`, `Click the 2nd "css:…"` |

## Shipped in PR #22 — assertions (all auto-waiting, bound recorded)

| Feature | Spec syntax |
|---|---|
| Negative | `assert: page does not show TestConnection` |
| Occurrence count | `assert: page shows playwrightTemplateRoot 2 times` |
| Field value | `assert: the templateName field contains X` / `the "Field Name" field contains X` |
| Element-scoped | `assert: the "css:#live_preview" shows Street` |
| Visibility | `assert: the "css:#modal" is visible` / `is not visible` |

The ladder resolver runs inside the assert poll — asserting on a toast
that appears later works, at recording and replay. Syntax note learned the
hard way: a YAML scalar cannot START with `"`, so quoted targets always
follow `the `.

## Shipped in PR #23 — session setup & navigation

| Feature | Spec syntax |
|---|---|
| Env refs in `url:` | `url: ${DM_BASE_URL}/templates` |
| Cookie injection (pre-load) | `session.cookies: [{name, value}]` |
| localStorage seeding (before page scripts) | `session.local_storage: {projectId: ${DM_PROJECT_ID}}` |
| Mid-flow navigation | `Go to /settings` |
| Reload | `Reload the page` |

Session values resolve `${VAR}` at apply time (recording and every replay)
and never enter the trace.

## Shipped in PR #24 — suite runner

`flowproof run specs/` — every `*.flow.yaml` under the directory, failing
flows don't stop the rest, one merged `.flowproof/suite-junit.xml`, exit 1
if any flow failed.

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
  `following-sibling::button`): needs a `data-test='previewToggle'` in the
  app — flagged in the spec as an app-side one-liner, not a framework gap.
