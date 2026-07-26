#!/usr/bin/env python3
"""The agent under test in the README's demo GIF.

A real, minimal support assistant built on the official OpenAI Python SDK - the
same client a production agent uses, doing a genuine tool-calling loop. flowproof
points its base URL at the record/replay proxy through the standard
`OPENAI_BASE_URL` env var and hands it the task through `FLOWPROOF_PROMPT`.

Deliberately smaller than examples/agent-demo: `lookup_order` returns a
DETERMINISTIC result, so the flow needs no `result:` mock and the demo stays a
tight three commands. See examples/agent-demo for the mock (a tool whose real
result is volatile) and examples/access-control for the guard assertions.
"""
import json
import os

from openai import OpenAI

client = OpenAI(
    base_url=os.environ["OPENAI_BASE_URL"],
    api_key=os.environ.get("OPENAI_API_KEY", "unused-at-replay"),
)
MODEL = os.environ.get("FLOWPROOF_AGENT_MODEL", "claude-sonnet-4-5")

TOOLS = [
    {
        "type": "function",
        "function": {
            "name": "lookup_order",
            "description": "Look up the status of a customer order.",
            "parameters": {
                "type": "object",
                "properties": {"order_id": {"type": "string"}},
                "required": ["order_id"],
            },
        },
    }
]

ORDERS = {"A-4471": {"status": "shipped", "carrier": "DPD", "total_eur": 240}}


def lookup_order(order_id):
    return {"id": order_id, **ORDERS.get(order_id, {"status": "unknown"})}


def main():
    messages = [{"role": "user", "content": os.environ["FLOWPROOF_PROMPT"]}]
    for _ in range(6):
        resp = client.chat.completions.create(
            model=MODEL, messages=messages, tools=TOOLS
        )
        msg = resp.choices[0].message
        if msg.tool_calls:
            messages.append(msg.model_dump(exclude_none=True))
            for call in msg.tool_calls:
                args = json.loads(call.function.arguments)
                messages.append(
                    {
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": json.dumps(lookup_order(**args)),
                    }
                )
            continue
        print(msg.content or "")
        return
    print("(agent gave up)")


if __name__ == "__main__":
    main()
