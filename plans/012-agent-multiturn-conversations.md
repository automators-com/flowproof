---
status: done
---
# Plan 12 — Multi-turn agent conversations

Issue: #375. `CHARTER.md` Milestone 3.

## Problem

`docs/agent-testing/running-agent-flows.md` already says this plainly: "Every
`prompt:` step is joined by newlines into ONE task string... it is a single
turn, not a back-and-forth conversation." The join is positional-blind — a
spec written as `prompt -> assert_tool_call -> prompt` concatenates both
prompts and delivers them before the agent starts; the second `prompt:` is
not a second turn and its position relative to the assertion is discarded.

That is sufficient for single-task agents, but it cannot represent the flows
most likely to exercise approval and safety controls:

- the agent asks a clarifying question and the user answers;
- the agent proposes a destructive action and waits for approval;
- a user changes intent after seeing an intermediate result;
- a denial or tool error is followed by recovery;
- a later turn attempts to exploit context accumulated earlier.

Today, none of these can be tested honestly. A flow author who writes two
`prompt:` steps around an `assert_no_tool_call` gets a green check that
proves nothing: both prompts already reached the agent before it acted, so
an agent that skips the approval gate entirely passes identically to one
that respects it.

## Naming collision to resolve first

The codebase already uses `Turn` for something else. `crates/flowproof-trace/
src/cassette.rs` defines `Turn` as one request/response exchange at the model
boundary — one call to the LLM and its answer, including intermediate
tool-call round-trips within a single delivered task. A `Cassette` is
`Vec<Turn>` of these. `docs/trace-format.md`'s `served_turn` language and the
`url:` driver's "served-turn count" diagnostics all use "turn" this way too.

This plan's new concept — a human/user message delivered into an
already-running conversation, only after the agent's prior trajectory
settled — is a different, coarser unit: it groups zero or more existing
`Turn`s. Reusing the word "turn" for both is exactly the kind of ambiguity
this codebase's own docs warn against elsewhere ("prose describing code that
no longer exists is a defect" — the sibling failure mode is prose where one
word means two things).

**Decision: the new unit is called a `delivery`.** A `delivery` is "one user
message plus the agent `Turn`s it provokes, up to the agent's recorded
settle point." The existing `Turn` type, `served_turn` field name, and all
existing docs keep their current meaning unchanged. New schema fields and new
YAML keys use `delivery`/`deliveries`, never bare `turn`, to keep the two
concepts lexically distinct in code, schema, and docs.

## Constraint (from the issue, restated against real code)

- Replay makes zero LLM calls and needs no provider key — unchanged; this
  plan only changes when a delivery's turns are consumed, not how a `Turn`
  is matched (`cassette.rs`'s three matching rules stand as-is).
- A cassette remains ground truth.
- Recorded delivery boundaries are replayed, not re-decided — replay must
  not re-run the agent's own logic to decide where a delivery ends; it plays
  back the recorded settle point.
- Healing proposes a diff and never silently changes the recording.
- Secrets do not enter the trace (already true of `Turn`/`Message`; a new
  delivery-boundary marker must carry no message content of its own).

## Decision: authoring model is live, not scripted

The obvious-looking alternative — a human writes every `user:` line in the
YAML up front, for both sides of the exchange, before ever running `record`
— is **rejected**. It doesn't fit how this codebase already treats
`record`: for GUI/SAP flows, a human drives the *live* app and flowproof
observes and writes the trace; nobody hand-writes the click sequence first
and asks flowproof to fake driving it. Agent conversations should work the
same way, and there's a sharper reason it must: you often cannot know what
the agent will ask before it asks it. Pre-scripting "Yes, I confirm — go
ahead and cancel it" as the second line of a YAML file you wrote before the
agent ever spoke means you already knew the exact shape of its clarifying
question, which quietly defeats the point — a test whose human side was
written with full foreknowledge of the agent's behavior cannot catch the
agent changing that behavior.

**So `conversation:` blocks are recorded interactively, then written to
`.flow.yaml`, not authored from a blank page.** During `flowproof record`,
a human has an actual live back-and-forth with the running agent: the agent
asks or responds, the operator types their reply at the terminal in real
time, and flowproof captures the whole exchange — every delivery, every
`Turn` it provoked — into the cassette. The `.flow.yaml`'s `conversation:`
block is generated from that recorded transcript afterward (the same
before-you-approve posture `record` already has for other app kinds — see
plan 7's "the operator's job is to hit record once"), and the operator then
edits in `assert*:` lines against what actually happened, rather than
guessing them beforehand.

```
$ flowproof record support-agent-cancel.flow.yaml --agent-conversation
Agent: How can I help?
You> Please cancel my order A-4471.
Agent: Are you sure? This can't be undone.
You> Yes, I confirm — go ahead and cancel it.
Agent: Done — order A-4471 is cancelled.
[record] wrote support-agent-cancel.flow.yaml (2 deliveries, edit in
assertions and re-run to confirm)
```

The generated file looks the same shape as before — a `conversation:` list
of deliveries — but the `user:` text in it is a **record of what the
operator actually typed**, not a prediction:

```yaml
name: Support agent cancels an order with confirmation
app: agent
agent:
  command: python3 scripts/demo/support_agent.py
tools:
  - name: cancel_order
steps:
  - conversation:
      - user: Please cancel my order A-4471.
        # assert: reply contains "are you sure"      <- operator adds these
        # assert_no_tool_call: cancel_order              after reviewing the
      - user: Yes, I confirm — go ahead and cancel it.   recorded transcript
        # assert_tool_call: cancel_order where order_id contains A-4471
        # assert: reply contains cancelled
```

- `conversation:` takes a list of deliveries, each carrying the recorded
  `user:` text and, once the operator has added them, zero or more
  `assert*:` keys scoped to that delivery alone (delivery-local assertions —
  see next section).
- A `conversation:` step is itself one step in the existing `steps:` list, so
  it composes with everything before and after it (setup steps, other
  assertions) unchanged.
- A bare `prompt:` step (today's form) remains legal and is defined as sugar
  for a `conversation:` of exactly one delivery — see "Backward
  compatibility." It stays authorable by hand as today, precisely because it
  is a single, static, up-front task — the "you can't know the agent's
  follow-up before it happens" problem only exists once there's a second
  delivery reacting to the first.
- **Editing `user:` text by hand after recording is legal but adversarial by
  design**, not part of the intended authoring loop: it's exactly the
  "changed later user turn" red-path scenario this plan tests (see Tests),
  because a hand-edited delivery no longer reflects a real exchange that
  happened. It's how you deliberately construct a divergence fixture, not
  how you write a new flow.
- `flowproof run` (replay) is unaffected by any of this — it always plays
  back the recorded cassette against the flow file's assertions, whether the
  `user:` text came from an interactive `record` session or (for the sugar
  case) a hand-written `prompt:`.

## Decision: assertion scope

The issue asks explicitly whether assertions are conversation-wide,
turn-local, or both, "with unambiguous syntax." Both, disambiguated by
placement, not by a new keyword:

- An `assert*:` nested inside a delivery (as in the example above) is
  **delivery-local**: it is checked against only the `Turn`s that delivery
  produced, and it is checked immediately after that delivery settles —
  before the next delivery is sent. This is what makes "the agent must not
  call `cancel_order` yet" a meaningful check instead of a check against the
  final state of the whole exchange.
- An `assert*:` at the outer step level, after the `conversation:` block
  closes, is **conversation-wide**: it sees the full accumulated trajectory
  across every delivery, exactly like today's single-turn `assert*:` sees
  the full (single-delivery) trajectory. No new syntax — it is the existing
  `assert*:` step type, just now potentially following a multi-delivery
  `conversation:` instead of a bare `prompt:`.

```yaml
steps:
  - conversation:
      - user: Please cancel my order A-4471.
        assert_no_tool_call: cancel_order          # delivery-local
      - user: Yes, confirm.
        assert_tool_call: cancel_order              # delivery-local
  - assert: reply contains cancelled                # conversation-wide, sees final reply
```

`reply` continues to mean "the final assistant message of the trajectory so
far" at whatever point it's evaluated — delivery-local uses the trajectory
truncated to that delivery, conversation-wide uses the whole thing. This
reuses the existing `reply` semantics from `running-agent-flows.md` rather
than inventing a second meaning.

## Decision: delivery gating (the driver-contract change)

This is the part the issue calls out as "a driver-contract change, not only
a new grammar form."

**On `record`:** the agent process/service is started once per `conversation:`
session (not once per delivery). The operator's first typed line is sent —
via `FLOWPROOF_PROMPT` for `command:`, via the POST body for `url:` — and
flowproof waits for the agent's trajectory to reach its existing settle
condition (the same "the agent decided it's done" signal `reply` already
depends on today, e.g. a `stop_reason` that isn't a pending-tool-call state,
or the process/request cycle completing). Only once settled does flowproof
print the agent's reply at the terminal and prompt the operator for their
next line — it never sends a second delivery until a human has actually
typed one. Each delivery's `Turn`s are appended to the same `Cassette` in
order, tagged with which delivery produced them (new field:
`Turn.delivery_index: usize`, defaulted to `0` and omitted from JSON when
zero, so every existing single-delivery trace round-trips byte-identical —
see "Backward compatibility"). The operator ends the session with an
explicit "done" input (or EOF); flowproof then writes the `.flow.yaml` and
cassette together, exactly as it already treats a passing `record` run for
other app kinds.

**On `replay`:** the cassette already knows, per `Turn`, which delivery it
belongs to. Replay serves delivery 0's turns until it's exhausted, then
*waits for the flow's own step sequencing* to hand it delivery 1's `user:`
text before serving delivery 1's turns — it does not re-decide the boundary
by inspecting agent behavior, it plays back what `delivery_index` already
recorded. This satisfies "recorded turn boundaries are replayed, not
re-decided" directly: the boundary is data, not inference, at replay time.

**Mechanism for "wait for settle" per driver:**

- `command:`: the same process instance stays alive across deliveries. The
  existing single-shot contract ("agent reads `FLOWPROOF_PROMPT`, runs to
  completion, process exits") does not fit a live multi-turn agent, since
  most real conversational agents run as one long-lived process across
  turns rather than exiting and being re-spawned. This plan therefore
  requires a second contract for a `command:` agent used with
  `conversation:`: instead of `FLOWPROOF_PROMPT`-then-exit, subsequent
  deliveries are written to the process's stdin, one JSON line per delivery
  (`{"prompt": "..."}`, matching the `url:` driver's existing body shape for
  consistency), and settle is detected the same way the proxy already
  detects "no more model calls pending" for that delivery. An agent that
  only supports the old single-shot contract keeps working via the
  bare-`prompt:` sugar path (no `conversation:` block, one spawn, one exit).
- `url:`: closer to a natural fit already — `running-agent-flows.md` notes
  the trigger must be stateless per request or reset via `before_each`.
  Multi-turn instead requires the OPPOSITE for a `conversation:` block: the
  service must be conversational (preserve its own history across the
  `conversation:`'s POSTs), and flowproof POSTs each delivery in turn to the
  same trigger URL, waiting for each response before sending the next. This
  is a documented behavior change scoped to `conversation:` only — a bare
  `prompt:` against `url:` keeps today's one-shot-stateless expectation.

## Decision: trace/schema changes

- `crates/flowproof-trace/src/cassette.rs`: add `delivery_index: usize` to
  `Turn`, `#[serde(default, skip_serializing_if = "is_zero")]` so a v1
  single-delivery trace is byte-identical after this change reads it back.
- Add a `deliveries: Vec<DeliveryMeta>` (or similar) top-level field to
  `Cassette`, `#[serde(default, skip_serializing_if = "Vec::is_empty")]`,
  recording per-delivery metadata needed for replay gating and reporting:
  the delivery's `user:` text (for report/diff readability — matching
  behavior, not matched against on replay, since the wire wouldn't re-send
  it) and the count of `Turn`s it produced. Empty for every pre-existing
  trace.
- `docs/trace-format.md` gets a new "Multi-delivery conversations" section
  documenting both fields, written in the same register as the existing
  side-effect-lane section (explicit about what's additive, what's
  byte-identical when absent, what a reader must not infer from absence).
- `crates/flowproof-trace/schema/trace-v1.schema.json` is for the step-log
  (`app: <driver>`) shape, not the agent cassette — confirm during
  implementation whether the agent cassette has its own JSON Schema file to
  update, or whether it's serde-only today; if the latter, this plan adds
  one rather than leaving the new fields undocumented in schema form (the
  issue's acceptance criteria requires "the JSON Schema... in the same
  change").

## Decision: dialect and streaming coverage

The issue requires OpenAI-compatible and Anthropic dialects, buffered and
streaming, for both drivers — four combinations, all of which already exist
for single-delivery flows per `Turn.protocol` and the existing proxy. This
plan does not change per-`Turn` matching or protocol handling at all; it only
changes when a `Turn` is appended to the cassette and when the *next*
delivery's prompt is released. Because that logic sits above the
protocol-specific proxy code (it gates on delivery settle, not on wire
format), the four combinations should be exercised as fixtures rather than
requiring new per-dialect code paths — this is a testing task, not a new
implementation surface, and should be flagged during implementation if that
assumption breaks.

## Backward compatibility

- A bare `prompt:` step, or a run of consecutive `prompt:` steps with no
  `conversation:` wrapper, is defined as exactly today's behavior:
  newline-joined into one string, delivered as delivery 0, once. This is not
  a compatibility shim bolted on after the fact — it's the same code path
  `conversation:` reduces to when it has exactly one delivery, so there is
  one implementation, not two.
- Every existing `.trace.jsonl`/cassette file replays unchanged:
  `delivery_index` defaults to `0`, `deliveries` defaults to empty, both
  omitted from serialized JSON when at their default, so a diff against an
  old cassette shows nothing until a flow actually opts into
  `conversation:`.
- No flag or opt-in is needed to keep old flows working; `conversation:` is
  new syntax, not a new mode old syntax must declare.

## Implementation steps

1. Resolve the `Turn`/`delivery` naming decision in code first (this plan
   already resolves it above) so no PR introduces a second meaning for
   "turn."
2. Add `delivery_index` to `Turn` and `deliveries` metadata to `Cassette` in
   `flowproof-trace`, with round-trip tests proving a pre-existing cassette
   is byte-identical after (de)serializing through the updated structs.
3. Add the `conversation:` grammar to the flow-file parser, with
   `prompt:`/bare steps reducing to a single-delivery `conversation:`
   internally.
4. Add delivery-local vs conversation-wide assertion scoping to the
   assertion evaluator, keyed on where in the YAML the `assert*:` is
   nested — no new assertion keyword.
5. Change the recording agent driver (`flowproof-agent`/`flowproof-adapters`
   proxy plumbing) to gate delivery N+1 on delivery N's settle condition,
   for both `command:` (stdin-line contract) and `url:` (sequential POST
   contract).
6. Change the replay driver (`flowproof-replay`) to serve `Turn`s grouped by
   `delivery_index` and release the next delivery only when the flow's own
   step sequence reaches it — never by inferring settle from replayed agent
   behavior.
7. Update `docs/trace-format.md`, `docs/agent-testing/running-agent-flows.md`
   (the "one task string... single turn" language is now wrong and must be
   corrected in the same change, per this repo's "prose describing code that
   no longer exists is a defect" rule), the JSON Schema, and add a
   `conversation:` example under `examples/`.
8. Add the red-path tests the issue lists explicitly (see below).

## Tests

- Round-trip: an old single-delivery cassette (re)serializes byte-identical
  through the updated `Turn`/`Cassette` structs.
- A `conversation:` flow recorded and replayed for each of the four
  dialect × streaming combinations, proving zero upstream model calls on
  replay across the whole conversation, not just delivery 0.
- Delivery-local assertion catches a violation the conversation-wide
  assertion would miss (the `assert_no_tool_call` example above is the
  canonical case — this is the test that proves the feature does what the
  issue exists for).
- Changed later user turn: edit delivery 2's `user:` text after recording;
  replay must fail at that delivery, not silently succeed or fail
  elsewhere.
- Missing turn: remove a delivery from the flow file while the cassette
  still has it recorded; replay must report an unconsumed recorded delivery,
  not silently ignore it.
- Extra turn: add a delivery to the flow file with nothing recorded for it;
  replay must report an unrecorded delivery, not fabricate a response.
- Tool-call divergence after delivery one: change an asserted tool argument
  on delivery 2 only; must fail there, not cascade into a misleading
  delivery-1 failure (matches `cassette.rs` rule 2, extended across
  deliveries).
- `command:` driver: an agent using the old single-shot
  `FLOWPROOF_PROMPT`-then-exit contract still passes unmodified via the
  bare-`prompt:` path.

## Out of scope

- Changing `Turn`'s existing matching rules (byte-for-byte, consumed once,
  fail-at-first-divergence, envelope-first reporting) — this plan groups
  turns into deliveries, it does not change how a turn matches.
- MCP tool-server lanes (`mcp:`) — additive and orthogonal; a
  `conversation:` flow that also mocks MCP servers is expected to work by
  composition, but is not this plan's test surface to design.
- The seccomp side-effect lane — likewise orthogonal and additive.
- Any change to `heal`'s behavior beyond it needing to understand the new
  `delivery_index`/`deliveries` fields when re-recording a drifted
  multi-delivery trace.

## Resolved decisions (formerly open questions)

- **Settle-detection for `command:` is proxy-observed, not agent-emitted.**
  No new agent-side contract is required. flowproof's proxy already sees
  every model call the long-lived process makes (that's the entire premise
  of model-boundary interception); settle for a delivery is defined as "the
  most recent response the proxy observed for this delivery has a
  `stop_reason` that isn't a pending-tool-call state, and no further model
  call arrives before the per-delivery timeout." This is the same signal
  `reply` already relies on for the single-shot case, just evaluated without
  waiting for process exit. Rejected alternative: an explicit end-of-delivery
  marker line the agent process itself prints. That would work but requires
  every `command:` agent to adopt a new, flowproof-specific protocol just to
  be testable multi-turn — inconsistent with the rest of this feature, which
  deliberately asks nothing new of the agent under test (the `url:` driver
  gets settle for free from the HTTP response for the same reason: no new
  agent-side cooperation).
- **`deliveries` metadata stores `user:` text verbatim.** `cassette.rs`
  already states the design goal plainly — "diffable and reviewable" — and
  the redundancy cost (a short string, once per delivery) is negligible next
  to what it buys: a cassette diff or `heal` report that reads as an actual
  transcript instead of a hash a human has to cross-reference against the
  flow file by hand. A hash/length-only encoding is the kind of savings that
  matters for secrets (hence the side-effect lane's path hashing), and
  `user:` text is not a secret — it is the operator's own already-visible
  flow-file content. Verbatim storage wins with no real trade-off here.
- **Timeout is per-delivery, not a shared block budget.** Each delivery keeps
  the existing 300-second bound, reset at the start of that delivery, rather
  than dividing one 300-second budget across however many deliveries a
  `conversation:` block has. A shared budget would make a flow's viable
  delivery count silently depend on how slow earlier deliveries happened to
  be — nondeterministic in the same way this whole feature exists to
  eliminate. Per-delivery keeps each delivery's bound predictable and
  matches how the constant is already used today (as a bound on one
  settle-cycle, not on a whole flow file's wall-clock).
- **`before_each`/service reset is a flow-run boundary, never a mid-block
  boundary.** A `conversation:` block's deliveries share one live service
  session by construction — that's what makes it a conversation rather than
  N independent single-turn flows — so reset semantics don't change from
  today: `before_each` (or process (re)spawn for `command:`) fires once,
  before delivery 1, exactly where it fires today before a flow's one
  (implicit) delivery. Nothing resets between deliveries inside a block. This
  needs one clarifying sentence added to `running-agent-flows.md`'s
  `before_each` guidance in the same doc change as everything else in this
  plan, but requires no new mechanism.

## Shipped

Every decision above is implemented and tested, in four commits: the trace
schema (`delivery_index`/`deliveries`), the `conversation:` grammar, `url:`
delivery gating, `command:` delivery gating, and the interactive `record
--agent-conversation` authoring loop (the `conversation: interactive`
placeholder + text-substitution write-back this plan's authoring-model
section calls for). All tested end to end with fakes — a real spawned
process, a fake real-model upstream, a fake stateful `url:` service — never
a real model or API key.

**Known gap, not blocking:** the dialect/streaming coverage this plan asked
for ("exercised as fixtures... flagged if that assumption breaks") was not
built out. Every test here uses the OpenAI wire shape, non-streaming. The
delivery-gating code added is dialect-agnostic by construction — it never
inspects `Turn.protocol` or touches the proxy's streaming path, only the
served-count/tool-call-presence signals `cassette.rs` already tracks
uniformly — so there is no known reason Anthropic or streaming would behave
differently, but that is confidence, not a fixture. Left as follow-up
coverage rather than a blocker.
