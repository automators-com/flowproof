---
title: "Actions"
description: "The generic action grammar shared by web, SAP, and vision flows, including native-dialog handling."
---

| Step | Notes |
|---|---|
| `Type <text> into the [2nd ]"<label>" field` | text anchor / `css:` / `id:`. FILL semantics: the field ends up reading `<text>` exactly, whatever it held - the choice every mainstream tool makes. The keys are still real keystrokes (typed over a select-all), so keydown-filtering apps behave as with a person typing |
| `Type <text> into the <id> field` | bare native id. Fill semantics, as above |
| `Type <text>` | types into the FOCUSED element - raw keystrokes, APPENDING. The step for dropdown filters, pre-focused rename boxes, and adding to what is there |
| `Replace the [2nd ]"<label>" field with <text>` | clear + type, one step. Same result as `Type into` now states; kept for specs that say it explicitly |
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
| `Wait until the download completes as <name> [within <N>s]` | web only; no target, like `Press <Key>` — a download belongs to the surface, not an element. Waits for exactly one browser download to land and finish writing, then captures its RESOLVED PATH into `${captured.<name>}` — read at execution time on record and every replay, so only the name travels in the trace, the same indirection every `Remember … as` capture uses. Pairs with a `browser: {downloads_dir: …}` block to pin where downloads land (optional — the driver creates its own per-launch temp directory otherwise), and with `assert_spreadsheet` or a later surface's launch command (`EXCEL.EXE ${captured.<name>}`, see [Multi-surface flows](#multi-surface-flows-apps-and-in-blocks)) to act on the file it names |

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
