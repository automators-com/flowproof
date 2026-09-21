---
title: "Repeating steps"
description: "Repeating a block with foreach, and repeating until the app settles with repeat: and when:."
---

## Repeat over known values with `foreach`

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

## Repeating until the app settles, and optional blocks (`repeat:` and `when:`)

`foreach` repeats a block as many times as you know when you write it.
Sometimes you do not know: press a button until the label changes, dismiss
a banner if it appeared. Those are `repeat:` and `when:`.

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

**`repeat:` is settled while recording.** The condition is read against the
live app before each pass, and what lands in the trace is the passes that
actually ran: ordinary concrete steps, no `repeat`. How many passes it took
is a fact about that recording, and a replay that needs a different number
is a different behaviour, which for a regression test is the right way
round: a loop that silently re-adapted every run would always pass.

`until:` is checked **before** the first pass, so a `repeat:` whose
condition already holds runs zero times. `max:` is required: if the
condition never holds within it, recording fails and names the bound. Each
`repeat:` gets its own budget.

**`when:` is read again at every replay.** Recording reads the condition
against the live app and, if it holds, records the block's steps with the
condition attached to each of them as a guard. Replay reads that guard
again, once per block, before the block's first step: if it holds the
block runs, and if not its steps are skipped, reported as `SKIP` with the
condition named, and the flow continues. A skipped block is not a passed
one; the report says which branch each run took.

Two consequences follow. A `when:` block is only in the trace if its
condition held while recording, so record with the optional element
present: the banner showing, the dialog open. If it was absent at record
time there is nothing to replay when it appears later, and the step after
it fails exactly as it did before. And the guard is read once per block,
not once per step, so a block that removes the very thing its condition
saw (dismissing the banner) still runs to its end.

Conditions read state; they never wait:

| Condition | Holds when |
|---|---|
| `page shows <text>` / `page does not show <text>` | the whole surface's text does or does not contain it |
| `the "<target>" shows <text>` | that element's text contains it |
| `the "<target>" is visible` / `is not visible` | it is on screen, or is missing or hidden |
| `the "<a>" is greater than the "<b>"` / `is less than` | both read as numbers, and the ordering holds |

A missing element makes a positive `shows` false and a negative one true,
the same reading replay takes. Anything else is refused by name.

The comparison is the one condition that weighs two readings against each
other rather than a reading against a literal, and it is **numeric**: `"9"`
is greater than `"10"` as text and smaller as a number, and a condition that
quietly answered the text question would be worse than one that refuses. A
side that does not read as a number fails the recording and is quoted back.
It is also the one condition settled at record time only: it has no
replay-time form yet, so a `when:` on it records the branch that held and
replays it unconditionally.

Scope conditions tightly: `page shows ERROR` also matches a heading reading
"Errors occur", so name the element instead.
