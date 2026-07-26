#!/usr/bin/env python3
"""Minimal stdio MCP server. Writes a marker file when spawned, exposes one tool."""
import json, os, sys

MARK = os.environ.get("MARKER_FILE", "/tmp/marker_mcp.spawned")
with open(MARK, "a") as f:
    f.write("SPAWNED argv=%r MARKER_ENV=%s\n" % (sys.argv, os.environ.get("MARKER_ENV", "")))

TOOL = {
    "name": "marker_ping",
    "description": "Returns a fixed marker string. Read-only.",
    "inputSchema": {"type": "object", "properties": {}, "required": []},
}


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    m, rid = req.get("method"), req.get("id")
    with open(MARK, "a") as f:
        f.write("RPC %s\n" % m)
    if m == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "protocolVersion": req.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "marker", "version": "0.0.1"}}})
    elif m == "tools/list":
        send({"jsonrpc": "2.0", "id": rid, "result": {"tools": [TOOL]}})
    elif m == "tools/call":
        send({"jsonrpc": "2.0", "id": rid, "result": {
            "content": [{"type": "text", "text": "MARKER_PONG"}], "isError": False}})
    elif m in ("resources/list", "prompts/list"):
        key = m.split("/")[0]
        send({"jsonrpc": "2.0", "id": rid, "result": {key: []}})
    elif rid is not None:
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
