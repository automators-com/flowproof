# Debugging what a tool sends

When a request goes wrong, the first question is: what did the tool actually
put on the wire? Logs, proxies, and pretty-printers all sit between you and
the answer, and each one can normalize away the very detail you are chasing.
`flowproof capture` removes that gap. It is a small HTTP endpoint that records
every request it receives BYTE FOR BYTE - method, path, all headers, and the
raw body both as text and as a hexdump - and answers `200` so the sending
tool completes. Nothing is reserialized. Header casing and order are kept as
sent. The bytes on disk are the bytes that arrived.

It ships inside the flowproof binary, so a consultant on a locked-down
Windows box gets one signed executable instead of a Python install or an
unsigned script: `flowproof capture` and go.

## The worked example: SAP `/BA1/` field names

SAP namespace-style field names look like `/BA1/C55APPL` - a slash, a
namespace, another slash, the name. When a tool serializes a payload
containing those names, a bug in the serialization can mangle them (drop a
segment, change the slashes, re-encode the string), and a pretty-printing
proxy can hide the mangling by re-emitting a "clean" version.

Point the tool's HTTP connection at `flowproof capture` instead of its real
target, run the request, and read the saved file. Because the capture is
byte-faithful, the field names appear exactly as the tool wrote them. As a
convenience, the endpoint also scans each body for names matching the regex
`/[A-Za-z0-9_]+/[A-Za-z0-9_]+` and lists them under their own heading, so a
missing or altered `/BA1/...` name is easy to spot next to the raw bytes.

This describes what the tool does: it shows you the wire bytes. It does not
diagnose the bug for you - it makes the evidence visible so you can.

## Usage

```
flowproof capture --port 8899 [--out ./captured] [--json]
```

- `--port <N>` - the TCP port to listen on (required).
- `--out <DIR>` - where per-request `req-NNN.txt` files are written; created
  if missing. Defaults to `./captured`.
- `--json` - emit a structured JSON-Lines report on stdout, one object per
  request (method, path, ordered headers with raw casing, body length, the
  extracted namespace fields, and the saved file path), instead of the human
  view.

Point the tool-under-test's endpoint URL at `http://<this-host>:<port>/`, run
it, and watch each request print. Every request is also saved to
`<out>/req-NNN.txt`. Press Ctrl-C to stop: the listener exits cleanly.

Each saved file contains the request line, all headers verbatim, the body as
text, the namespace-style field names found, and a hexdump of the raw body -
the hexdump is the ground truth when the body is non-UTF-8 or carries control
bytes.

HTTP only. If the tool is pinned to HTTPS, either switch the connection to
HTTP for the test or put a terminating proxy in front - capturing plaintext
is the point.

## Bind address and security

`flowproof capture` binds `0.0.0.0`: all network interfaces. This is
deliberate. The motivating case is a sender on another machine (a remote API-testing tool
reaching the capture endpoint over the network), or the same box, and binding
only loopback would defeat that.

The trade-off is stated plainly, and the tool prints it on startup: the
capture endpoint is UNAUTHENTICATED, listens on ALL interfaces, and writes
raw request bodies to disk. Anyone who can reach the port can make it write to
disk, and whatever a tool sends - including anything sensitive in a payload -
lands in the `req-NNN.txt` files in plaintext. Run it deliberately for a
debugging session and stop it (Ctrl-C) when you are done. Do not leave it
running.
