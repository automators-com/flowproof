# Falsifiability fixture for cassette call-order tolerance (#258).
#
# An agent that makes TWO INDEPENDENT model calls -- neither depends on the
# other's answer, which is what makes their relative order meaningless. This is
# the goose shape that forced the 0.8.0 change: it issues its task call and a
# session-title call concurrently and does not wait, so which one reaches the
# proxy first is a coin flip, and a positional matcher reported a divergence
# when nothing about the agent had changed.
#
# Two knobs, read from files beside this script so record and replay can differ
# without an environment race (the convention mcp_stdio_e2e.rs already uses):
#
#   order.txt   "ab" (default) or "ba" -- which call is issued first
#   mutate.txt  if present, the SECOND call's content is changed
#
# `order.txt` is the tolerance: reordering must NOT break replay.
# `mutate.txt` is the discrimination: changing what is SENT must still break it.
# Without the second, "order-tolerant" would be indistinguishable from
# "nothing about the request is checked at all".
#
# Do not make these calls depend on each other. Their independence is the
# premise of the whole proof.
import json, os, urllib.request

here = os.path.dirname(os.path.abspath(__file__))
base = os.environ["OPENAI_BASE_URL"]

def read(name, default=""):
    p = os.path.join(here, name)
    return open(p).read().strip() if os.path.exists(p) else default

order = read("order.txt", "ab")
mutated = os.path.exists(os.path.join(here, "mutate.txt"))

alpha = "ALPHA: summarise the incident"
beta = "BETA: name this session"
if mutated:
    beta = "BETA: name this session differently"

calls = [alpha, beta] if order == "ab" else [beta, alpha]

last = ""
for content in calls:
    payload = json.dumps({"model": "gpt-4o",
                          "messages": [{"role": "user", "content": content}]}).encode()
    req = urllib.request.Request(base + "/chat/completions", data=payload,
                                 headers={"content-type": "application/json"})
    with urllib.request.urlopen(req) as resp:
        last = json.load(resp)["choices"][0]["message"].get("content", "")

print(last)
