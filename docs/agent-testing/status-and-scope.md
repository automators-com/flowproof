---
title: "Status and scope"
description: "Implementation status, what multi-turn conversation testing covers today, and why model-output evals are out of scope."
---

Built and tested, each independently:

| Piece | What it does |
|---|---|
| cassette | the recorded trajectory, plus strict positional matching and envelope-first divergence reporting |
| tool-call matching | ordered subsequence, partial dotted-path arguments, the `assert_no_tool_call` guard path |
| proxy | serves a cassette over an OpenAI-compatible endpoint, and in record mode forwards to a real model and captures |
| substitution | rewrites a mocked tool result at the model boundary, identically at record and replay |
| trajectory diff | sorts a re-record into what the agent DID versus what it was TOLD, flagging changes the spec asserts |
| `assert_tool_call` grammar | the prose form |
| `app: agent` | the spec surface, process runner, record/replay orchestration and CLI dispatch, exercised end to end |
| egress containment | `allow_egress` / `assert_no_egress`, enforced by a Linux seccomp supervisor (proven by the Linux CI E2E); "not contained" and honestly reported on macOS/Windows and for `url:` flows |
| filesystem observation | the same seccomp filter also traps the destructive filesystem syscalls, REPORTS them to stderr, and - since #465 - records them into the trace's `side_effects` lane, workspace-relative or hash-redacted. `assert_no_side_effect` turns the record into a verdict; without that step the lane asserts nothing. Linux only, and only where egress containment or side-effect observation is engaged |
| MCP tool boundary | stdio (v3.1) and streamable-HTTP (v3.2): flowproof stands in as the server, records the JSON-RPC traffic once and replays it with no server running. A tool with a `result:` here is answered by the stand-in and never forwarded, in either phase - the one boundary that stops a tool executing |
| Anthropic Messages | built and covered end to end, record leg included: a flow records against a Messages-dialect upstream and replays it with no model at all |
| Streaming | built and covered end to end in both dialects, record leg included: a `stream: true` agent is served SSE at record and at replay, and the test asserts the FRAME BOUNDARIES, not the assembled text - a replay that collapsed the stream into one buffered body would still produce the same reply |
| http-target | `agent.url` services are built, and covered end to end including the record leg: a service started independently and pointed at the proxy is triggered, recorded, and replayed offline |
| `conversation:` (multi-turn) | built (#375): a list of deliveries, each a `user:` message plus its own delivery-local assertions, checked against just the turns that delivery produced before the next one is sent. Both drivers gate on it - `agent.url` sends each delivery as its own sequential POST, `agent.command` sends delivery 0 via `FLOWPROOF_PROMPT` and later deliveries over the child's stdin. A bare `prompt:` step still reduces to a single-delivery conversation internally, so existing cassettes replay unchanged. See [Multi-turn conversations](#multi-turn-conversations) below for what is and is not covered yet |

Not built yet: per-call result sequences (one static result per tool), and
the structured `args:` / `args_exact:` assertion forms. The `matches`
argument matcher shipped in 0.3.x. The MCP tool
boundary is BUILT (v3.1 stdio, v3.2 streamable-HTTP) - an earlier revision of
this paragraph listed it as unbuilt, contradicting the Phasing section. v1's
acceptance bar (a real external agent recording and replaying through the
proxy) is met by [`examples/agent-demo/`](../examples/agent-demo/) (a real
OpenAI-SDK agent against a live model); the in-tree E2E proves the same path
with a fake agent and a fake model.

**Where the tests are, and are not.** Worth stating plainly, because "built"
and "covered by a test that would fail if it broke" are different claims:

| Capability | Coverage |
|---|---|
| OpenAI proxy + `assert_tool_call` | full: CLI record -> trace -> replay, agent as a real subprocess, on every PR |
| MCP stdio (v3.1) | full: real stand-in binary, real server, real agent subprocess, including "a mocked tool is never forwarded" |
| MCP streamable-HTTP (v3.2) | full: CLI record -> trace -> replay with a real agent subprocess against a real HTTP server, then replayed with that server stopped and deleted |
| Streaming replay | full, both dialects: CLI record -> trace -> replay with a `stream: true` agent subprocess, asserting the frames it received, so the record-mode synthesis is covered too |
| Anthropic Messages | full: CLI record -> trace -> replay against a Messages-dialect upstream, agent as a real subprocess, on every PR |
| http-target (`agent.url`) | full: a service flowproof did not start, pointed at the fixed `proxy_port`, driven through CLI record -> trace -> replay with no model reachable |
| `assert_no_tool_call` | full, both directions: the passing case, plus a red-path proof in which a model asks for the forbidden tool and an obedient agent calls it, so the record is refused and no trace is minted |

Every row above is now a CLI round trip with a real agent, not an assertion
about one. That list was for a long time a list of things believed to work; it
is now a list of things measured to.

The falsifiability suite is the other half of this table's honesty: a row
saying "covered" means a test exists, and
[how-flowproof-tests-flowproof.md](https://github.com/automators-com/flowproof/blob/main/internal/how-flowproof-tests-flowproof.md) is where
each assertion is proven able to FAIL. Coverage that cannot fail is not
coverage.

## Multi-turn conversations

A flow can now express "the user replies, then the agent should ...".
`conversation:` (issue #375; see `plans/012-agent-multiturn-conversations.md`)
is a list of deliveries, each a `user:` message plus its own delivery-local
`assert:`/`assert_tool_call:`/`assert_no_tool_call:`, checked against just the
turns that delivery produced before the next one is sent. Both drivers gate
delivery N+1 on delivery N's settle condition; the cassette carries
`delivery_index`/`deliveries` metadata so replay serves the right recorded
response to each turn rather than the whole trajectory, and existing
single-delivery cassettes are unaffected since a bare `prompt:` reduces to a
one-delivery conversation internally. A `conversation:` block can also be
authored live with `flowproof record <spec> --agent-conversation`
(`agent.command` only for now), rather than typed into the YAML by hand. See
[Running agent flows](running-agent-flows) for the full runtime contract.

Two things this does not cover yet:

- **Dialect and streaming fixtures.** The issue's acceptance criteria ask for
  OpenAI-compatible and Anthropic dialects, buffered and streaming - four
  combinations. The delivery-gating code is dialect-agnostic by construction
  (it gates on delivery settle, never inspects `Turn.protocol` or the proxy's
  streaming path), so there is no known reason Anthropic or streaming
  conversations would behave differently, but every `conversation:` test
  shipped so far uses the OpenAI wire shape, non-streaming. That is
  confidence, not a fixture, and is tracked as follow-up coverage rather than
  a blocker.
- **Egress containment.** A `conversation:` flow that also engages
  `allow_egress`/`assert_no_egress` still falls back to the ordinary
  single-shot path for now.

A useful workaround for either gap today: for a system whose conversation is
driven by an outer loop you control, test that loop's single-shot entry
point, or record one flow per turn with the conversation state seeded
through `agent.env`.

## Decision: model-output evals are out of scope

The second problem ("is the model's answer good?") needs samples,
scoring, thresholds, and judges. Its verdicts are statistical, not
deterministic, and its artifacts are score distributions, not traces. A
future `flowproof eval` could exist as a *separate* runner sharing the
proxy/cassette infrastructure, but the replay engine's promise
("recorded once, passes forever unless the system changed") must not be
blurred by a step type that can fail on an unchanged system. Same
philosophy as the `page.evaluate` rejection in
[design.md](https://github.com/automators-com/flowproof/blob/main/internal/design.md): protect the invariant that makes the tool
trustworthy.

A *third* problem is neither of these two, and is proposed separately in
[explore-mode.md](https://github.com/automators-com/flowproof/blob/main/internal/explore-mode.md):
not "is the answer good?" but "can a
control this suite already declares be violated by an input the recording
never saw?" Its verdict is existential rather than statistical: one
violation is a finding, and the finding converts into an ordinary
deterministic replay, but it can still fail on an unchanged system, so it
inherits the constraint above in full: a separate runner, a separate report
path, and no contribution to `flowproof audit`.
