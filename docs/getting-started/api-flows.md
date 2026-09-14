---
title: "API-only flows"
description: "Flows with no UI at all, including minting traces offline against a contract responder."
---

Not every test drives a UI. `app: api` runs a flow of **out-of-band
assertions only** (HTTP status/body and SQL row checks) with no browser
and no window launched, on any platform:

```yaml
name: Provisioning API
app: api
steps:
  - assert_api:
      request: GET ${API}/health
      status: 200
      body_contains: '"status":"ok"'
  - assert_api:
      request: POST ${API}/teams/${TEAM}/members    # cross-team write must 403
      status: 403
  - assert_sql:
      connection: reporting
      query: SELECT count(*) FROM members WHERE team_id = '${TEAM}'
      equals: "1"
```

These are the tests that assert on HTTP status codes and response bodies
with no UI to drive; they run through the same deterministic record/replay
spine (zero model calls), and the connection names and `${VAR}` hosts never
enter the trace. See `examples/api/health.flow.yaml`.

A repeated block with one value changing collapses into a `foreach`
values matrix: scalars use `${each}`, mappings use `${each.<key>}`
(whole-string tokens keep their YAML type, so `status: ${each.status}`
stays a number). Expansion happens at parse time: each iteration is an
ordinary recorded step.

```yaml
steps:
  - foreach:
      values: [mysql, mssql, oracle]
      steps:
        - assert_api:
            request: POST ${API}/connections/test
            body: { type: "${each}" }
            status: 500
            body_contains: "Database not yet supported!"
```

### Minting traces offline against a contract responder

Traces store only raw `${VAR}` references, verified end to end: no
resolved host, token, or connection string ever lands in the file. That
gives `app: api` flows a genuinely useful property: **recording against a
faithful local responder produces the same trace a live-stack recording
would** (only `trace_id`/timestamps differ). The official pattern for
minting api-flow traces without infrastructure:

1. Stand up a tiny local server speaking the endpoint's contract (the
   right paths, status codes, and body shapes, not the real logic).
2. Point the spec's `${VAR}`s at it and `flowproof record`.
3. Commit the trace. At replay, the same `${VAR}`s point at the real
   stack; the trace neither knows nor cares where it was recorded.

The in-repo proof is `crates/flowproof-cli/tests/api_pipeline.rs`: every
api-flow trace there is minted against a throwaway `tiny_http` responder
and replayed cleanly, with leak assertions on the secrets.
