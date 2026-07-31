# Fixture for the streamable-HTTP MCP boundary (#212).
#
# An agent that speaks MCP over HTTP rather than stdio. It reads
# FLOWPROOF_MCP_URL_WEATHER -- the in-process listener flowproof stands up in
# place of the real server -- and POSTs JSON-RPC to it.
#
# That one variable is the whole cooperation the HTTP boundary asks for. A
# real agent's MCP config must point at it instead of the real server's URL,
# and an agent that ignores it reaches the real server directly while the lane
# records nothing. flowproof cannot verify the wiring up front; it can only
# fail loudly when the listener is never contacted. This fixture does the
# honest thing so the recorded lane is real.
#
# The tool result is printed so the caller can prove which server answered:
# the real one replies REAL:<tool>, and flowproof's stand-in replies from the
# mock when one is declared.
import json, os, urllib.request

base = os.environ["FLOWPROOF_MCP_URL_WEATHER"]
model = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]

payload = json.dumps({"model": "gpt-4o",
                      "messages": [{"role": "user", "content": prompt}]}).encode()
req = urllib.request.Request(model + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    reply = json.load(resp)["choices"][0]["message"].get("content", "")

def rpc(obj):
    body = json.dumps(obj).encode()
    r = urllib.request.Request(base, data=body,
                               headers={"content-type": "application/json",
                                        "accept": "application/json, text/event-stream"})
    with urllib.request.urlopen(r) as resp:
        return json.loads(resp.read().decode())

rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2024-11-05",
                "clientInfo": {"name": "http-agent", "version": "1"},
                "capabilities": {}}})
rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
called = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
              "params": {"name": "get_weather", "arguments": {"city": "Nairobi"}}})
print("TOOL", json.dumps(called))
print(reply)
