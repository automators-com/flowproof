#!/usr/bin/env python3
"""Plain-HTTP -> HTTPS relay so flowproof can record through this container's
TLS-terminating proxy. flowproof's Rust client uses compiled-in roots and does
not honour SSL_CERT_FILE or the system trust store, so it cannot verify the
proxy CA itself. ENVIRONMENT WORKAROUND, not adoption glue."""
import os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import requests

UPSTREAM = os.environ.get("RELAY_UPSTREAM", "https://api.anthropic.com")
CA = os.environ.get("RELAY_CA", "/root/.ccr/ca-bundle.crt")
# content-encoding is stripped because requests already decodes the body;
# forwarding the header would advertise gzip over plaintext bytes.
HOP = {"connection", "keep-alive", "transfer-encoding", "content-length", "host",
       "content-encoding"}


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _relay(self, body):
        url = UPSTREAM + self.path
        try:
            j = __import__("json").loads(body or b"{}")
            msgs = j.get("messages", [])
            with open(os.environ.get("RELAY_LOG", "/tmp/relay_calls.log"), "a") as f:
                f.write("CALL %s model=%s stream=%s nmsgs=%d keys=%s last=%r\n" % (
                    self.path, j.get("model"), j.get("stream"), len(msgs),
                    sorted(j.keys()),
                    str(msgs[-1].get("content"))[:80] if msgs else ""))
        except Exception:
            pass
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        try:
            r = requests.request(self.command, url, data=body, headers=headers,
                                 stream=True, verify=CA, timeout=300)
        except Exception as e:
            self.send_response(502)
            self.send_header("content-type", "text/plain")
            self.end_headers()
            self.wfile.write(str(e).encode())
            return
        # Buffer the whole body and send it with an exact content-length.
        # Hand-rolled chunked encoding here was silently dropping the second
        # of two near-simultaneous responses, which looked exactly like a
        # flowproof capture bug. flowproof always forwards upstream
        # non-streaming, so there is nothing to stream and nothing to gain.
        body = r.content
        self.send_response(r.status_code)
        for k, v in r.headers.items():
            if k.lower() not in HOP:
                self.send_header(k, v)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()

    def do_POST(self):
        n = int(self.headers.get("content-length") or 0)
        self._relay(self.rfile.read(n) if n else None)

    def do_GET(self):
        self._relay(None)

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8100
    print(f"relay {port} -> {UPSTREAM}", file=sys.stderr, flush=True)
    ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
