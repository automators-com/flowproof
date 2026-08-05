# The authoring grammar — every accepted form

In the default `--author auto` mode, a plain scalar UI step is
**natural-language model intent**:

```yaml
- Enter 24 Market Street in the shipping address field
```

`record` grounds that intent against the live scene. To opt one step into
the deterministic grammar instead, mark it explicitly:

```yaml
- rules: Type Ada into the "Full name" field
- rules: Press the "Save" button
```

`--author rules` remains the global opt-in when a whole flow already uses
the deterministic grammar; `--author llm` forces model authoring for plain
UI steps. Structured forms such as `assert:`, `assert_api:`, `repeat:` and
`when:` retain their own semantics in every mode.

If auto mode has no configured authoring model, recording says so visibly
and falls back to deterministic rules for plain steps. It never silently
reinterprets model intent. Human output identifies each step's route as
`rules`, `llm`, `reused`, or `fallback`, and structured/JSON output carries the same
per-step routing information for tooling; consumers should use the
structured output rather than scraping the display text.

This page is the **complete rules grammar**. The forms below are the text
accepted inside `rules: <text>` (or as plain steps under global
`--author rules`). They require no model call and are covered by tests that
parse the exact examples shown (`documented_grammar_examples_all_resolve`
in `crates/flowproof-agent/src/rules.rs` — if the doc and the code drift,
CI fails).

Model authoring does not make replay probabilistic. The driver gives the
model a finite list of provenance-neutral scene tokens and accepts only
actions grounded to those listed tokens; the resulting selectors and
actions are persisted in the trace. Replay executes that trace directly,
with zero model calls.

On the web, that inventory also represents readable values whose identity is
relational rather than global. A value cell in a div-based row may have no
unique id or class of its own, but still be stable as “the value beside `order
id`”. Flowproof exposes it to the model as one opaque `scoped:` token containing
a container, neighbouring text anchor, and inner selector. The model must copy
that token exactly; the token itself is not persisted. Recording translates it
to the same deterministic scoped target used by explicit rules, so replay
finds the newly rendered row by its anchor and reads the current value.

The inventory covers the rendered page, not just the current viewport, because
users naturally refer to a control that starts below the fold. It also gives
the model grounded identities for table-row collections, final table cells,
drag sources and destinations, small styled or identified visual targets, and
readable/actionable elements inside visible same-origin frames. Frame and
scoped tokens are authoring-only handles: Flowproof translates them to ordinary
deterministic targets before writing the trace.

A plain step is a unit of intent, not a unit of work. `Fill out all the vehicle
data and click next` is one step, and the model answers it with the whole
sequence of grounded actions it takes — one per field, plus the button — in a
single call. Every action in that sequence is grounded against the same listed
inventory and rejected as a whole if any one of them is not, so a half-filled
form never reaches the trace. The inventory also reports what each field
currently holds, which the page marks required, which boxes are ticked, and a
dropdown's exact options, so a `<select>` is given a name it really has rather
than a plausible guess. Values of password fields are never reported.

Plain language is not limited to midpoint clicks and typing. The structured
model response can directly express clicking a point within a control,
dragging, remembering a count or value, choosing one or several select options,
scrolling a container to an exact offset, typing inside a frame, and pressing a
key. These are capabilities of the authoring protocol, not syntax authors must
learn. Write the user intent—for example, `Select Functional, End2End, GUI, and
Exploratory testing together`—and keep `rules:` for the comparatively rare case
where exact deterministic grammar is deliberately wanted in the source spec.

Conventions: forms are case-insensitive in their keywords. `<text>` is
literal text (may carry `${VAR}` secret references). A quoted `"<label>"`
is a **text anchor** — matched against visible text, accessible label
(`aria-label`), placeholder, an associated `<label>` (both
`<label>Name: <input/></label>` wrapping and `<label for>`/`id` pairing),
or, for `<input type="submit|button|reset">`, the `value` attribute (the
accessible name of a void button-type input, so `Press the "Login"
button` finds `<input type="submit" value="Login">`).
Matching is exact first, then prefix (`"Name"` finds the field labelled
`Name:`), then ASCII case-insensitive as a last resort (`"Close Account"`
still finds the button reading `Close account`) — a case-sensitive match
always wins. `page shows` reads visible text **plus** the accessible names
of visible elements, so icon-only buttons that exist purely as an
`aria-label` count. Assertion TEXT matches the same way selectors do:
exact first, then case-insensitive (`page shows Close Account` passes
against a page reading `Close account`), and the negative forms mirror
it — if `shows X` would pass, `does not show X` fails. Two escape
hatches work inside any
quoted label: `"css:<selector>"` (web) and `"id:<native id>"` (DOM id,
UIA AutomationId, SAP scripting id). `[2nd ]` marks an optional 1-based
ordinal (`2nd`, `3rd`, `10th`) for when several elements match.

Steps are only half the spec. Starting state that a flow should not
rebuild through the UI (an authenticated session, a pre-filled cart or
other app-state fixture) is declared in the spec-level `session:` block,
and network shaping in `mock:` - see
[test-context seeding](getting-started.md#test-context-seeding-sessions-fixtures-and-navigation)
before migrating a suite's setup helpers step by step.

## Actions (web, sap, vision — the generic grammar)

| Step | Notes |
|---|---|
| `Type <text> into the [2nd ]"<label>" field` | text anchor / `css:` / `id:` |
| `Type <text> into the <id> field` | bare native id |
| `Type <text>` | types into the FOCUSED element |
| `Replace the [2nd ]"<label>" field with <text>` | clear + type, one step |
| `Replace the <id> field with <text>` | |
| `Clear the [2nd ]"<label>" field` / `Clear the <id> field` | fill-with-empty semantics |
| `Remember the [2nd ]"<target>" as <name>` | read the target's text into a flow-scoped name (`[a-z][a-z0-9_]*`) for a later assertion to compare against. The VALUE is read at execution time on record and on every replay, so it never enters the trace - the same indirection `${VAR}` secrets use. Re-using a name overwrites it |
| `Remember how many "<target>" appear as <name>` | the COUNT of matching elements, not their text. Same family and the same indirection as the reading above: taken at execution time on record and on every replay, so a page that grew a row does not need the trace rewritten. Counting rides the ordinal every adapter implements, so it means on each adapter exactly what `the 2nd "Row"` means there. **Zero fails** - a selector typo matches nothing and so does an empty table, and `0` is a confident wrong number to hand to an app; the step that MEANS zero is `assert: the "<target>" appears 0 times`. `appears` is accepted for `appear` |
| `Check the [2nd ]"<label>" checkbox` / `Uncheck the …` | drives a checkbox, radio, or `role=switch` to a STATE, not a toggle: `Check` on an already-checked box is a no-op, so the step means the same thing however the environment arrives. Resolves the control inside a wrapper too (the common pattern of a visually hidden `input` inside a styled label), performs a real click so the app's own handlers fire, then verifies the state took |
| `Select <option> from the [2nd ]"<label>" field` | native `<select>`: committed via the value setter, fires `input`+`change` (React-safe). `in the` and `… dropdown` also accepted. The option is matched by `value`, then exact visible text, then prefix - so `Audit` finds `Auditor`. A name matching NONE of those fails naming it, rather than falling through to typing (typing into a `<select>` is a prefix search of its own, so it would land on some other option and pass) |
| `Select "<A>", "<B>" and "<C>" from the [2nd ]"<label>" field` | a `<select multiple>`, driven to EXACTLY the named set in one commit with one `input`+`change` - what the app's own handler expects to see is a user finishing a selection, not three of them. Set-a-state like `Check`, not a toggle: what is named becomes selected and what is not named does not, so the step means the same thing however the environment arrived. **Every item is quoted**, because option text is arbitrary app text - `"Rock, Paper and Scissors"` is one option, and an unquoted list could not tell it from three. Names are resolved before anything is selected, so a typo in the third option leaves the control untouched rather than half-applied, and the step then verifies the selection took. Web only |
| `Press the [2nd ]"<label>" button` / `Press the <id> button` | |
| `Right-click [the [2nd ]]"<text>"` | opens the element's context menu; `Right click` also accepted |
| `Double-click [the [2nd ]]"<text>"` | fires a real `dblclick` on the element; `Double click` also accepted. Web only. Like `Click`, its effect is app-defined, so the step verifies the element resolved and the event dispatched, not app state |
| `Hover over [the [2nd ]]"<text>"` | moves the pointer onto the element with a single `mouseMoved`, no press/release (the `over` is required - there is no `Hover "<text>"` shorthand). The scoped `in the item containing "<anchor>"` form composes like any action. Web only. The step self-verifies that the element actually matches `:hover` after the move (the hit test landed on it or a descendant), so a move onto an occluded element fails rather than passing. Hover state persists until the next explicit pointer action, so a following `Click` can hit a hover-revealed element. Async-revealed menus and tooltips are handled by the next step's auto-wait, exactly like `Scroll`. Limitation: a hover-revealed target far below the fold is not replayable - if the next step scrolls the page, the pointer parked at the hover midpoint drifts off the trigger and a close-on-mouseout menu dismisses before the click lands |
| `Upload <path> into the [2nd ]"<label>" field` | sets a file on a file-chooser input (may be hidden behind a styled button); relative paths resolve against the working directory at execution |
| `Upload <path> into the <id> field` | |
| `Click [the [2nd ]]"<text>"` | tabs, links, menu options, rows. Refuses at RECORD time when another element would receive the click - the same hit test replay applies, so a click that could not replay is never recorded as one |
| `Drag [the [2nd ]]"<source>" onto [the [2nd ]]"<target>"` | press at the source, move across with the button held, release on the target. **The next step must be an assertion** — a compile error otherwise. Every other action has something intrinsic to verify; a drop does not, because its effect is app-defined (a reorder, a mutation that re-renders identically, nothing at all), and "events dispatched" is not a verification. So the grammar makes you say what the drop did, and a silent no-op turns red at the assert instead of green at the drag. Mouse family only — the one jQuery UI, SortableJS and react-dnd's mouse backend listen to; a page using native HTML5 drag-and-drop is a different mechanism and is not served by this. Both ends resolve through the ordinary ladder and both wait to be actionable. Web only |
| `Click [the [2nd ]]"<text>" at <x>%,<y>%` | click a POINT INSIDE the element rather than its midpoint, for a control that reads `offsetX`/`offsetY` and acts on where it was hit - a split button, a slider track, a canvas region. **Percentages of the element's own box**, never pixels: an element's size depends on the viewport and the font, so a pixel offset recorded on one machine addresses a different part of the control on another. Both parts must be between 0% and 100% - out of range is a parse error rather than a clamp, because a clamped `120%` becomes an edge click that looks deliberate and is not what was written. The step verifies the point actually lands on the element before dispatching (the `Hover` hit test), so an offset that falls on a rounded corner or an overlapping sibling fails instead of clicking the wrong thing. Web only |
| `Scroll the [2nd ]"<target>" to the [top\|bottom]` | scroll the TARGET as a container to an edge (the `the` before top/bottom is optional). Web only |
| `Scroll [the [2nd ]]"<target>" into view` | bring an in-DOM element into the viewport. Web only |
| `Scroll [the [2nd ]]"<target>" to <n>px` | scroll a container to an EXACT offset from its top. Pixels, not a percentage, and this is the one place pixels are right: `scrollTop` is a real DOM unit applications key behaviour to, while a percentage of `scrollHeight` is a unit nothing asserts. The unit is required (`to 147`, without `px`, is a parse error, so a second unit could never change what an old flow meant), and the offset must be a whole number. Verified after the write with a ±1 tolerance, because `scrollTop` reads back fractionally under a non-integer device pixel ratio. Fails - rather than clamping - when the container stops short, and refuses a target whose content fits, since scrolling that would pass without moving anything. Only meaningful under a pinned `browser.viewport`, the caveat visual assertions carry. Web only |
| `Scroll to the [top\|bottom]` | scroll the PAGE itself (no target, like `Press <Key>`). Web only. Scroll is instant with no settle-wait - the next assertion auto-waits - and the step verifies the scroll took (edge reached / rect in viewport) |
| `<action> … the "<target>" in the item containing "<anchor>"` | any action above, scoped to one list item or table cell - see [Scoped targets](#scoped-targets-table-cells-and-list-items-by-identity). `Select` takes it too: `Select Approved from the "Value" column of the row containing "Invoice 4711"`, where the role noun is optional because the column and the row anchor already say which control is meant |
| `Press <Key>` / `Press <Mod>+<Key>` | `Enter`, `Escape`, `Tab`, `Backspace`, `Delete`, `Space`, arrows, `Home`/`End`, `PageUp`/`PageDown`; chords `Control+V`, `Alt+Shift+Backspace`. `Mod` (aliases `CtrlOrMeta`, `ControlOrMeta`) is the **portable** primary modifier: stored neutrally in the trace and resolved at execution — Meta on macOS, Ctrl elsewhere — so `Press Mod+K` recorded on a Mac replays on Linux CI |
| `Press F1`–`F12`, alone or in a chord (`Press Alt+F4`) | **Desktop only** — every UIA-driven app plus SAP and vision. SAP is largely driven by them (F3 back, F4 value help, F8 execute), and they are often the only way to reach an action with no clickable equivalent; `Alt+F4` is also the dependable way to close a window, since a title-bar caption button is frequently absent from the UIA tree. Spelling is case-insensitive and stored canonically (`f4` → `F4`). On **web** these are refused at authoring time, not at replay: the browser has no key definition for them, so drive the control itself with `Press the "<label>" button` or `Click "<text>"` |
| `Go to <path-or-URL>` / `Navigate to <path-or-URL>` | relative paths resolve against the flow URL's origin; on SAP this is transaction navigation (`Go to /nVA01`) |
| `Reload the page` | web |
| `<trigger>, accepting the "<message>" dialog` / `, dismissing [the "<message>"] dialog` / `, answering the prompt with "<text>"` | a **dialog suffix** on any trigger (`Click`, `Press the … button`, `Right-click`, `Double-click`, `Hover`) that opens a native `alert`/`confirm`/`prompt`/`beforeunload`. See [Native dialogs](#native-dialogs) below. Web only |
| `Wait until page shows <text> [within <N>s]` | long-bound auto-waiting assert (default 60s) |

There is deliberately **no `Blur` step**. Blur is not something a user does;
it is a DOM event that a user action causes. `Press Tab` is that action, it
already works, and it additionally tests what the user really experiences -
that focus lands somewhere sensible. Blur-triggered form validation is
exercised with `Press Tab`.

### Refused on purpose

`Blur` is one of a set. When deterministic rule authoring is selected,
these shapes are **recognised in order to be refused**: each fails with the
reason and what to write instead. A refused `rules:` step is never rerouted
to the model. That difference is the point: explicit deterministic intent
must either mean exactly what the grammar says or stop, never be quietly
reinterpreted as something adjacent that records green.

| Refused | Why, and what to write |
|---|---|
| `Click "Next" until …`, `While … , …` | Repetition is a block, not a step — write [`repeat:`](#repeating-until-the-app-settles-repeat-and-when). A loop written inside a step could not be expanded at record time, because by the time the step's own text is parsed there is nothing left to expand it into |
| `If … , …` / `… otherwise …` | A branch is a block, not a step — write [`when:`](#repeating-until-the-app-settles-repeat-and-when). If the two branches are two different things to prove, they are still two flows |
| `Remember the "<t>" matching /…/ as <n>` | A regex in the grammar is a second language inside the first. A capture reads an element's whole text; give the value its own element |
| `Remember the text between "X" and "Y" as <n>` | Pattern matching by another name. `Remember the "<target>" as <name>` reads a whole element, which is the unit a page actually renders |
| `${date:…}` / `{Date[…]}` | Against the wall clock a flow means something different every day; against a pinned `browser.clock` it is a constant you can write by hand. Pin the clock and type the literal |
| `Click … without hovering` | Dispatching an event no user could produce breaks the claim that a passing flow describes something a person can do. `Click` already moves the pointer, which is what a user does |

### Native dialogs

A native `window.alert` / `confirm` / `prompt` (and the navigation
`beforeunload`) blocks JavaScript **synchronously**: nothing else runs until
it is answered. So it cannot be its own step AFTER the click that opens it -
by then the page is already frozen waiting. Instead the disposition folds
into the **triggering action** as a comma suffix, and the engine arms a
one-shot handler BEFORE dispatching the click:

```yaml
- Click "Delete", accepting the "Are you sure?" dialog
- Click "Cancel", dismissing the dialog
- Press the "Rename" button, answering the prompt with "New name"
```

`accepting` presses OK, `dismissing` presses Cancel. The message is optional;
when present it is matched `contains`, and it IS the assertion - a dialog
whose text does not contain it fails the step. `answering the prompt with
"<text>"` accepts a `prompt` and supplies the reply; the reply is authored
input like `Type`, so a `${VAR}` reference resolves at execution and only the
reference is stored. The suffix works on every trigger - `Click`, `Press the
… button`, `Right-click`, `Double-click`, `Hover` - and composes with the
scoped `in the item containing "<anchor>"` form.

The post-condition is verified: a **declared** dialog that does not open
fails the step (parallel to `Check` verifying its state took). And a
flow-wide safety net catches the other direction - a step that triggers a
dialog it did NOT declare is **dismissed and failed** with `an unexpected
dialog opened: <message>`, never left to hang (the least diagnosable
failure). Both directions are deterministic. Native dialogs are **web only**;
a desktop message box is a real window, driven by ordinary steps.

## Assertions (every app — the shared grammar)

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

**Steps are not instant, and some apps care.** A step costs roughly **three
seconds** between one action landing and the next one reaching the page -
measured at 3.1-3.2s for a click followed by a type, on a local fixture with
no network. Most of it is CDP round trips: resolving the target, waiting for
it to be actionable, and reading back the state that proves the step took.

That is invisible until an app puts a DEADLINE on an interaction - a value
that stays valid for two seconds, a token that expires, a confirmation that
auto-dismisses. Those are currently **out of reach**, and the failure is at
least loud rather than silent: the app's own complaint (an alert, a
rejection) surfaces as a failed step rather than a green run that did the
wrong thing.

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

### Remembering and reusing live values

Model-authored steps may describe a remembered value naturally and use a
clear name or an unambiguous pronoun later:

```yaml
- Remember the order number
- Enter it in the "Confirmation" field
```

Recording grounds both steps to the live scene and persists deterministic
capture/read and type actions. The remembered value itself is still read
fresh during recording and replay; it is not baked into the trace. A named
reference is useful when the flow remembers more than one value:

```yaml
- Remember the order number as the order ID
- Remember the customer number as the customer ID
- Enter the order ID in the "Confirmation" field
```

A pronoun such as `it` is accepted only when one remembered value is an
unambiguous candidate. If two values could be meant, recording stops with
a structured clarification that lists the candidates. It does not choose
the nearest name or the first value.

For exact deterministic grammar, `${captured.<name>}` remains the explicit
advanced syntax. Mark the steps with `rules:` (or record the whole flow with
`--author rules`):

```yaml
- rules: Remember the "id:oid" as oid
- rules: Type ${captured.oid} into the "Order id" field
```

Computed assertions answer "did this change by the right amount?", which a
literal cannot express because the starting value is only known at run time:

```yaml
- Remember the "Account Balance" as balance
- Press the "Pay" button
- assert: the "Account Balance" shows ${captured.balance} - 100
```

The expression grammar is deliberately tiny and does not compose: one
capture reference, optionally one `+` or `-`, and one plain number. There is
no second capture, no nesting, no `*` or `/`.

A capture may also be **typed**, which is how a value the app generates per
run gets entered — there is no literal a trace could record, so the trace
stores the reference and every replay reads the value fresh:

```yaml
- Click "GENERATE ORDER ID"
- Remember the "id:oid" as oid
- Type ${captured.oid} into the "Order id" field
```

**A typed value is interpolated, not evaluated.** Every `${captured.<name>}`
in the text is replaced by what that element displayed, and the literal
characters around them are typed as written:

```yaml
- Type order-${captured.oid} into the "Ref" field      # order-1061367
- Type ${captured.first} ${captured.last} into the "Name" field
```

More than one reference in one step is fine, and so is a step that is all
literal apart from them. What does **not** happen is arithmetic. This:

```yaml
- Remember the "id:no1" as a
- Remember the "id:no2" as b
- Type ${captured.a} + ${captured.b} into the "Sum" field
```

types `12 + 30` — three tokens of displayed text with a plus sign between
them — and not `42`. That is interpolation behaving correctly, not a bug,
and it is the reason the step is worth spelling out: `12 + 30` looks close
enough to an answer that a flow could go green on it while asserting
nothing anybody meant.

Arithmetic is refused deliberately, not merely absent. A capture is *text
the app displayed*, and supplying it back is data entry — the thing a user
does with a generated id. Deriving a new value from two of them is a
computation, and a trace that carries a computation has stopped being a
recording of what happened. The one exception is on the assertion side,
where `shows ${captured.x} + <number>` answers "did this change by the
right amount?" — a question a literal cannot express, because the starting
value is only known at run time. It takes one capture and one plain number,
and it does not compose.

A name that was never remembered fails closed, naming what was in scope,
rather than typing the reference or an empty string.

A **counted** capture is the same value with a different reading, so it
composes with everything a captured value already does — including the
computed comparison, which needs no second definition of what a number is:

```yaml
- Remember how many "css:.order-row" appear as rows
- Type ${captured.rows} into the "Rowcount" field
- Press the "Add row" button
- assert: the "Total" shows ${captured.rows} + 1
```

Typing is where it stops. A capture may not choose an element or a
destination - `Click "${captured.x}"`, `Go to ${captured.x}`, or a capture
in a target label are all parse errors, because that would let the app under
test decide what the flow does next. Supplying text it just displayed is
data entry; picking the next element is control flow. A name that was never
remembered fails closed, naming what was in scope.

### Handing a value to the next flow (`exports:`)

A capture is flow-scoped. `exports:` is how one crosses to the flows that
run AFTER this one in a suite — which is how a test case spans
technologies: one flow drives SAP GUI and captures the order number off the
status bar, the next drives the web portal that must show it. Each flow
keeps its own `app:` and its own driver; the suite is the test case, and
the export is the thread through it.

```yaml
# a-create-order.flow.yaml — SAP GUI mints the order number
name: Create standard order
app: sap
steps:
  - Go to /nVA01
  # ... create the order ...
  - Remember the "id:wnd[0]/sbar" matching /\d+/ as order
exports:
  ORDER_NO: ${captured.order}
```

```yaml
# b-verify-portal.flow.yaml — the portal must show what SAP minted
name: Order appears in the portal
app: web
url: ${PORTAL_URL}/orders
steps:
  - Type ${ORDER_NO} into the "Search" field
  - assert: page shows ${ORDER_NO}
```

Each export is `ENV_NAME: template`. The template may carry
`${captured.<name>}` references (this flow's captures) and plain `${VAR}`
references (the environment, resolved like suite `env`). When the flow's
last step has passed, the templates resolve and the pairs become
environment variables for the remaining flows — which reference them as
ordinary `${VAR}`s, so the downstream trace stores only the reference and
resolves it fresh on every replay. The handoff happens at REPLAY time, from
replay-time captures: flow B replays against the value flow A's replay just
read, not against a value frozen at record.

What holds, and why:

- **Nothing is persisted.** Like a capture, an exported value exists only
  in the memory of the run. The trace holds the capture name, the run
  report and the `[EXPORT]` line hold the export NAME — an order number or
  balance stays out of committed artifacts and CI logs alike.
- **An export that cannot resolve fails the flow that owns it.** A
  `${captured.<name>}` never remembered fails THIS flow with the captures
  that were in scope — not the downstream flow, which would otherwise fail
  holding a variable nobody visibly set. And a failed flow exports
  nothing: no partial contract.
- **A single `flowproof run <spec>` resolves exports too**, though there is
  no downstream flow to receive them — the verdict must not depend on
  whether the flow ran alone or in a suite.
- **`app: agent` flows cannot export** (a parse error): they record at the
  model boundary and have no captures. Chain them as consumers — an agent
  flow's spec can reference `${ORDER_NO}` like any other.

The suite's existing machinery composes: `env_from` mints the data the
FIRST flow needs, `order:` in `suite.yaml` pins who runs before whom, and
`exports:` carries what a flow LEARNED to whoever follows.

### iframes (same-origin, assertions)

An element inside an iframe is addressed with the same target-tail shape as
a container scope:

```yaml
- assert: the "css:#total" in the iframe "checkout" shows Total 42.00
- assert: the "Status" inside the iframe "checkout" is visible
- assert: the "css:#total" in the iframe "css:iframe[title=checkout]" shows Total 42.00
```

The frame names itself the way any target does: a quoted anchor matched
against the iframe's own `title`, `name`, `id`, or `aria-label`, or an
explicit `"css:<selector>"`. The phrase is cut out of the tail like the
container phrase, so every predicate composes without special casing, and a
role noun still goes before it.

**The frame is a fence, not a hint.** The inner target is looked up in the
frame's own document and nowhere else: if it is not in the frame, the
assertion fails even when an identically named element sits on the page
outside it. That is the whole point - a scope that silently fell back to
the main document would pass green on the wrong element.

Three failures are kept distinct so none of them can read as a pass:

| Situation | What happens |
|---|---|
| the named iframe is not on the page | fails naming the frames that ARE there (`iframe 'invoice' was never found (iframes present: checkout, receipt)`) |
| the iframe is cross-origin | the run ERRORS - the same-origin policy walls off the document, so the assertion cannot be checked, and it is never silently passed |
| the element is not inside the frame | an ordinary miss, reported as `inside iframe '<frame>'` so it is not confused with a page-wide miss |

Limits in v1, each for a reason rather than for later:

- **Value-driving actions, not pointer actions.** `Type`, `Replace`, `Clear`,
  `Check`/`Uncheck`, `Remember` and `Scroll` work inside a frame; `Click`,
  `Press … button`, `Hover`, `Double-click`, `Right-click` and `Upload` are
  a parse error naming the reason.

  The original refusal covered every action, on the grounds that actions act
  at composited coordinates resolved against the main document and so could
  "succeed" without touching the frame. That reasoning was right, and it is
  specifically about COORDINATES. A same-origin frame does not need them: the
  parent's own scripts can reach `iframe.contentDocument`, so a value action
  is driven through the frame's DOM - the same mechanism `Select` uses in the
  main document - and nothing is dispatched at a point.

  A pointer action has no such route. It could only reach the frame as an
  untrusted event (`isTrusted` is false), which an application is free to
  ignore while the step still passes - release-without-effect. So those stay
  refused until a trusted mechanism exists.

  **A framed `Type` is not the main-document `Type`.** In the main document
  it is real keystrokes; inside a frame it is a value assignment plus
  `input`/`change`. An application that filters on `keydown` will not see it.
  Two guards keep that honest rather than silent: the target must not be
  `disabled` or read-only (a value assignment succeeds on a disabled control
  where typing would be ignored - so it is refused by name), and the value
  is read BACK from the element afterwards, so a control that rejected or
  rewrote it fails the step.
- **Same-origin only.** A cross-origin frame's document is unreachable, and
  the CDP per-frame execution-context path is not deterministic enough to
  ship behind a grammar that looks identical.
- **One frame, no combining.** A frame scope cannot be nested inside a
  container or cell scope yet; one context per target.
- An ordinal cannot address a frame (`the 2nd iframe`): name it.
### Cookie controls (web, security)

"The session cookie is httpOnly" is a control that regresses SILENTLY: an
auth library config changes, the cookie becomes readable by page scripts,
and nothing about the UI looks different. These assertions pin it.

```yaml
control:
  id: sec.session.cookie-flags
  title: The session cookie is not readable by page scripts
steps:
  - assert: cookie "session_token" exists
  - assert: cookie "session_token" is httpOnly
  - assert: cookie "remember_me" is persistent
```

**A cookie's VALUE cannot be asserted, and never will be.** A session
cookie's value is a credential. There is no `cookie "x" is <value>` form,
no `contains`, and no redacted comparison: the moment a value can be
compared, the expected value has to live in the trace and the failure
message tempts someone to print the actual one. flowproof's traces are
meant to be safe to commit and safe to attach to a bug report. A failure
names the cookie, which fact failed, and - for a missing cookie - the NAMES
of the cookies that were set, which is what fixes a typo.

**The `is secure` honesty note.** Browsers exempt localhost from the secure
requirement, so `is secure` can pass over plain http and certify nothing
about production. The step still passes, because teams do run
TLS-terminated staging, but the run prints a warning saying it does not
certify production behaviour. Read that warning as a finding: a control
that has only ever passed over http is an unverified control.

Out of v1: `sameSite` (three-valued, so it does not fit the `is <flag>`
shape), exact expiry timestamps (nondeterministic across record and
replay), and domain/path matchers.

## Repeating a block (`foreach`)

A block that repeats with one value changing collapses into a `foreach`
values matrix. Scalars are referenced with `${each}`, mappings with
`${each.<key>}`; a whole-string token keeps its YAML type, so
`status: ${each.status}` stays a number. Expansion happens at parse time -
each iteration becomes an ordinary recorded step, so a `foreach` adds no
runtime construct to the trace.

```yaml
steps:
  - foreach:
      values: [mysql, mssql, oracle]
      steps:
        - assert_api:
            request: POST ${API}/connections/test
            body: { type: "${each}" }
            status: 500
```

## Repeating until the app settles (`repeat:` and `when:`)

`foreach` repeats a block as many times as you know when you write it.
Sometimes you do not know — press a button until the label changes, recover
if an error appeared. Those are `repeat:` and `when:`.

```yaml
steps:
  - repeat:
      until: the "id:button" shows Enough
      max: 15
      steps:
        - Press the "id:button" button
  - when: the "id:b1" is not visible
    steps:
      - Press the "id:tech" button
```

**Both expand while recording, not while replaying.** The condition is read
against the live app, and what lands in the trace is the passes that
actually ran — ordinary concrete steps, no `repeat` and no `when`. The trace
stays a recording of what happened and replay still decides nothing. Against
a non-deterministic application that recording only replays against the same
behaviour, which for a regression test is the right way round: a flow that
silently re-adapted every run would always pass.

`until:` is checked **before** the first pass, so a `repeat:` whose
condition already holds runs zero times. `max:` is required: if the
condition never holds within it, recording fails and names the bound. Each
`repeat:` gets its own budget.

Conditions read state; they never wait:

| Condition | Holds when |
|---|---|
| `page shows <text>` / `page does not show <text>` | the whole surface's text does or does not contain it |
| `the "<target>" shows <text>` | that element's text contains it |
| `the "<target>" is visible` / `is not visible` | it is on screen, or is missing or hidden |
| `the "<a>" is greater than the "<b>"` / `is less than` | both read as numbers, and the ordering holds |

A missing element makes a positive `shows` false and a negative one true —
the same reading replay takes. Anything else is refused by name.

The comparison is the one condition that weighs two readings against each
other rather than a reading against a literal, and it is **numeric**: `"9"`
is greater than `"10"` as text and smaller as a number, and a condition that
quietly answered the text question would be worse than one that refuses. A
side that does not read as a number fails the recording and is quoted back.

Scope conditions tightly: `page shows ERROR` also matches a heading reading
"Errors occur", so name the element instead.

## Driving an arbitrary Windows app (`app:` mapping, `window:` config)

`app:` is normally a registry id (`web`, `calc`, `notepad`, `sap`, `vision`,
`api`). It also accepts a mapping, which drives any Windows program through
UI Automation:

```yaml
app:
  command: '"C:\Program Files\My App\app.exe" --profile=test'
  window_title: ${APP_WINDOW}
window:
  width: 1280
  height: 800
```

`command` is a command LINE, not a program name: the program may be quoted
so a path with spaces survives, and everything after it reaches the app
verbatim. Both fields take `${VAR}` references, resolved at launch and
stored RAW in the trace. `command` is executed code, the same trust surface
as a suite's `env_from`: a spec is code.

`window:` pins the window's shape, which is a determinism precondition for
visual assertions rather than something a user does - so it is config,
applied once before the first step and identical at record and replay, not a
step. `width` and `height` go together; `x` and `y` are optional but go
together and need a size. Geometry values are literal integers, never
`${VAR}`: a precondition that varies by environment is not one. The trace
records what was APPLIED, so a spec that gives only a size still pins the
position the window landed on.

A vision flow names the window it attaches to in the same block, and may
pin geometry too - which is where it matters most, because OCR baselines
depend on it:

```yaml
app: vision
window:
  title: Citrix Receiver
  width: 1280
  height: 720
```

Each app kind has exactly ONE spelling for naming a window:
`app.window_title` for a Windows program flowproof launches, `window.title`
for a window vision attaches to but never launched. Using the wrong one is a
parse error that names the right one. A web flow sizes its page with
`browser: viewport`, and an api flow has no window at all.

### UWP and packaged apps

A UWP app (Calculator, Settings, anything from the Store) is not an exe you
launch by path. Launch one through the shell, naming the package by its
Application User Model ID:

```yaml
app:
  command: explorer.exe shell:AppsFolder\Microsoft.WindowsCalculator_8wekyb3d8bbwe!App
  window_title: Calculator
window:
  width: 640
  height: 900
```

`explorer.exe` returns immediately, before the app has a window, which is
exactly why `window_title` exists: flowproof waits for a window with that
title rather than for the process it spawned. List the ids on the machine
with `Get-StartApps` in PowerShell.

The window matters for geometry. A UWP app draws into a
`Windows.UI.Core.CoreWindow` hosted inside an `ApplicationFrameWindow` that
belongs to `ApplicationFrameHost.exe`, and the CoreWindow does not own its
own size - resizing it does nothing visible. flowproof detects the
CoreWindow class and applies `window:` to the hosting frame instead, so a
UWP flow pins its shape like any other. Nothing to configure; worth knowing
only when a resize appears to be ignored.

For running a UWP app on a CI runner that does not ship one, see
[Deploying a UWP app on a CI runner](getting-started.md#deploying-a-uwp-app-on-a-ci-runner):
a Windows Server image has no Store apps, but it can build and side-load
the one a suite needs.

## Out-of-band assertions (any app; structured steps, not prose)

```yaml
- assert_sql:
    connection: reporting        # resolved from FLOWPROOF_SQL_REPORTING
    query: SELECT count(*) FROM orders WHERE ref = '4711'
    equals: "1"
- assert_api:
    request: GET ${API}/orders/4711
    status: 200
    body_contains: "confirmed"
- assert_api:                    # authenticated JSON POST
    request: POST ${API}/connections/test
    headers:
      Authorization: Bearer ${SESSION_TOKEN}
    body:
      provider: postgres
      connectionString: ${TEST_CONN_STRING}
    status: 200
    body_contains: "Database not yet supported!"
- assert_api:                    # response-side JSON-field assertion
    request: GET ${API}/testData/users
    status: 200
    body_json: results.0.balance # a dotted path into the JSON response
    equals: 150953               # the leaf at that path must equal this
- assert_api:                    # how many elements are in a collection
    request: GET ${API}/testData/users
    status: 200
    body_json: results           # the path must resolve to an ARRAY
    count: 5                     # exactly 5 elements (count_at_least: 2 = a minimum)
- assert_api:                    # response-header assertion
    request: GET ${API}/testData/users
    status: 200
    header: Content-Type         # response header name (case-insensitive)
    header_contains: json        # a substring of the header value
```

`headers` values and `body` string values may carry `${VAR}` refs. The
trace stores only the raw reference; tokens and connection strings resolve
when the probe fires (record and every replay). `body` is any YAML
(mapping, list, or string), sent as JSON with `content-type:
application/json` unless you set your own `content-type` header (yours
wins). A `body` on GET/HEAD/DELETE is rejected at parse time.

`body_json` reads a value out of the JSON response and asserts on it,
alongside `status` and `body_contains` (all three may appear on one step;
they are checked in the order status, then body_contains, then body_json).
The path is a dotted sequence of segments, each a plain object key or a
decimal array index: `results.0.balance` means "the `balance` field of the
first element of the `results` array". That is the whole path language:
there are no wildcards, filters, brackets, or quoting, so a key that
literally contains a dot cannot be reached. One `body_json` per step; to
assert several fields, use several steps.

`body_json` on its own is an existence check: the path must resolve to a
scalar leaf (mirroring `assert_sql`, where omitting `equals` means a row
merely has to exist). Add `equals` (a string, number, or boolean) to also
check the value; `equals` without `body_json` is a parse-time error. A
string `equals` may carry a `${VAR}` ref, resolved at probe time exactly
like `body_contains` (only the ref travels in the trace). Comparison has
two tiers: when both the leaf and `equals` are numbers, they compare
numerically (`150953` equals `150953`); otherwise they compare by exact
canonical text, so a string leaf never numeric-matches a number (the
leaf `"0953"` does not equal the number `953`). Only leaves compare, so
there is no deep object equality.

The extracted response value never enters the trace: only the request and
the raw expectation are stored, and the plucked value exists solely inside
the live comparison, re-fetched on both record and replay. The failure
modes are soft (the auto-wait loop keeps polling until they clear or the
timeout elapses): a non-JSON body reports "response body is not valid
JSON"; a path that runs off the document names the segment where it died
(`path 'results.0.balance' stops at segment 'balance'`); a path that lands
on an object or array reports "path resolves to a non-scalar; assert a leaf
value".

`header` asserts on a response header, alongside `status`, `body_contains`,
and `body_json` (all may appear on one step; they are checked in the order
status, then body_contains, then body_json, then header). The header NAME is
case-insensitive, per HTTP: `header: Content-Type` matches a response that
spells it `content-type`. If the response repeats the header, its values are
joined with ", " (HTTP field-value semantics) before matching. One header per
step; to assert several headers, use several steps.

`header` on its own is an existence check: the header must be present
(mirroring `body_json` alone, where reaching a scalar leaf is the whole
assertion). Add `header_equals` (exact value) or `header_contains` (a
substring) to also check the value; at most one of the two per step, and
either without `header` is a parse-time error. Value comparison is
case-SENSITIVE (unlike the name). A `header_equals`/`header_contains` value
may carry a `${VAR}` ref, resolved at probe time exactly like `body_contains`
(only the ref travels in the trace). The live header value never enters the
trace: it exists solely inside the comparison, re-fetched on both record and
replay. The failure modes are soft: an absent header reports "response has no
'<name>' header (status <code>)", and a value mismatch reports "header
'<name>' is '<actual>', expected <equals|contains> '<want>' (status <code>)".

`count` (exactly N) and `count_at_least` (a minimum) ask how many elements
are in the array at `body_json`. Either requires `body_json`, at most one of
the two may appear, and neither pairs with `equals` (a count needs an array,
`equals` needs a scalar leaf) - all three are parse-time errors. When the
path resolves to something other than an array, the failure names what was
actually there: "path 'page' is an object, count requires an array (status
200)". A wrong count reports both sides: "path 'results' has 3 elements,
expected exactly 9 (status 200)". Both are soft failures, so on a `GET` they
auto-wait: "poll until the collection has N rows" is a real pattern.

### Retries: reads are polled, writes are sent once

A failing assertion auto-waits by RE-SENDING its probe until the bound
expires. That is right for a read (the API is still converging) and wrong
for a write, because the probe IS the mutation: polling a failing `POST`
delivers it once per tick, and a single failing step was measured
delivering 41 `POST`s inside the default 10s bound. So only `GET` and
`HEAD` are retried. `POST`, `PUT`, `PATCH` and `DELETE` are sent exactly
once, and their failure says so. (`DELETE` is idempotent per HTTP but not
side-effect-free, so it is grouped with the writes.) `assert_sql` is a
read and keeps polling.

Override per step when the default is wrong:

```yaml
- assert_api:                    # poll a write until it converges
    request: POST ${API}/jobs
    status: 202
    retry: true
- assert_api:                    # ask a read exactly once
    request: GET ${API}/jobs/1
    status: 200
    retry: false
```

On releases without `retry:`, `timeout_seconds: 0` is the mitigation: it
leaves no wait budget, so the probe fires once.

## Visual assertions (structured step)

```yaml
- assert_screenshot:
    name: dashboard              # baseline PNG name (no path, no extension)
    mask: ["css:.clock", "Sync"] # optional: selectors blanked before compare
    threshold: 0.001             # optional: fraction of pixels allowed to differ (default 0)
```

`record` captures the surface, blanks each mask's element rect, and mints
`<spec-stem>.baselines/<name>.png` next to the trace — re-recording (or
`record --reuse`) is how baselines refresh. Replay captures with the
**same masks** and compares pixel-exact (up to `threshold`); on failure
the run bundle gains `visual/<name>.actual.png` and `visual/<name>.diff.png`
(differing pixels in red) and the message names the diff percentage.
Masks take the same forms as quoted labels (text anchor, `css:`, `id:`)
and every mask must resolve — a silently-unmasked volatile region would
mint a flaky baseline. Pin the viewport with `browser:` so capture
dimensions stay stable across machines.

## Network mocks (web flows; spec-level, not steps)

```yaml
mock:
  - url_contains: /api/rates          # substring match on the request URL
    method: GET                       # optional; any method when absent
    status: 200                       # optional; default 200
    body:                             # any YAML: string served verbatim
      rate: 1.23                      #   (text/plain), anything else as
      source: mocked                  #   JSON; content_type: overrides
```

Requests matching a rule are answered inside the browser — the real host
is never contacted (it need not even exist). The rules travel in the
trace header and apply **identically at record and replay**: what was
mocked once is mocked always, which is what keeps the two executions
equivalent. Mocked responses carry permissive CORS headers and answer
preflights, so cross-origin `fetch()` calls just work. The tool for
third-party calls (payments, analytics) and hard-to-provoke server
states; for asserting on real APIs, use `assert_api` instead.

## Browser config (web flows; spec-level, not steps)

```yaml
browser:
  viewport:                   # device emulation, applied before navigation
    width: 390
    height: 844
    device_scale_factor: 3    # optional; default 1
    mobile: true              # optional; mobile layout + meta-viewport
    touch: true               # optional; emulate a touch screen
  user_agent: my-agent        # optional; navigator.userAgent override
  args: ["--lang=en-US"]      # optional; extra Chrome flags
  clock:                      # optional; pin the clock (GAP-P)
    at: "2026-01-15T12:00:00Z"   # required; RFC 3339, a mid-day time
    timezone: "Europe/Berlin"    # optional but recommended; IANA id
```

The config travels in the trace header and applies **identically at
record and replay** — a flow recorded on an emulated phone never replays
on a desktop viewport. This is how `*.mobile` test variants and
deterministic-seeding user agents (previously an env-var wrapper around
Chrome) become first-class. `args` forces a private (non-shared) browser
for the flow, since flags only apply at process start — expect its cold
start. A suite's `suite.yaml` may carry the same `browser:` block as a
default for every flow; a flow's own block wins outright.

### Pinning randomness

`browser.random` replaces the page's `Math.random` with a seeded PRNG,
injected before any page script for the same reason the clock's shim is: a
page that has already drawn a random number cannot be un-randomised
afterwards.

```yaml
browser:
  random:
    seed: 1234
```

Same argument as the clock, applied to the other source of per-run drift. A
page that mints a value from `Math.random` shows something different every
run, so the only honest thing to write against it is another read — and for
a value the flow must ENTER rather than compare, there is nothing to read.
Pinned, the value is a constant you can write by hand, and record and every
replay see the same one.

`seed` is a **literal**, never a `${VAR}`: a seed resolved from the
environment would make one trace mean different things on different
machines, which is the drift pinning exists to remove. Web-only; a
`random:` block on any other app kind is a spec error naming the
restriction, because there is no `Math.random` to pin on a desktop window
or an OCR frame.

Deliberately narrow, and stated rather than discovered:
`crypto.getRandomValues` is untouched (it is a security primitive, not a
convenience), web workers get their own real `Math.random`, and
server-side randomness is `mock:`'s job.

**The seed pins the SEQUENCE, not the position.** The page draws the same
series of numbers every run; which of them reaches the value you care about
depends on how many draws the page made first. A page whose earlier scripts
draw a *variable* number of times — an animation that fires once or twice
depending on timing — can still hand you a different value, taken from the
same series one place along. Observed once in the wild against a page that
generates on focus: the value was stable across six runs and shifted by
exactly one draw on a seventh.

So **assert the drawn value before you use it**:

```yaml
- assert: the "id:no1" shows 91        # the draw itself
- assert: the "id:no2" shows 98
- Type 189 into the "id:result" field  # the constant derived from it
```

Without the first two lines a shifted sequence types a confidently wrong
answer and fails somewhere else entirely, or worse, passes. With them, the
shift is the failure — which is the whole reason a pinned value is worth
asserting even though it is "constant".

### Pinning the clock

`browser.clock` freezes what the page reads as "now", so a date-dependent
flow is deterministic — a "last 7 days" filter, a "renews in N days"
label, a relative timestamp, a picker that opens on the current month. The
clock STARTS at `at` and advances at real wall rate (it is a fixed offset
on `Date`, not a hard freeze), so pick a **mid-day** `at` and no step will
straddle a pinned midnight. Both fields are literals, never `${VAR}`: a
precondition that varied by environment would not be one. Set `timezone`
whenever you set `at` — without it, local dates and week boundaries still
depend on the runner's zone.

What it does NOT cover, by design:

- **server-side "today"** — a date the SERVER computes (an SSR page, an API
  returning a relative window) is untouched; pin those with a `mock:` rule
  instead.
- **web workers** see the real clock; only the main frame's `Date` is
  pinned.
- **`performance.now()`** and timer scheduling are not shifted.

Clock control is web-only; a `clock:` block on any other app kind is a
parse error.

## Agent flows (`app: agent`)

An `app: agent` flow tests an AI agent at the model boundary rather than a
UI, so it has its own small step vocabulary, documented in full in
[agent-testing.md](agent-testing.md). Unlike the forms above, these are
structured steps that either parse or error; they do NOT fall back to the
LLM author. The step forms:

| Step | Meaning |
|---|---|
| `prompt: <text>` | the task handed to the agent; several `prompt:` steps are joined into one turn |
| `assert_tool_call: <tool> [where <path> <matcher> <value> [and …]]` | a tool call the agent must make. Matchers: `equals` (alias `is`), `contains`, `matches` (regex), `exists`, `is absent` |
| `assert_no_tool_call: <tool> [where …]` | a tool the agent must NOT call anywhere in the trajectory |
| `assert: reply contains <text>` | the final assistant message contains `<text>` |

`agent:` (command/env), `tools:` (the boundary mocks), and `strict:` are
spec-level config, like `mock:` and `browser:` above.

## App sugar

Sugar is an alias layer, not a cage: on every UIA-driven app (`calc`,
`notepad`, and the `app:` mapping form) the full shared action grammar
applies too — `Press the "<label>" button`, `Click "<text>"`, `Type <text>
into the "<label>" field`, `Press Ctrl+S`, `id:` targets and ordinals all
act on any control the app shows, menus and dialogs included. Sugar wins
where it matches; everything else falls through to the shared forms.

- **calc**: `Type <digits>` (one press per digit), `Press
  plus|minus|times|divided by|equals`, `assert: display shows <number>`.
  Keys the sugar never named are shared-grammar presses: `Press the
  "Square root" button`, `Click "History"`.
- **notepad**: `Type <text>` types into the *document*; the targeted form
  `Type <text> into the "<label>" field` addresses a dialog's field (Find,
  Replace, Save As) instead. `assert: document contains <text>` (plus the
  shared grammar).

## Security controls

A security control is not a special kind of test. It is a property that
must hold, expressed as an ordinary deterministic assertion over a recorded
flow: a viewer cannot delete, a secret never surfaces in output. The forms
below add just enough to NAME a control stably and to assert one class of
"this must never appear" that the shared grammar could not spell before. The
access-control pattern needs no new step at all (see below); it is composed
from grammar you already have.

What v1 ships, stated plainly so nothing here is mistaken for more:

- The `control:` block on any flow (a stable id for coverage).
- `assert_no_secret_leak: ${VAR}`, the **named form only**, on `app: agent`,
  `app: web`, and `app: api` flows (the scanned corpus is the agent trajectory,
  the page surface text, or the `assert_api` response bodies). A flow kind with
  no readable corpus fails as a capability error, not a vacuous pass.
- `flowproof audit`, the control map. It reads the structured run record
  `flowproof run` persists (never re-replays), renders each control-bearing
  flow's verdict with an evidence pointer, and with `--since <run-id>` diffs
  two runs by control id (added, removed, verdict-changed).

### Naming a control: the `control:` block

A flow-level block, at most one per flow, gives the control a stable id:

```yaml
name: A viewer cannot delete a customer
app: web
url: ${APP_URL}/customers
control:
  id: ac.customers.delete.viewer-denied      # required
  title: Viewer role is denied customer deletion   # optional
  description: >-                              # optional
    The viewer session may read a customer but the API refuses its DELETE.
steps: [ ... ]
```

The `id` is author-chosen, dotted, lowercase (`[a-z0-9._-]+`); a value with
whitespace or an out-of-range character is a parse error. Its one hard job
is STABILITY: it survives renames of the flow file, moves between suites, and
re-records, because it is the join key between what an auditor tracks and
what CI ran. `title` and `description` are author metadata. A recommended
(not enforced) convention for the id is
`<domain>.<resource>.<action>.<expectation>`. Teams mapping to an external
framework (SOC 2, ISO) keep that mapping in their own catalog keyed by the
id; flowproof models no compliance ontology, it provides the stable key.

**Uniqueness is a suite property.** Two flows in one suite sharing a control
id is a suite-load error naming BOTH flows, because a duplicated join key
would corrupt the coverage map. A lone `flowproof run` on a single flow sees
only that flow, so it neither checks nor needs uniqueness.

### Access-control regression (a pattern, not a step)

The highest-value control in practice is "identity X must be denied action
Y". It is NOT a new `assert_no_*` subject. "Unauthorized access" is not a
lane the engine observes; it is an attempt the flow performs plus a denial
the shipped grammar already asserts. So the flow is three ordinary moves:
become the identity, perform the attempt, assert the denial.

The one rule that makes it a real control: **a denial is only evidence when
the same run proves the identity was alive.** If the app returns `403` for
both an unauthorized-but-valid session AND a dead one (an expired token, a
logged-out browser), then a credential that quietly expired reads as a
PASSING control while testing nothing. So a denial flow MUST also assert that
the identity is entitled to succeed at something: a `200` on an action it is
allowed, or a UI fact only the signed-in session shows. A denial flow with no
liveness assertion is an incomplete control.

The worked example lives at
[`examples/access-control/`](../examples/access-control/): a `suite.yaml`
declaring identities and a `viewer-cannot-delete.flow.yaml` that carries the
liveness proof and the denial side by side. See it for the full flow.

### `assert_no_secret_leak: ${VAR}` (v1)

The engine already guarantees the TRACE never stores a secret (`${VAR}`
resolves at the moment of use, only the reference is written). That protects
flowproof's own artifacts. It says nothing about the APP under test, which
can render a connection string into an error or echo a token into a response.
That is the leak this control catches.

v1 ships the **named-selector form only** (one `${VAR}`, or a list):

```yaml
- assert_no_secret_leak: ${DB_PASSWORD}        # one named secret
- assert_no_secret_leak:                       # or several at once
    - ${DB_PASSWORD}
    - ${API_TOKEN}
```

Semantics, all inherited from the shared grammar:

- **The lane is the run's captured outputs.** Which outputs depends on the
  flow kind (detailed below): a closed corpus, not "everything", so the control
  can name what it checked. Channels the engine never observed (server logs,
  third-party sinks) are out of scope and the audit output says so.
- **The forbidden event is an occurrence of the resolved secret value** in
  that corpus. At execution (record) and on every replay, each asserted
  `${VAR}` is resolved through the same resolve-refs machinery and the
  in-memory corpus is substring-scanned for the resolved value. The trace
  stores only the variable NAMES; the value is never written or printed.
- **Whole-run scope.** Position in `steps:` does not narrow it.
- **Only names travel.** A failure names every matching variable (in a stable
  order, so a run leaking two secrets reports both), the corpus element it
  appeared in, and the step index. It never prints the value.
- **A secret too short to scan is refused, not weakened.** A resolved value
  under a small minimum length (4 characters) fails the run at execution, in
  the same shape as the `MissingSecret` error, naming the variable and the
  minimum but never the value (scanning for `"1"` would fire on any page
  showing a 1).

**Bonus: the record-time scan is a store-guard.** On an agent flow the
model-boundary trajectory is persisted into the trace as a cassette, so a
leaked secret would otherwise be written to disk. The scan runs BEFORE the
trace is minted, so a leak fails the run and NO trace is written: the leaked
secret never reaches disk. Determinism holds because the corpus is
re-observed by the same mechanism at both phases, so an unchanged system
yields the same scan and the same verdict.

The corpus depends on the flow kind: an `app: agent` flow scans the
model-boundary trajectory and its MCP lanes; a `web` flow scans the surface
text read at each step boundary (not page source, and not continuously
between steps); an `api` flow scans each `assert_api` response body. A flow
kind with no readable corpus fails as a capability error rather than passing
vacuously.

One thing is deliberately NOT in v1: the **bare** form ("scan for every
`${VAR}` the flow referenced") is deferred until a suite-level `secrets:`
declaration gives it a defined domain (`${APP_URL}`, `${API}`, and minted
test data legitimately appear in output, so a bare scan would false-fail on
nearly every flow).

### `flowproof audit`: the control map

A suite run already yields per-flow verdicts and writes one structured run
record at `.flowproof/runs/<run-id>/report.json`. `flowproof audit <dir>` READS
that record and folds the flows that carry a `control:` block into a
control-coverage report. It never re-replays: the verdicts come from the record
`flowproof run` wrote, so audit is a pure rendering and stays fast and
side-effect-free. If no run has been recorded yet, audit refuses with an error
pointing you at `flowproof run` rather than silently re-running anything.

```text
$ flowproof run examples/access-control              # writes the run record
$ flowproof audit examples/access-control            # YAML on stdout
$ flowproof audit examples/access-control --json     # JSON instead
$ flowproof audit examples/access-control --run <id> # a specific past record
```

```yaml
suite: access-control
run: 2026-07-24T09-14-03Z-a1b2
controls:
  - id: ac.customers.delete.viewer-denied
    title: Viewer role is denied customer deletion
    flow: viewer-cannot-delete.flow.yaml
    verdict: pass
    evidence:
      trace: viewer-cannot-delete.trace.jsonl
  - id: sec.assistant.no-db-password-leak
    title: The DB password never surfaces in agent output
    flow: assistant-no-leak.flow.yaml
    verdict: pass
    lanes: [secret_leak]
    evidence:
      trace: assistant-no-leak.trace.jsonl
    secrets_checked: ["${DB_PASSWORD}"]        # variable names, never values
    corpus:
      - model-boundary trajectory (cassette request and response bodies)
      - MCP lanes
    excluded:
      - channels the engine never observed (server logs, third-party sinks)
```

Each control row carries an `evidence` pointer to the trace its proof lives in
(and, for a contained agent flow, any egress destinations containment blocked),
so a reader can go from the coverage map to the underlying artifact. Blocked
destinations appear only when THIS run was contained: they are read from the
recorded trace, so a recording made under containment and replayed on a host
without it would otherwise present another machine's blocks as evidence here.

A flow that engages egress also carries `containment:` - the tier the run
actually ran under (`enforced (linux seccomp)`, or the honest reason it was
not). `lanes` says what the flow ASSERTED; `containment` says what was
ENFORCED. On a host where the mechanism does not exist the flow can still
pass, so without this field a passing row would imply a certification the
run never made.

**Diffing runs.** `flowproof audit <dir> --since <run-id>` compares the latest
record against an earlier one, folded by `control.id`: controls **added**,
controls **removed** (present in the older record, gone in the newer - coverage
that shrank), and controls whose **verdict changed** (old -> new). It exits
non-zero on a regression - a removed control or a control that changed to
`fail` - so CI catches coverage silently shrinking.

```text
$ flowproof audit examples/access-control --since 2026-07-24T09-14-03Z-a1b2
```

```yaml
base: 2026-07-24T09-14-03Z-a1b2
head: 2026-07-24T11-02-55Z-9f3c
added:
  - id: ac.orders.refund.viewer-denied
    verdict: pass
removed: []
changed:
  - id: sec.assistant.no-db-password-leak
    old: pass
    new: fail
```

Three verdicts, kept distinct so a report can never launder "we could not
check" into "it held":

- `pass` - the control held on replay.
- `fail` - the control did not hold. `flowproof audit` exits non-zero when
  any control failed.
- `capability-error` - the platform could not enforce or observe the lane,
  or the flow never ran (a missing trace is a capability error naming the
  `flowproof record` to run, never a silent pass).

`secrets_checked` / `corpus` / `excluded` appear only for a flow that ran a
secret-leak scan. The audit surface is a stable file external tooling can
ingest, sourced from the persisted run record at
`.flowproof/runs/<run-id>/report.json`. Both once-absent pieces now ship on top
of that record: **evidence pointers** (the `evidence.trace` on each control row)
and **cross-run report diffing** (`audit --since <run-id>`, including
removed-control detection). Retention keeps the most recent 10 records per
suite, pruned after each run, so the `--since` window stays bounded.

## When a step cannot be authored

In auto mode, plain freeform UI text (for example, `Smash the shiny
button`) is model intent. The model receives the live scene and must ground
its answer to one of the listed target tokens; it cannot invent a selector.
An explicit `rules:` step instead succeeds or fails against the grammar on
this page and names the accepted forms for that app. Use `--author rules`
or `--author llm` when the entire recording should force one backend.

When auto mode has no configured model (`FLOWPROOF_AI_PROVIDER` /
`FLOWPROOF_AI_API_KEY`), the CLI emits a visible warning before trying the
deterministic grammar. This fallback is identified as its own `fallback` route in
the per-step human and structured diagnostics, so ordinary prose is never
silently mistaken for deliberate rule syntax.

When a step is too *ambiguous* to author at all ("make required field
changes" — which fields? — or `Enter it` with several remembered values),
recording fails with a structured **clarification payload**: the stuck step
plus the relevant live-scene fields or remembered-value candidates. It is
available via `record --json`, the MCP record tool, or Python's
`ClarificationNeeded`. The driving agent rewrites the step more precisely
and re-records — see [self-help.md](self-help.md) for the loop.

Whichever route authors a step, recording persists grounded selectors and
actions in the trace. `flowproof run` executes those deterministic artifacts
directly and makes zero authoring-model calls. See
[getting-started](getting-started.md#authoring-with-a-model-arbitrary-steps).
