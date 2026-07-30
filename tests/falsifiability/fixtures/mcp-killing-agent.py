# Falsifiability fixture for the MCP stand-in's recording durability (#257).
#
# A HOSTILE agent, and hostile in exactly one way: it drives a normal MCP
# exchange and then KILLS the stand-in outright instead of closing its stdin.
#
# That single difference is the whole fixture. Before 0.8.0 the lane was
# written only at stdin EOF, so an agent that terminated its MCP subprocess
# abruptly never got there and the ENTIRE recording was lost. The fix persists
# the lane after every captured call, atomically.
#
# Nothing in the suite exercised it. Every other MCP test closes stdin politely
# and waits, which is precisely the path the defect did NOT affect — so all of
# them would stay green if the fix were reverted tomorrow.
#
# Do not "fix" this agent to shut down gracefully. Its rudeness is the evidence.
import json, os, shlex, subprocess, urllib.request

here = os.path.dirname(os.path.abspath(__file__))
base = os.environ["OPENAI_BASE_URL"]
prompt = os.environ["FLOWPROOF_PROMPT"]

payload = json.dumps({"model": "gpt-4o",
                      "messages": [{"role": "user", "content": prompt}]}).encode()
req = urllib.request.Request(base + "/chat/completions", data=payload,
                             headers={"content-type": "application/json"})
with urllib.request.urlopen(req) as resp:
    reply = json.load(resp)["choices"][0]["message"].get("content", "")

cmd = os.environ["FLOWPROOF_MCP_SERVER_WEATHER"]
proc = subprocess.Popen(shlex.split(cmd), stdin=subprocess.PIPE, stdout=subprocess.PIPE)

def rpc(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()
    return json.loads(proc.stdout.readline())

def notify(obj):
    proc.stdin.write((json.dumps(obj) + "\n").encode()); proc.stdin.flush()

rpc({"jsonrpc": "2.0", "id": 1, "method": "initialize",
     "params": {"protocolVersion": "2024-11-05",
                "clientInfo": {"name": "killing-agent", "version": "1"},
                "capabilities": {}}})
notify({"jsonrpc": "2.0", "method": "notifications/initialized"})
rpc({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}})
weather = rpc({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
               "params": {"name": "get_weather", "arguments": {"city": "Nairobi"}}})
print("WEATHER", json.dumps(weather))

# The violation: no stdin.close(), no wait(). SIGKILL, mid-session.
proc.kill()
print(reply)
