# Vision on a host terminal screen

A client's legacy application runs inside nested Citrix sessions as a **host
terminal screen**: a fixed monospace character grid of protected text and
white input fields, with a PF-key legend, a message line and a status bar
showing the cursor's row and column. Its "tables" are header lines with rows
of input fields under them, one screen line per row. People move through
them with Tab, not the mouse. Vision could neither read nor fill them.

There is no such screen to test against here, so this directory has one.

## The replica

`replica/` is a small Windows program (Rust, drawn with GDI) that shows a
layout from YAML and behaves like a host terminal:

- **Tab / Shift+Tab** move between input fields in screen order and wrap.
- **Overwrite mode**: typing replaces characters; the old tail stays.
- **Auto-skip**: typing the last character of a field moves to the next one,
  so a Tab after a full field skips a field.
- **Erase EOF** (`Ctrl+End` by default) clears from the cursor to the end of
  the field.
- **Enter** is a round trip to the host: the keyboard is locked for
  `enter_delay_ms` (the status bar reads `Dialog wartet`; keys are dropped),
  then a new empty line opens in the table the cursor is in (`add_row`) or
  only the message line answers (`submit`).
- **PF keys** page a table that holds more rows than it shows (PF8/PF7 by
  default); PF13-PF24 are Shift+F1-F12.

What the keys do differs between emulators, so each of these is set under
`keys:` in the layout. `leistung.yaml` is the client's invoice-entry screen,
rebuilt from a screenshot with invented values.

Options make each frame look as it would after Citrix: `--scale 0.8`
(shrink and stretch back), `--bits 16` or `--bits 8` (colour depth) and
`--jpeg 35` (compression). `--state out.json` writes what is on screen after
every change: every field, every table row, the cursor and the screen text.
That file is the ground truth the evals score against.

```bash
cargo build -p flowproof-terminal-replica
target/debug/flowproof-terminal-replica evals/terminal-grid/leistung.yaml --state state.json
```

## The evals

```powershell
powershell -File evals/terminal-grid/run.ps1 -Engine <flowproof.exe> [-Only <flow>]
```

Each flow in `flows/` is recorded with `--author rules --no-repair` (no
model is called) and then replayed, against a fresh replica each time,
under three profiles: `clean`, `citrix-light` (JPEG 70) and `citrix-heavy`
(JPEG 35, scale 0.8, 16-bit colour). A run passes only when the engine
reports a pass **and** the replica's state holds what `<flow>.expect.json`
says. A value typed into the wrong field fails even if every assertion
was green.

| Flow | What it asks of vision |
|---|---|
| `read-cells` | find values on screen, including `0O10` and `1l70` |
| `read-german` | read umlauts, `ß` and `§` as written |
| `write-label` | fill a header field by its label (`Type … into the "Diag" field`) |
| `write-cell` | Tab to a table cell, erase it and type over it |
| `add-row` | Enter, wait out the locked keyboard, fill the new line |
| `paging` | PF8 to the rows the table does not show |

`FLOWPROOF_VISION_UIA=0` is set for every run: over Citrix there is no
accessibility tree, so the local window must not have one either.

## Baseline: flowproof 0.23.18, 9 November 2026

Windows 11, English profile (Windows OCR languages: en-GB, en-US only).
Record and replay agreed on every row except where noted.

| Flow | clean | citrix-light | citrix-heavy | Why |
|---|---|---|---|---|
| `read-cells` | fail | fail | fail | `0O10` reads `0010`, `1l70` reads `1170`; heavy also loses `1250,00` |
| `read-german` | fail | fail | fail | every umlaut reads `?`, `§` reads `$` |
| `write-label` | wrong state | wrong state | fail | the fill lands in the field but leaves `A129`: select-all does nothing on a terminal and typing overwrites, yet `page shows A12` passed. Heavy: the `Diag` label is not found |
| `write-cell` | fail | pass | fail | the value IS written (`ZB99` in the state); OCR reads it back as `7B99`, so the assertion fails |
| `add-row` | pass | replay fail | pass | keyboard-only and right; on one replay `Dialog aktiv` was not read from the status bar within 60s |
| `paging` | pass | pass | pass | keyboard-only |

What this says:

- **Moving and typing by keyboard works**, even through heavy degradation:
  Tab counts, auto-skip, Enter's new line and PF paging all land where
  the flow meant. The failures are all in *reading*.
- **Reading a grid with general OCR is the gap.** Look-alike characters,
  `Z`/`7`, `.`/`,` and punctuation are misread, columns separate from
  their rows (OCR groups the table into column blocks), and a short value
  next to another (`ZB30` beside `1`) merges into `2ZB30`.
- **Assertions can pass on the wrong field.** `page shows` matches
  anywhere on screen, so a value in the wrong place reads as success. Only
  the replica's state caught `A129`.
- **A label fill does not clear a terminal field.** Writes need Erase EOF
  before typing.
- **German needs a German OCR language.** Windows OCR follows the user's
  profile languages; with only English installed, non-ASCII text is lost.

Each of these points at the terminal-grid mode proposed for the engine:
read a field by its row and column on the character grid (crop exactly
one field, OCR it alone), check the cursor position in the status bar
after each Tab, erase before writing and read the field back, and a
configurable OCR language. These flows are its acceptance tests.
