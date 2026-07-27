#!/usr/bin/env python3
"""A tiny stdio MCP server for flow 3: one read-only tool and one destructive one.

Only ever spawned at RECORD, and only for the tools flowproof does NOT intercept.
`delete_all` carries a `result:` in the spec, so flowproof's stand-in answers it
and this file's implementation must never run. It therefore writes a loud marker
if it ever does - if that file appears, the `mcp:` boundary did not hold and the
flow is lying.
"""
import json, os, sys

DESTRUCTIVE_MARKER = os.environ.get("DESTRUCTIVE_MARKER", "/tmp/DESTRUCTIVE_RAN")

# Diagnostic: who spawned us? If the parent is `flowproof mcp-stdio`, the
# stand-in is in the path and forwarded to us. If it is the agent, it is not.
try:
    _ppid = os.getppid()
    with open("/proc/%d/cmdline" % _ppid, "rb") as _f:
        _parent = _f.read().replace(b"\0", b" ").decode("utf-8", "replace").strip()
    with open(os.environ.get("PARENT_LOG", "/tmp/mcp_parent.txt"), "a") as _f:
        _f.write("ppid=%d parent=%s\n" % (_ppid, _parent))
except Exception:
    pass

TOOLS = [
    {
        "name": "list_files",
        "description": "List the files in the project. Read-only.",
        "inputSchema": {"type": "object", "properties": {}, "required": []},
    },
    {
        "name": "delete_all",
        "description": "Permanently delete every file in the project. Destructive.",
        "inputSchema": {
            "type": "object",
            "properties": {"confirm": {"type": "boolean"}},
            "required": ["confirm"],
        },
    },
]


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def call(name):
    if name == "delete_all":
        # Never reached when the spec gives delete_all a `result:`. If this ever
        # fires, flowproof forwarded a tool it promised to answer itself.
        with open(DESTRUCTIVE_MARKER, "a") as f:
            f.write("delete_all EXECUTED — the mcp: boundary did not hold\n")
        return "deleted everything"
    if name == "list_files":
        return "README.md\nCargo.toml\nsrc/main.rs"
    return f"unknown tool {name}"


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    m, rid = req.get("method"), req.get("id")
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": req.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "files", "version": "0.0.1"}}})
    elif m == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": TOOLS}})
    elif m == "tools/call":
        name = req.get("params", {}).get("name", "")
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": call(name)}], "isError": False}})
    elif m in ("resources/list", "prompts/list"):
        key = m.split("/")[0]
        send({"jsonrpc": "2.0", "id": rid, "result": {key: []}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
