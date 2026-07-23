#!/usr/bin/env python3
"""A real, minimal agent built on the official OpenAI Python SDK.

Not a flowproof test double: it is the same `openai` client thousands of
production agents use, doing a genuine tool-calling loop. flowproof points
its base URL at the record/replay proxy through the standard
`OPENAI_BASE_URL` env var, and hands it the task through `FLOWPROOF_PROMPT`.

The `get_weather` tool returns a VOLATILE reading (a live timestamp), so a
deterministic replay is only possible because flowproof substitutes the
spec's mock at the model boundary.
"""
import json
import os
import time

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
            "name": "get_weather",
            "description": "Current weather for a city.",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
        },
    }
]


def get_weather(city):
    # A real tool with a volatile result: the observation time changes
    # every run. flowproof's mock is what makes replay deterministic.
    return {"city": city, "sky": "clear", "observed_at_ns": time.time_ns()}


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
                result = get_weather(**args)
                messages.append(
                    {
                        "role": "tool",
                        "tool_call_id": call.id,
                        "content": json.dumps(result),
                    }
                )
            continue
        print(msg.content or "")
        return
    print("(agent gave up)")


if __name__ == "__main__":
    main()
