# Spike diagnostics — NOT tests

These two scripts are throwaway instruments used to answer the iteration-1 spike
questions in [`../LOOP.md`](../LOOP.md). They are committed as reproduction
evidence, not as part of a test suite. Nothing in CI runs them.

- `probe_server.py` — a logging HTTP server that answers OpenAI- and
  Anthropic-shaped requests. Used to determine **which** base-URL environment
  variable goose actually honours, by running goose under `env -i` with one
  candidate set at a time.
- `marker_mcp.py` — a minimal stdio MCP server that records that it was spawned
  and exposes one read-only tool. Used to prove goose spawns an MCP server
  command handed to it via `--with-extension "$FLOWPROOF_MCP_SERVER_<NAME>"`.

**`probe_server.py` is not the fake-model baseline.** That baseline is a separate,
deliberately scheduled deliverable, built only after flow 1 is recorded and green.
Neither script may ever be used as the upstream for a `flowproof record` run — a
cassette recorded against a scripted responder is a fabricated cassette.

## Reproducing

```sh
# goose v1.44.0, prebuilt Linux binary
curl -sSL -o goose.tar.bz2 \
  https://github.com/aaif-goose/goose/releases/download/v1.44.0/goose-x86_64-unknown-linux-gnu.tar.bz2
tar xjf goose.tar.bz2

PROBE_LOG=$PWD/probe.jsonl python3 probe_server.py 8099 &

# Q1 — which base-URL variable does goose honour?
env -i PATH=$PWD:/usr/bin:/bin HOME=$PWD/goosehome \
  GOOSE_PROVIDER=openai GOOSE_MODEL=probe-model OPENAI_API_KEY=fake \
  OPENAI_BASE_URL=http://127.0.0.1:8099/v1 \
  goose run --no-session -t "hi"

# Q2 — can an MCP extension command be redirected?
env -i PATH=$PWD:/usr/bin:/bin HOME=$PWD/goosehome \
  GOOSE_PROVIDER=openai GOOSE_MODEL=probe-model OPENAI_API_KEY=fake \
  OPENAI_BASE_URL=http://127.0.0.1:8099/v1 \
  MARKER_FILE=$PWD/marker.log \
  FLOWPROOF_MCP_SERVER_MARKER="python3 $PWD/marker_mcp.py" \
  sh -c 'goose run --no-session --no-profile \
         --with-extension "MARKER_ENV=from_env $FLOWPROOF_MCP_SERVER_MARKER" \
         -t "call the marker_ping tool"'
```

Run the Q1 matrix with each of `OPENAI_BASE_URL`, `OPENAI_API_BASE`,
`OPENAI_BASE`, `OPENAI_HOST`, and once with none set. The no-variable control run
is the row that makes the result meaningful.

## Minimal reproduction of B1 (no model, no network, no TLS)

`probe_server.py` honours `PROBE_DELAY` (seconds). A slow canned upstream drops a
captured turn in roughly 1 run in 3; an instant one never does. This is the
cheapest way to reproduce B1 and the way to verify any fix.

```sh
PROBE_DELAY=2.5 PROBE_LOG=$PWD/probe.jsonl python3 probe_server.py 8102 &

for i in $(seq 1 6); do
  rm -f probe.jsonl iso.trace.jsonl
  FLOWPROOF_AGENT_UPSTREAM=http://127.0.0.1:8102/v1 ANTHROPIC_API_KEY=fake \
    npx flowproof record iso.flow.yaml >/dev/null 2>&1
  echo "run $i: upstream=$(grep -c . probe.jsonl) turns=$(python3 -c \
    "import json;print(len(json.load(open('iso.trace.jsonl'))['cassette']['turns']))")"
done
```

`iso.flow.yaml` is `../goose/smoke.flow.yaml` with the assertion relaxed to
`reply contains PROBE_OK`. A correct fix makes this print `turns=2` six times out
of six. Do NOT keep any trace produced this way — it is recorded against a
scripted responder and is not a real cassette.
