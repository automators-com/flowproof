#!/usr/bin/env python3
"""A real multi-turn agent built on the official OpenAI Python SDK.

The same `openai` client as `weather_agent.py`, but built for a
CONVERSATION rather than one task. That is the whole difference, and it is
a runtime contract, not a spec feature:

  * delivery 0 arrives in `FLOWPROOF_PROMPT`, as always;
  * every LATER delivery arrives as one JSON line on stdin,
    `{"prompt": "..."}`, once the previous delivery has settled;
  * stdin is closed after the last delivery, so the loop below ends and the
    process exits on its own.

The message history is kept across deliveries, which is what makes the
second turn able to mean "yes" - the agent has to remember what it asked.

`cancel_order` here is a LOCAL STUB, not a real cancellation. In a real
suite a tool that destroys something belongs behind the `mcp:` boundary,
which flowproof answers itself so the real server never runs it. A `tools:`
mock like this flow's rewrites what the model is TOLD, and the agent still
runs its own tool - fine for a stub that mutates a dict, wrong for anything
that actually cancels an order.
"""
import json
import os
import sys

from openai import OpenAI

client = OpenAI(
    base_url=os.environ["OPENAI_BASE_URL"],
    api_key=os.environ.get("OPENAI_API_KEY", "unused-at-replay"),
)
MODEL = os.environ.get("FLOWPROOF_AGENT_MODEL", "claude-sonnet-4-5")

SYSTEM = (
    "You are an order support agent. Never cancel an order until the user "
    "has explicitly confirmed it in a later message. On the first request, "
    "ask whether they are sure instead of calling any tool."
)

TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "cancel_order",
            "description": "Cancel an order. Irreversible.",
            "parameters": {
                "type": "object",
                "properties": {"order": {"type": "string"}},
                "required": ["order"],
            },
        },
    }
]

# The stub's "world". A real one would be a service call.
CANCELLED = set()

messages = [{"role": "system", "content": SYSTEM}]


def cancel_order(order):
    CANCELLED.add(order)
    return {"order": order, "status": "cancelled"}


def settle():
    """Run the tool loop until the model answers with prose."""
    for _ in range(6):
        resp = client.chat.completions.create(
            model=MODEL, messages=messages, tools=TOOLS
        )
        msg = resp.choices[0].message
        if not msg.tool_calls:
            messages.append({"role": "assistant", "content": msg.content or ""})
            print(msg.content or "", flush=True)
            return
        messages.append(msg.model_dump(exclude_none=True))
        for call in msg.tool_calls:
            args = json.loads(call.function.arguments)
            result = cancel_order(**args)
            messages.append(
                {
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": json.dumps(result),
                }
            )
    print("(agent gave up)", flush=True)


def main():
    messages.append({"role": "user", "content": os.environ["FLOWPROOF_PROMPT"]})
    settle()
    # Every later delivery, one JSON line at a time. The loop ends when
    # flowproof closes stdin after the final delivery settles.
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        messages.append({"role": "user", "content": json.loads(line)["prompt"]})
        settle()


if __name__ == "__main__":
    main()
