# Vision on a host terminal screen

Legacy applications often run inside Citrix as a **host terminal screen**:
a fixed monospace character grid of protected text and white input fields,
with a PF-key legend, a message line and a status bar showing the cursor's
row and column. Their "tables" are header lines with rows of input fields
under them, one screen line per row, and people move through them with Tab,
not the mouse.

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
  `enter_delay_ms` (the status bar reads `Status wartet`; keys are dropped),
  then a new empty line opens in the table the cursor is in (`add_row`) or
  only the message line answers (`submit`).
- **PF keys** page a table that holds more rows than it shows (PF8/PF7 by
  default); PF13-PF24 are Shift+F1-F12.
- **A click** puts the cursor on the clicked cell.

What the keys do differs between emulators, so each of these is set under
`keys:` in the layout. `host-screen.yaml` is an invented screen: every
name, number and value in it is made up.

Options make each frame look as it would after Citrix: `--scale 0.8`
(shrink and stretch back), `--bits 16` or `--bits 8` (colour depth) and
`--jpeg 35` (compression). `--state out.json` writes what is on screen after
every change: every field, every table row, the cursor and the screen text.
That file is the ground truth the evals score against.

```bash
cargo build -p flowproof-terminal-replica
target/debug/flowproof-terminal-replica evals/terminal-grid/host-screen.yaml --state state.json
```

## The evals

```powershell
powershell -File evals/terminal-grid/run.ps1 -Engine <flowproof.exe> [-Only <flow>] [-FontPx <size>]
```

Each flow in `flows/` is recorded with `--author rules --no-repair` (no
model is called) and then replayed, against a fresh replica each time,
under three profiles. `clean` is the screen as drawn. `citrix` is what
nested Citrix sessions do to sharp text: no compression blocks, but each
session may rescale, so the frame is shrunk to 90% and stretched back.
`stress` (JPEG 35, scale 0.8, 16-bit colour) is far harsher, kept to show
where OCR gives out. The layout draws a thin serif Courier whose zero has
no slash, as host emulators often do; `-FontPx` draws it at another size,
and the workflow runs 13, 15, 18 and 20px. A run passes only when the
engine reports a pass **and** the replica's state holds what
`<flow>.expect.json` says. A value typed into the wrong field fails even if
every assertion was green.

| Flow | What it asks of vision |
|---|---|
| `read-cells` | find values on screen, including `0O10` and `1l70` |
| `read-german` | read umlauts, `ß` and `§` as written |
| `write-label` | fill a header field by its label (`Type … into the "Kenn" field`) |
| `write-cell` | Tab to a table cell, erase it and type over it |
| `add-row` | Enter, wait out the locked keyboard, fill the new line |
| `paging` | PF8 to the rows the table does not show |
| `cell-read` | read cells by column header and a value in their row |
| `cell-write` | fill cells by column and row, emptying each first |

`FLOWPROOF_VISION_UIA=0` is set for every run: over Citrix there is no
accessibility tree, so the local window must not have one either.

## What the evals found

Moving and typing by keyboard - Tab counts, auto-skip, Enter's new line, PF
paging, label fills - holds up at every size and through the Citrix
profile. Reading is where Windows OCR gives out on thin Courier: it
returns nothing for an isolated one- or two-letter word (a header such as
`KZ`, at any size), and misreads characters in ways no safe rule can
forgive (`XB30` as `XE30`, `1250,00` as `1250, aa`). A larger font helps
some reads and hurts others. Reading such screens reliably needs a reader
made for a character grid, which learns the screen's own font and matches
each character cell against it.
