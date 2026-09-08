---
title: "Variables and exports"
description: "Remembering and reusing live values within a flow, and handing a value to the next flow with exports:."
---

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
run gets entered; there is no literal a trace could record, so the trace
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

types `12 + 30` (three tokens of displayed text with a plus sign between
them) and not `42`. That is interpolation behaving correctly, not a bug,
and it is the reason the step is worth spelling out: `12 + 30` looks close
enough to an answer that a flow could go green on it while asserting
nothing anybody meant.

Arithmetic is refused deliberately, not merely absent. A capture is *text
the app displayed*, and supplying it back is data entry: the thing a user
does with a generated id. Deriving a new value from two of them is a
computation, and a trace that carries a computation has stopped being a
recording of what happened. The one exception is on the assertion side,
where `shows ${captured.x} + <number>` answers "did this change by the
right amount?", a question a literal cannot express, because the starting
value is only known at run time. It takes one capture and one plain number,
and it does not compose.

A name that was never remembered fails closed, naming what was in scope,
rather than typing the reference or an empty string.

A **counted** capture is the same value with a different reading, so it
composes with everything a captured value already does, including the
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
run AFTER this one in a suite, which is how a test case spans
technologies: one flow drives SAP GUI and captures the order number off the
status bar, the next drives the web portal that must show it. Each flow
keeps its own `app:` and its own driver; the suite is the test case, and
the export is the thread through it.

```yaml
# a-create-order.flow.yaml: SAP GUI mints the order number
name: Create standard order
app: sap
steps:
  - Go to /nVA01
  # ... create and save the order ...
  # VA02 opens with the order just created already filled in, so the number
  # has an element of its OWN. That is the shape a capture needs; reading
  # it out of the status bar's sentence would be pattern matching, which
  # the grammar refuses.
  - Go to /nVA02
  - Remember the "id:wnd[0]/usr/ctxtVBAK-VBELN" as order
exports:
  ORDER_NO: ${captured.order}
```

```yaml
# b-verify-portal.flow.yaml: the portal must show what SAP minted
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
environment variables for the remaining flows, which reference them as
ordinary `${VAR}`s, so the downstream trace stores only the reference and
resolves it fresh on every replay. The handoff happens at REPLAY time, from
replay-time captures: flow B replays against the value flow A's replay just
read, not against a value frozen at record.

What holds, and why:

- **Nothing is persisted.** Like a capture, an exported value exists only
  in the memory of the run. The trace holds the capture name, the run
  report and the `[EXPORT]` line hold the export NAME: an order number or
  balance stays out of committed artifacts and CI logs alike.
- **An export that cannot resolve fails the flow that owns it.** A
  `${captured.<name>}` never remembered fails THIS flow with the captures
  that were in scope, not the downstream flow, which would otherwise fail
  holding a variable nobody visibly set. And a failed flow exports
  nothing: no partial contract.
- **A single `flowproof run <spec>` resolves exports too**, though there is
  no downstream flow to receive them; the verdict must not depend on
  whether the flow ran alone or in a suite.
- **`app: agent` flows cannot export** (a parse error): they record at the
  model boundary and have no captures. Chain them as consumers: an agent
  flow's spec can reference `${ORDER_NO}` like any other.

The suite's existing machinery composes: `env_from` mints the data the
FIRST flow needs, `order:` in `suite.yaml` pins who runs before whom, and
`exports:` carries what a flow LEARNED to whoever follows.
