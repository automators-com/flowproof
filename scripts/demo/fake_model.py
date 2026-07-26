#!/usr/bin/env python3
"""A local OpenAI-compatible upstream, so the README GIF can be captured from a
REAL `flowproof record` with no API key and no network.

It scripts the same two turns the in-tree agent E2E does
(`crates/flowproof-cli/tests/agent_flow_e2e.rs`): ask for `lookup_order`, then -
once a tool result comes back - answer in text. What it deliberately never does
is ask for `issue_refund`, which is the behaviour the flow's guard assertion
pins.

Nothing about the capture is faked downstream of here: flowproof records this
upstream through its own proxy, mints a real cassette, and the replay in the GIF
serves that cassette with this process not running.

Prints its base URL on the first line of stdout, then serves until killed.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOL_TURN = {
    "choices": [
        {
            "index": 0,
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": None,
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "lookup_order",
                            "arguments": json.dumps({"order_id": "A-4471"}),
                        },
                    }
                ],
            },
        }
    ]
}

TEXT_TURN = {
    "choices": [
        {
            "index": 0,
            "finish_reason": "stop",
            "message": {
                "role": "assistant",
                "content": "Order A-4471 has shipped and is on its way.",
            },
        }
    ]
}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        body = self.rfile.read(length).decode("utf-8", "replace")
        # Ask for the tool until a tool result has come back.
        turn = TEXT_TURN if '"role": "tool"' in body or '"role":"tool"' in body else TOOL_TURN
        payload = json.dumps(turn).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, *args):  # keep the captured terminal clean
        pass


def main():
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(f"http://127.0.0.1:{server.server_address[1]}/v1", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
