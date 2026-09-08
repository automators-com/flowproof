---
title: "Assertions"
description: "The shared assertion grammar every app profile supports, including scoped targets for table cells and list items."
---

All assertion forms **auto-wait** (default 10s, recorded into the trace);
append `within <N>s` to any form to change the bound.

| Assert | Meaning |
|---|---|
| `page shows <text>` | the whole surface (page text / window subtree / SAP session / OCR frame) contains `<text>` — `the page shows <text>` also accepted |
| `page shows <text> <N> times` | exact occurrence count of the TEXT |
| `page does not show <text>` | waits for it to be GONE |
| `page url is <expected>` | the surface's URL. A `<expected>` starting with `/` compares the PATHNAME exactly, including the query only when `<expected>` carries a `?` and the fragment only when it carries a `#` (so `/orders` ignores `?page=2`); one containing `://` compares the whole URL exactly. Web flows only: a window or an OCR frame has no URL, and the error says so |
| `page url contains <text>` | substring of the whole URL |
| `cookie "<name>" exists` | the cookie is set. Web flows only; auto-waits, since a cookie lands with a response |
| `cookie "<name>" is httpOnly` | not readable by page scripts - the control that stops an XSS exfiltrating a session |
| `cookie "<name>" is secure` | only sent over TLS. See the honesty note below |
| `cookie "<name>" is persistent` | carries an explicit expiry, so it outlives the browser session |
| `page title is <expected>` | the document title, compared whole (trimmed). Auto-waits like `page url`, because an SPA sets `document.title` after the route commits. Web flows only: a desktop window has a window CAPTION, which is a different property, and the error says so |
| `page title contains <text>` | substring of the document title |
| `the [2nd ]"<label>" field contains <text>` | input VALUE, by label |
| `the <id> field contains <text>` | input VALUE, by native id |
| `the [2nd ]"<target>" shows <text>` | element-scoped substring |
| `the [2nd ]"<target>" shows ${captured.<name>}` | compare against a remembered value: text, with the same matching ladder as any `shows` |
| `the [2nd ]"<target>" shows ${captured.<name>} + <number>` / `- <number>` | compare NUMERICALLY against the remembered number offset by a literal, e.g. `the "Balance" shows ${captured.balance} - 100`. Currency symbols and thousands separators are ignored on both sides |
| `the [2nd ]"<target>" is visible` / `is not visible` | the target resolves **and is rendered**. Resolving is only half of it: a `display:none` input is in the DOM and answers every selector, so a presence-only reading called it visible and the assertion could not fail. On the web the browser's own definition decides (`display:none` anywhere up the tree, `visibility:hidden`, `content-visibility`, the `hidden` attribute); `is not visible` is satisfied by absent OR hidden, because both mean the user cannot see it. A failure says which: *present and not rendered* is a different bug from *never appeared*. An element the browser renders but which occupies no box still counts as visible. A surface with no notion of rendered-ness beyond resolution (UIA, SAP, vision) keeps the presence reading |
| `the "<target>" appears <N> times` | how many ELEMENTS match the anchor. Exact, not a minimum. No ordinal: `the 2nd "Row"` is one element by construction, so counting it has no answer |
| `the [2nd ]"<target>" is enabled` / `is disabled` | platform enabled state (`disabled`/`aria-disabled` on web, UIA IsEnabled on desktop) |
| `the [2nd ]"<target>" checkbox is checked` / `is not checked` | checkbox state, read from the `checked` property or `aria-checked`. A target that is not a checkbox fails as exactly that, not as "wrong state" |
| `the "<target>" is empty` / `is not empty` | the target's trimmed visible text (or input value) is empty. A first-class predicate: `shows ""` cannot express it |
| `the [2nd ]"<target>" attribute <name> is <value>` / `is not <value>` | a DOM attribute's value, compared EXACT and case-SENSITIVE (attributes are machine strings - no text-matching ladder, no substring). `<name>` is case-insensitive. `is not` passes when the attribute is ABSENT or has a different value. Missing and empty are distinct. `${VAR}` resolves in the value; a `${captured.x}` there is a parse error (captures compare against visible text with `shows`). Web only |
| `the [2nd ]"<target>" has attribute <name>` / `does not have attribute <name>` | attribute PRESENCE only (`download=""` counts as present). Web only |
| `the [2nd ]"<target>" style <prop> is <value>` / `is not <value>` | a COMPUTED CSS value. `<prop>` is a closed allowlist: `color`, `background-color`, `text-transform` (anything else is a parse error - geometry belongs in `assert_screenshot`, visibility in `is visible`). Colors compare CANONICALLY (named / `#rgb` / `#rrggbb` / `rgb()` / `rgba()` all parse to RGBA); `text-transform` compares its keyword case-insensitively. `style`, not `css`: `css:` is the selector escape hatch. Web only |
| `the "<column>" column of the row containing "<anchor>" <predicate>` | a table cell, by IDENTITY. See below |
| `the "<inner>" in the item containing "<anchor>" <predicate>` | an element inside the list item holding `<anchor>`. See below |
| `the "<inner>" in the iframe "<frame>" <predicate>` | an element inside a same-origin iframe. Assertions only. See [iframes](#iframes-same-origin-assertions) |
| `the "<inner>" in the "css:<container>" containing "<anchor>" <predicate>` | the same, with the container named explicitly |

Two different questions share the word "times", and picking the wrong one
is a quiet way to write a test that cannot fail:

```yaml
- assert: page shows Pending 3 times      # the TEXT appears 3 times anywhere
- assert: the "css:.order-row" appears 3 times   # 3 ELEMENTS match
```

A list assertion almost always wants the second. Three rows whose labels
happen to repeat a word are still three rows, and a row that renders its
status twice would satisfy the first without any row existing at all.

Counting rides on the same ordinal as `the 2nd "Row"`, so it means on each
adapter exactly what an ordinal means there: DOM order on web, UIA tree
order on the desktop, reading order under vision. A passing count costs
`N + 1` questions to the app; only a FAILING one counts further, so the
error can say `found 5` rather than just "not 3".

The URL forms map `cy.location("pathname").should("equal", "/signin")` and
`cy.url().should("include", "checkout")`, and they auto-wait like every other
assertion, because an SPA redirect lands asynchronously:

```yaml
- assert: page url is /signin
- assert: page url contains checkout
- assert: page title is Orders - Acme Admin
- assert: page title contains Acme
- assert: page url is /orders?page=2 within 15s
```

Checkboxes map `cy.check()` / `should("be.checked")`:

```yaml
- Check the "Remember me" checkbox
- assert: the "Remember me" checkbox is checked
- Uncheck the "Remember me" checkbox
- assert: the "Remember me" checkbox is not checked
```

### Scoped targets: table cells and list items, by identity

Repeated UI - a grid's rows, a list's items, a board's cards - needs a way
to say WHICH one without counting. Both forms name the region by its
content and then address the element inside it:

```yaml
# a table cell: the column's header text plus an anchor identifying the row
- assert: the "Status" column of the row containing "Grace Hopper" shows Suspended
- assert: the "Balance" column of the row containing "Grace Hopper" is empty
- Click the "Actions" column of the row containing "Grace Hopper"

# a list item: an anchor identifying the item, then the ordinary target
- assert: the "css:.amount" in the item containing "Invoice 4711" shows 50.00
- assert: the "Amount" field in the item containing "Invoice 4711" contains 50
- Click the "Pay" in the item containing "Invoice 4711"
- Check the "Select" checkbox in the item containing "Invoice 4711"

# a container the `item` rung cannot see: name it
- Click the "Ship" in the "css:.card" containing "Order 8801"

# one column does not always name a row: require both
- Click the "Edit" in the item containing "John" and "Doe"
- assert: the "Email" column of the row containing "John" and "Doe" shows john@example.com
```

The same cell target composes with every predicate (`shows`, `is empty`,
`is [not] visible`, `is enabled`, `checkbox is [not] checked`, `attribute
<name> is [not] <value>`, `has|does not have attribute <name>`, `style <prop>
is [not] <value>`) and every action (`Click`, `Type … into`, `Clear`,
`Check`, `Scroll`, `Select`). `in the row containing` also works - the of/in
coin flip is one you should not have to remember.

A **dropdown inside a row** is the case this exists for. A page that puts
one `<select>` per row gives them all the same label, so without a scope
the only way to reach the third one is `the 3rd "Value"` - the positional
addressing scopes were built to remove:

```yaml
- Select Approved from the "Value" column of the row containing "Invoice 4711"
- Select "A", "B" from the "Tags" field in the item containing "Invoice 4711"
```

A column is matched by its header's text and then addressed by that
header's position **within its own row**, counting header and data cells
together. A schedule-style grid whose header row opens with a stub above
the row-label column (`<tr><td></td><th>Monday</th>…`) therefore lines up:
counting `th`s against `td`s would read one column to the left, and return
a real cell, which passes as confidently as the right one.
| Form | Notes |
|---|---|
| `the "<column>" column of the row containing "<anchor>"` | a table cell; `in the row containing` also works - the of/in coin flip is one you should not have to remember |
| `… containing "<A>" and "<B>"` | on either scoped form: EVERY anchor must be in the SAME row or item. For when one column does not name a row - two people called John, two called Doe. The quotes delimit and `and` is a separator, exactly as in a multi-option `Select`. Anchors sitting in different rows match nothing rather than picking one, and a conjunction that is still ambiguous gives the same "matches N rows" error a single anchor does |
| `the "<inner>" in the item containing "<anchor>"` | `item` means exactly `li`, `[role=listitem]`, `[role=row]`, `[role=option]`, `[role=article]`, `tr` - a closed list, not a guess |
| `the "<inner>" inside the item containing "<anchor>"` | `inside` is a synonym for `in` |
| `the "<inner>" in the "css:<sel>" containing "<anchor>"` | any container, named explicitly; `"id:<id>"` too |

Both targets compose with **every predicate** (`shows`, `shows
${captured.x} ± n`, `is [not] empty`, `is [not] visible`, `is
enabled|disabled`, `field contains`, `checkbox is [not] checked`) and
**every action** (`Click`, `Type … into`, `Clear`, `Check`/`Uncheck`,
`Press … button`, `Right-click`, `Remember … as`): one shared suffix
parse rebinds the target, so nothing composes specially. A role noun goes
BEFORE the scope phrase: `the "Amount" field in the item containing "X"
contains 50`, `Check the "Select" checkbox in the item containing "X"`.

Why identity, not `the 2nd ".column-status"`: an ordinal encodes position,
so inserting a row or reordering a column silently makes the assertion hit
the wrong record. Identity survives both - the trace records the header
text, the anchor, and (when the live DOM offers one) the row's or
container's own id as a fallback, and replay finds them wherever they
moved. For that reason an ordinal cannot address a scoped target on either
half: `the 2nd "Status" column …` and `the "Amount" in the 2nd item
containing …` are both parse errors. Nor can the two nest: one container,
or one cell, and the element inside it.

Resolution is generic over any `<table>` or ARIA grid (`role=grid`/`table`/
`treegrid`), so react-admin, MUI DataGrid and AG Grid all work with no
framework-specific selector. Two things are hard errors rather than a
silent wrong guess, and both point at the `css:` escape hatch: a row anchor
that matches more than one row (`use a more specific anchor`), and a
duplicate column header. **Known boundary:** a virtualized grid that keeps
off-screen rows out of the DOM (AG Grid's row virtualization) can only be
addressed for rows that are rendered; bring the anchor row in with `Scroll
"<anchor>" into view` first (or use `css:` against the grid's own row API),
then the cell predicates - `shows`, `attribute <name> is <value>`, `style
<prop> is <value>`, and the rest - resolve it.
Cell resolution is generic over any `<table>` or ARIA grid
(`role=grid`/`table`/`treegrid`), so react-admin, MUI DataGrid and AG Grid
all work with no framework-specific selector. Container resolution has two
rungs and no heuristics: the explicit `css:`/`id:` selector, or the closed
`item` list above. Among the containers holding the anchor the **innermost
wins**, so an item nested in a group resolves to the item.

Three things are hard errors rather than a silent wrong guess, and all
three point at the `css:` escape hatch: an anchor matching more than one
row or item (`use a more specific anchor or a css: container`), a duplicate
column header, and a container that is neither `item` nor a selector (`in
the "Transaction" containing …`, where "Transaction" is a noun, not a
container).

**Steps are not instant, and some apps care.** A click step costs roughly
**0.2 seconds** between the action landing and the next one reaching the
page, and typing adds about **20ms per character** — measured on a local
fixture with no network. The cost is CDP round trips, and a keystroke is
two of them. The probes that only need an answer — does the target exist,
is it actionable — each ask the page in a single round trip for css and
text-anchor targets; earlier engines walked an element-handle path that
cost four to six calls per question, which put a step at 3.1-3.2s.

Those numbers assume the patched transport this workspace pins (see the
`[patch.crates-io]` block in the root `Cargo.toml`). The published
`headless_chrome` transport shares one mutex between the socket reader and
every sender, and the reader holds it across a blocking read — so a send
waits out a read rather than proceeding. Profiling found the reader holding
that lock for 94% of a run's wall-clock while the writes themselves cost
0.07ms each. Unpatched, a click costs ~1.4s and a character ~213ms, which
is where the "0.2s per character" figure in older notes comes from. The
patch shortens the read timeout and stops polling for responses; it does not
remove the shared lock, so a send still queues behind a read.

A deadline-bearing interaction — a value that stays valid for two seconds,
a token that expires, a confirmation that auto-dismisses — may still be
**out of reach** once a step involves typing more than a few characters,
and the failure is at least loud rather than silent: the app's own
complaint (an alert, a rejection) surfaces as a failed step rather than a
green run that did the wrong thing.

If a flow needs to beat a deadline, the honest options are to remove the
deadline from the environment under test (`mock:` the endpoint that issues
it, or pin the clock) rather than to hope the step lands in time.

**Known boundaries.** A virtualized list or grid that keeps off-screen rows
out of the DOM (AG Grid's row virtualization, windowed feeds) can only be
addressed for what is rendered: scroll the anchor into view first, or use
`css:` against the widget's own API. Content inside a closed shadow root or
a cross-origin iframe is unreachable to any selector, scoped or not. And an
anchor that appears in EVERY item ("Invoice", when every item says
"Invoice") is ambiguous by design: it identifies nothing, and the error
says so instead of picking the first one. `appears <N> times` cannot be
scoped to a container yet.
