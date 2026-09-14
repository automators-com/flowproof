---
title: "Repeating steps"
description: "Repeating a block with foreach, and repeating until the app settles with repeat: and when:."
---

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
Sometimes you do not know: press a button until the label changes, recover
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
actually ran: ordinary concrete steps, no `repeat` and no `when`. The trace
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

A missing element makes a positive `shows` false and a negative one true,
the same reading replay takes. Anything else is refused by name.

The comparison is the one condition that weighs two readings against each
other rather than a reading against a literal, and it is **numeric**: `"9"`
is greater than `"10"` as text and smaller as a number, and a condition that
quietly answered the text question would be worse than one that refuses. A
side that does not read as a number fails the recording and is quoted back.

Scope conditions tightly: `page shows ERROR` also matches a heading reading
"Errors occur", so name the element instead.
