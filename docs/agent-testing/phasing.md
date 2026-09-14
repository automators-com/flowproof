---
title: "Phasing"
description: "What landed in v1, v2, and v3 of agent-boundary testing."
---

1. **v1**: OpenAI-compatible chat-completions proxy (non-streaming),
   `app: agent` process driver, cassette in trace v1 (additive header +
   step artifacts), `assert_tool_call` grammar, trajectory diff on
   re-record.
2. **v2**: **Landed** - the Anthropic Messages API (`/v1/messages`) and
   streaming replay for both dialects. A request with `stream: true` is served
   the recorded turn as a synthetic SSE stream in the client's own dialect
   (OpenAI chat-completion chunks, or Anthropic `message_start` /
   `content_block_*` / `message_delta` / `message_stop` events), so every
   existing cassette serves a streaming client with no re-record and no schema
   change. Chunk boundaries are synthesized rather than recorded (they carry
   no test signal, and recording them would break turn matching); the
   assembled turn is still what matches, and `stream` is transport, never part
   of the comparison. Both wire protocols normalize into one neutral cassette,
   tagged per turn (`protocol`, defaulting to `openai` so v1 traces are
   byte-unchanged); a turn recorded in one dialect and replayed in another
   diverges on that first. To keep record and replay symmetric, the record
   path forwards non-streaming to the upstream and synthesizes the same stream
   back to the agent. Also landed: **http-target agents** (drive an
   already-running service via `agent.url` instead of spawning a process; see
   "Driving a running service" above). v2 is complete.
3. **v3**: MCP servers as a second mockable boundary, for systems whose
   tools are external MCP processes rather than internal functions.
   **Landed (v3.1)**: the stdio transport, with per-tool result mocks and
   per-server strict-positional lanes in the trace (see "Mocking MCP tool
   servers" above). **Landed (v3.2)**: the streamable-HTTP transport
   (`url:`/`port:`), an in-process loopback listener that forwards to the
   real server at record (reading `application/json` or `text/event-stream`
   answers) and replays the lane as single JSON bodies with zero network.
   The trace shape is unchanged, so a lane is transport-blind: one recorded
   through stdio replays through an HTTP-declared server and vice versa.
   **Landed (v3.3)**: server-initiated NOTIFICATIONS and the standalone
   server-push SSE stream. A notification is recorded (inline in a POST's SSE
   body, or off the bridged `GET` stream) into its lane with an `after`
   anchor, and replayed over the `GET` stream flowproof now serves (a second
   concurrent `GET` is a `409`); anchors are recorded and replayed but never
   matched, so the verdict is unchanged. The remaining v3.4 slice is
   server-initiated REQUESTS (sampling, elicitation, roots-list), which need
   answer correlation: on both transports a request mid-record fails by name
   rather than corrupt a lane, and a JSON-RPC batch is a `400`.
