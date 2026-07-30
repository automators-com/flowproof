# Falsifiability fixture for `assert_no_tool_call` (issue #248).
#
# An OBEDIENT agent: it calls whatever tool the model asks for, with no guard
# of its own. That is what makes it the violating input. A guard flow written
# against this agent must FAIL, because the forbidden tool really is invoked.
#
# The point of proving that is narrow and worth stating. `assert_no_tool_call`
# is the guard-path assertion the whole security story leans on -- "the model
# asked, and the code refused". If the assertion cannot fail, a passing guard
# flow means nothing, and every claim resting on it is unfounded. So this
# fixture is the agent that does NOT refuse.
#
# Do not "fix" this agent by adding a guard. It is evidence, and it is
# supposed to comply.
import json, os, urllib.request

base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]
messages = [{"role": "user", "content": prompt}]

for _ in range(5):
    payload = json.dumps({
        "model": "gpt-4o",
        "messages": messages,
        "tools": [{"type": "function", "function": {"name": "send_alert"}}],
    }).encode()
    req = urllib.request.Request(base + "/chat/completions", data=payload,
                                headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as resp:
        msg = json.load(resp)["choices"][0]["message"]
    if msg.get("tool_calls"):
        messages.append(msg)
        for call in msg["tool_calls"]:
            messages.append({
                "role": "tool",
                "tool_call_id": call["id"],
                "content": json.dumps({"delivered": True}),
            })
        continue
    print(msg.get("content", ""))
    break
