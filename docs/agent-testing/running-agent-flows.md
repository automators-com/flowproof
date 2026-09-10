---
title: "Running agent flows"
description: "Running an agent flow, driving a running service with url:, and mocking MCP tool servers with mcp:."
---

The agent under test is an ordinary process flowproof spawns (`agent.command`).
Five facts about the runtime contract, all exercised by
[`examples/agent-demo/`](../examples/agent-demo/):

- **The prompt arrives in `FLOWPROOF_PROMPT`.** Every `prompt:` step is joined
  by newlines into ONE task string, set on the process environment before it
  starts. flowproof delivers the whole task up front and reads the trajectory
  the agent produces; it is a single turn, not a back-and-forth conversation.
  Note the joining is positional-blind: a spec written as
  `prompt -> assert_tool_call -> prompt` concatenates BOTH prompts and
  delivers them before the agent starts. The second `prompt:` is not a second
  turn, and its position relative to the assertion is discarded.

  A real multi-turn conversation is a separate step form, `conversation:`
  (issue #375; see `plans/012-agent-multiturn-conversations.md`): a list of
  deliveries, each a `user:` message plus its own delivery-local
  `assert:`/`assert_tool_call:`/`assert_no_tool_call:`, checked against just
  the turns that delivery produced before the next one is sent. Both drivers
  gate on it: `agent.url` sends each delivery as its own sequential POST,
  and `agent.command` sends delivery 0 via `FLOWPROOF_PROMPT` as always and
  writes every later delivery to the child process's own stdin, one JSON
  line (`{"prompt": "..."}`) per delivery, once the previous one settles -
  the process is spawned once and stays alive for the whole conversation,
  and its stdin is closed after the last delivery settles so a well-behaved
  multi-turn agent can finish and exit. A `conversation:` flow that also
  engages egress containment still falls back to the ordinary single-shot
  path for now. A bare `prompt:` step is unaffected either way; it is not
  desugared into a one-delivery `conversation:` internally.

  A `conversation:` block is authored LIVE, not typed into the YAML by
  hand: write a placeholder step `- conversation: interactive` in the
  flow's `steps:`, then run `flowproof record <spec> --agent-conversation`
  (`agent.command` only for now). This drives a real interactive terminal
  session against the real agent - it prints `You>` and waits for a typed
  line, sends it as the next delivery, and prints the agent's reply
  (`Agent> ...`, the model-boundary reply, never the process's own stdout)
  once that delivery settles. An empty line, EOF, or typing `done` ends the
  session (at least one delivery is required). On success, the ONE
  `- conversation: interactive` line is replaced - by exact text
  substitution, not a full-file reserialize, so nothing else in the flow
  file (comments included) is touched - with the deliveries actually typed,
  and the trace is written with `delivery_index`/`deliveries` metadata
  already correct. The generated block carries no assertions; add
  `assert:`/`assert_tool_call:`/`assert_no_tool_call:` under each delivery
  afterward, then `flowproof run` to confirm.
- **The proxy URL is injected for you.** flowproof points the agent at its
  local proxy by setting `OPENAI_BASE_URL`, `OPENAI_API_BASE`, `OPENAI_BASE`,
  and `FLOWPROOF_LLM_PROXY`, plus a placeholder `OPENAI_API_KEY` so a client
  that refuses to start without a key still starts. `agent.env` is applied
  LAST, so a flow can override any of these for a client that reads a
  different variable.
- **Record needs a real model; replay needs none.** On `record`, name the
  upstream with `FLOWPROOF_AGENT_UPSTREAM` (falling back to an
  `OPENAI_BASE_URL` you already have set) and supply the key through
  `FLOWPROOF_AGENT_KEY`, `ANTHROPIC_API_KEY`, or `OPENAI_API_KEY`. The key
  goes straight into the outbound `Authorization` header (a bare key is
  `Bearer`-wrapped) and nowhere else: the trace stores request bodies only,
  so no key is ever written to disk. `replay` serves the cassette and makes
  zero model calls.
- **`reply` is the final assistant message** of the trajectory, not the
  process's stdout (see "Settled in review").
- **A flow is bounded to 300 seconds.** The agent's own logic decides when it
  is done; if it never finishes, the run fails on the timeout.

And one the demo cannot show you, because the demo works:

- **An agent that never starts is reported as such, with its stderr.** A
  process that exits non-zero without reaching the proxy fails with its exit
  code and the tail of what it printed, not with a bare "made 0 model calls":
  the failure is the agent's, and the reason is usually in its own output. An
  agent that exits CLEANLY without calling a model is the different failure:
  its client never honoured the injected base URL, which is what
  `flowproof doctor` diagnoses.

### Driving a running service (`url:`)

Instead of a `command` flowproof starts, an agent flow can drive a service
that is ALREADY running, by POSTing to it:

```yaml
app: agent
agent:
  url: http://localhost:8088/task    # POST {"prompt": ...} triggers a turn
  proxy_port: 4646                   # required: the local port the proxy binds
  headers:                           # optional; ${VAR} allowed, never stored
    Authorization: Bearer ${DEV_TOKEN}
```

`command:` and `url:` are the two drivers, and a flow uses exactly one.
flowproof binds its proxy at `http://127.0.0.1:<proxy_port>/v1`, POSTs
`{"prompt": "<your prompt steps, joined>"}` (plus any `headers:`) to `url`
to trigger the run, and reads the trajectory from the proxy exactly as it
does for a process. Everything else is identical: the reply is still the
final assistant message, the run is still bounded to 300 seconds, and the
verdict still comes from the trajectory, never the trigger's HTTP status (a
service that answers 500 after swallowing a divergence still fails).

**The wiring contract.** flowproof cannot inject environment into a service
it did not start, so the service must ALREADY point its model calls at the
proxy's port. Start it with its model base URL set there: the same one
variable a `command:` flow relies on, just set by whoever starts the
service.

```bash
OPENAI_BASE_URL=http://127.0.0.1:4646/v1 npm run dev
# or, for an Anthropic client:
ANTHROPIC_BASE_URL=http://127.0.0.1:4646 npm run dev
```

flowproof cannot verify that wiring up front, but it catches a mispointed
service every run: a record whose trajectory is empty, or a replay whose
served-turn count is wrong, fails loudly with a hint naming the port to
point at.

**What it cannot do.** The proxy binds loopback only (it is an
unauthenticated endpoint), so the service must run on the SAME machine and
must accept a model-base-URL configuration at startup. A deployed endpoint
on someone else's infrastructure, or a service whose model URL is compiled
in with no configuration, cannot be intercepted; prefer a `command:` flow
(which flowproof starts, with zero configuration) whenever you can.

**Two caveats for a long-lived service.** First, during a run the flow's
trigger must be the ONLY source of model calls: another caller hitting the
same service interleaves into the positional turn count and diverges.
Second, the trigger must be stateless per request, or reset by a suite
`before_each`; a service that grows per-conversation history sends a
different first request on the next run, which reads as a turn-1 divergence.

## Mocking MCP tool servers (`mcp:`)

When an agent's tools are external **MCP servers** (separate processes it
speaks JSON-RPC to over the Model Context Protocol), the tool EXECUTION does
not cross the model boundary at all: the model returns a tool-use, and the
agent then calls an MCP server to run it. The `mcp:` block makes that server
a second record/replay boundary, so a flow whose tools are real MCP processes
(with side effects, network, cost) becomes testable hermetically.

```yaml
app: agent
agent:
  command: "npm run assistant"
mcp:
  - name: filesystem                       # the flow/trace name for this server
    command: "npx -y @modelcontextprotocol/server-filesystem ./sandbox"
                                           # the REAL server; run only at record
    tools:                                 # optional: intercept specific tools
      - name: delete_file
        result: { ok: true }               # answered by the stand-in, never run
```

flowproof stands in AS the server the agent spawns: it records the JSON-RPC
traffic once against the real server, then replays it with **zero external
processes**. So at replay the tools genuinely do not exist, which retires v1's
honest caveat ("the system still executes its own tools") for MCP-backed
tools. A tool given a `result:` here is answered by the stand-in and NEVER
forwarded to the real server, in either phase: the way to prove a genuinely
dangerous tool is never invoked.

**Two transports, one vocabulary.** A server speaks exactly one, chosen the
same way the `agent:` block chooses command vs url:

```yaml
mcp:
  - name: filesystem                       # a STDIO server (command:)
    command: "npx -y @modelcontextprotocol/server-filesystem ./sandbox"
  - name: remote                           # a streamable-HTTP server (url:)
    url: "https://tools.example.com/mcp"
    port: 8931                             # optional fixed listener port
```

A **stdio** server (v3.1) is spawned by the agent over a subprocess pipe, so
the only place to interpose is to BE the command the agent spawns. flowproof
injects `FLOWPROOF_MCP_SERVER_<name>` (its stand-in command) into the agent's
environment, and the agent's MCP config must point that server's command at
it.

A **streamable-HTTP** server (v3.2, `url:`) is dialed over HTTP, so flowproof
hosts an in-process loopback listener and injects
`FLOWPROOF_MCP_URL_<name>` (`http://127.0.0.1:<port>/mcp`) for the agent's
MCP config to point at instead of the real server's URL. The port is
ephemeral by default (read back from the bind); an optional `port:` forces a
fixed one, for a flow whose agent is itself `url:`-driven and so cannot be
handed the listener's port at launch (`port:` on a `command:` server is a
parse error - a stdio server is spawned, not dialed). At RECORD the listener
forwards each POST to the real `url:` (passing the agent's `Authorization`
and `Mcp-Session-Id` through, storing neither) and captures the response,
reading a `text/event-stream` answer's `data:` frames back into one JSON-RPC
message; at REPLAY it answers every POST from the recorded lane as a single
`application/json` body, with zero network. The agent is served plain JSON on
every POST reply in both phases - flowproof never turns a POST answer into an
SSE stream toward the agent.

**Server notifications (v3.3).** A server may push notifications (a JSON-RPC
message with a `method` and no `id`: `notifications/tools/list_changed`,
`.../message`, `.../progress`, `.../resources/updated`). These are now
recorded and replayed on both transports. On stdio, flowproof's stand-in
captures a notification the real server writes back and re-emits it at replay.
On HTTP, a notification that arrives inline in a POST's `text/event-stream`
body is captured (and stripped from the single JSON reply), and the standalone
server-push channel is bridged: when the agent opens `GET <endpoint>`,
flowproof opens a matching upstream `GET` and pumps the server's notification
frames through, capturing each; at replay flowproof serves that `GET` itself,
re-emitting the recorded notifications as the agent reaches the point each was
recorded (a second concurrent `GET` is a `409`). Each notification is stored
in its server's lane with an `after` anchor (the count of client calls
answered when it crossed); the anchor is an emission cue, RECORDED and
REPLAYED but never MATCHED, so a notification racing at call n versus n+1
changes bytes, not the verdict. The verdict still judges the `calls` lane
only. An agent that never opens the `GET` stream at replay simply leaves the
notifications undelivered, without hanging or failing the run.

Either way this is the same one-variable cooperation the model boundary asks
for, applied to the tool boundary. flowproof cannot verify the wiring up
front, but a record whose declared server was never contacted fails loudly
("the agent never spawned flowproof's MCP stand-in for `<name>`" for stdio,
"the agent never contacted flowproof's MCP listener for `<name>`" for http;
both name the env var its config still needs to point at), and a replay whose
calls diverge or run short fails at the exact call.

Each server records into its own lane in the trace (`mcp.<name>.calls`),
matched strictly by position: the JSON-RPC method first, then for `tools/call`
the tool name, then a field-level diff of the arguments naming the first
divergent path. The two boundaries stay consistent without a cross-boundary
equality check: the model cassette pins the tool-use decision, the MCP lane
independently pins the execution's name and arguments, so any change in how
the agent threads one into the other diverges at the MCP lane.

**What it cannot do.** An agent whose MCP server command is hardcoded and
unconfigurable, or that scrubs the environment when spawning servers, cannot
be intercepted. Server-initiated REQUESTS (sampling, elicitation, roots-list:
an id-bearing message with a `method`, which the agent must answer) are the
remaining NAMED v3.4 slice: on BOTH transports a real server that sends one
mid-record fails the record loudly with "the real MCP server sent a
server-initiated request (`<method>`) mid-response; recording server-initiated
traffic is v3.4", rather than corrupt a lane silently. (Server NOTIFICATIONS,
which need no answer, ARE recorded and replayed - see above.) The older
HTTP+SSE transport with a separate SSE endpoint is not handled. A JSON-RPC
batch (a top-level array POST) is a named `400`, not silently half-recorded.
Session ids are an ignored knob (passed through at record, a constant
`flowproof-replay` at replay, never stored or matched), as are `initialize`'s
`clientInfo`/`capabilities` (an SDK patch bump is a tuned dial);
`protocolVersion` IS matched.
