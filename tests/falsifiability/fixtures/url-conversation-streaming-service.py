# Fixture for a STREAMING multi-turn `agent.url` conversation (#375).
#
# The streaming sibling of url-target-service.py, and stateful where that one
# is not. Two properties matter here and neither is exercised by the
# single-turn fixture:
#
#   1. It keeps its message history ACROSS trigger POSTs, so the second
#      delivery can mean "yes" -- flowproof sends each delivery as its own
#      sequential POST and the service is what remembers the conversation.
#   2. Every model call asks for `stream: true` and fully drains the SSE
#      response before answering the trigger. That is what makes this fixture
#      able to catch an early release: if flowproof POSTed the next delivery
#      while the previous stream was still draining, the frames below would be
#      short or interleaved.
#
# It appends ONE JSON line per model call to the log path in argv[2], holding
# that call's ordered frames, flushed immediately. The first frame is the
# response content type, so a stream collapsed into one buffered body is
# visible in the log rather than silently assembling into the same text.
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
import urllib.request

BASE = os.environ["OPENAI_BASE_URL"]
PORT = int(sys.argv[1])
LOG = sys.argv[2]

# Conversation state, kept for this process's whole lifetime.
messages = []


def record(frames):
    with open(LOG, "a") as fh:
        fh.write(json.dumps(frames) + "\n")
        fh.flush()


def settle():
    """Run the tool loop until the model answers with prose."""
    for _ in range(5):
        payload = json.dumps({
            "model": "gpt-4o",
            "stream": True,
            "messages": messages,
            "tools": [{"type": "function", "function": {"name": "cancel_order"}}],
        }).encode()
        req = urllib.request.Request(BASE + "/chat/completions", data=payload,
                                     headers={"content-type": "application/json"})
        frames = []
        content = ""
        calls = []
        with urllib.request.urlopen(req) as resp:
            kind = resp.headers.get("content-type", "")
            frames.append("content-type:" + kind)
            if "text/event-stream" not in kind:
                # Tolerated, not failed on: the trajectory still assembles, so
                # the frame log is the ONLY evidence the stream was collapsed.
                msg = json.load(resp)["choices"][0]["message"]
                content = msg.get("content") or ""
                calls = msg.get("tool_calls") or []
            else:
                for raw in resp:
                    line = raw.decode("utf-8").strip()
                    if not line.startswith("data:"):
                        continue
                    data = line[len("data:"):].strip()
                    if data == "[DONE]":
                        frames.append("DONE")
                        break
                    choice = json.loads(data)["choices"][0]
                    delta = choice.get("delta", {})
                    if "role" in delta:
                        frames.append("role:" + delta["role"])
                    if delta.get("content"):
                        frames.append("content:" + delta["content"])
                        content += delta["content"]
                    for call in delta.get("tool_calls", []):
                        fn = call["function"]
                        frames.append("tool:" + fn["name"] + ":" + fn["arguments"])
                        calls.append(call)
                    if choice.get("finish_reason"):
                        frames.append("finish:" + choice["finish_reason"])
        record(frames)
        if not calls:
            messages.append({"role": "assistant", "content": content})
            return
        messages.append({"role": "assistant", "content": None, "tool_calls": calls})
        for call in calls:
            # The REAL tool: a live timestamp the recording must not pin.
            real = json.dumps({"cancelled_at": 1, "status": "gone"})
            messages.append({"role": "tool", "tool_call_id": call["id"],
                             "content": real})


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        messages.append({"role": "user", "content": body.get("prompt", "")})
        try:
            settle()
            out = json.dumps({"ok": True}).encode()
        except Exception as exc:  # noqa: BLE001 - surfaced to the test
            out = json.dumps({"ok": False, "error": str(exc)}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):
        pass


HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
