# Fixture for the `agent.url` HTTP-target driver (#211).
#
# A long-lived service flowproof does NOT start. That is the whole point of the
# url driver: flowproof cannot inject environment into a process it did not
# spawn, so the service must already be pointed at the proxy by whoever started
# it. This script reads OPENAI_BASE_URL at startup, exactly as the docs say a
# real service must.
#
# POST /task with {"prompt": ...} triggers one turn: the service calls the
# model and answers 200. flowproof reads the trajectory from the proxy, never
# from this response -- so the verdict comes from what crossed the model
# boundary, not from what this service chose to return.
import json, os, sys
from http.server import BaseHTTPRequestHandler, HTTPServer
import urllib.request

BASE = os.environ["OPENAI_BASE_URL"]
PORT = int(sys.argv[1])

class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        body = json.loads(self.rfile.read(n) or b"{}")
        prompt = body.get("prompt", "")
        payload = json.dumps({
            "model": "gpt-4o",
            "messages": [{"role": "user", "content": prompt}],
        }).encode()
        req = urllib.request.Request(BASE + "/chat/completions", data=payload,
                                     headers={"content-type": "application/json"})
        try:
            with urllib.request.urlopen(req) as resp:
                json.load(resp)
            out = json.dumps({"ok": True}).encode()
        except Exception as exc:  # noqa: BLE001 - surfaced to the test, not swallowed
            out = json.dumps({"ok": False, "error": str(exc)}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(out)))
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *args):
        pass

HTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
