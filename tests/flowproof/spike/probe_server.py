#!/usr/bin/env python3
"""Probe: logs every HTTP request goose makes, answers OpenAI/Anthropic shaped."""
import json, os, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ.get("PROBE_LOG", "probe.jsonl")

OPENAI_REPLY = {
    "id": "chatcmpl-probe", "object": "chat.completion", "created": 1, "model": "probe",
    "choices": [{"index": 0, "finish_reason": "stop",
                 "message": {"role": "assistant", "content": "PROBE_OK"}}],
    "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2},
}
ANTHROPIC_REPLY = {
    "id": "msg_probe", "type": "message", "role": "assistant", "model": "probe",
    "content": [{"type": "text", "text": "PROBE_OK"}],
    "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 1},
}
MODELS = {"object": "list", "data": [
    {"id": "probe-model", "object": "model", "created": 1, "owned_by": "probe"}]}


class H(BaseHTTPRequestHandler):
    def _log(self, body):
        rec = {"t": time.time(), "method": self.command, "path": self.path,
               "headers": {k.lower(): v for k, v in self.headers.items()},
               "body": body[:4000].decode("utf-8", "replace") if body else ""}
        with open(LOG, "a") as f:
            f.write(json.dumps(rec) + "\n")
        print(f"PROBE-HIT {self.command} {self.path}", file=sys.stderr, flush=True)

    def _send(self, obj):
        raw = json.dumps(obj).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        self._log(b"")
        self._send(MODELS if "model" in self.path else {"ok": True})

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        body = self.rfile.read(n) if n else b""
        self._log(body)
        if "messages" in self.path and "chat" not in self.path:
            self._send(ANTHROPIC_REPLY)
        else:
            self._send(OPENAI_REPLY)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8099
    print(f"probe listening on {port}", file=sys.stderr, flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
