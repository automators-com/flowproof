---
title: "Out-of-band assertions"
description: "Structured assertions outside the UI, including assert_spreadsheet and how reads vs. writes are retried."
---

```yaml
- assert_sql:
    connection: reporting        # resolved from FLOWPROOF_SQL_REPORTING
    query: SELECT count(*) FROM orders WHERE ref = '4711'
    equals: "1"
- assert_api:
    request: GET ${API}/orders/4711
    status: 200
    body_contains: "confirmed"
- assert_api:                    # authenticated JSON POST
    request: POST ${API}/connections/test
    headers:
      Authorization: Bearer ${SESSION_TOKEN}
    body:
      provider: postgres
      connectionString: ${TEST_CONN_STRING}
    status: 200
    body_contains: "Database not yet supported!"
- assert_api:                    # response-side JSON-field assertion
    request: GET ${API}/testData/users
    status: 200
    body_json: results.0.balance # a dotted path into the JSON response
    equals: 150953               # the leaf at that path must equal this
- assert_api:                    # how many elements are in a collection
    request: GET ${API}/testData/users
    status: 200
    body_json: results           # the path must resolve to an ARRAY
    count: 5                     # exactly 5 elements (count_at_least: 2 = a minimum)
- assert_api:                    # response-header assertion
    request: GET ${API}/testData/users
    status: 200
    header: Content-Type         # response header name (case-insensitive)
    header_contains: json        # a substring of the header value
```

`headers` values and `body` string values may carry `${VAR}` refs. The
trace stores only the raw reference; tokens and connection strings resolve
when the probe fires (record and every replay). `body` is any YAML
(mapping, list, or string), sent as JSON with `content-type:
application/json` unless you set your own `content-type` header (yours
wins). A `body` on GET/HEAD/DELETE is rejected at parse time.

`body_json` reads a value out of the JSON response and asserts on it,
alongside `status` and `body_contains` (all three may appear on one step;
they are checked in the order status, then body_contains, then body_json).
The path is a dotted sequence of segments, each a plain object key or a
decimal array index: `results.0.balance` means "the `balance` field of the
first element of the `results` array". That is the whole path language:
there are no wildcards, filters, brackets, or quoting, so a key that
literally contains a dot cannot be reached. One `body_json` per step; to
assert several fields, use several steps.

`body_json` on its own is an existence check: the path must resolve to a
scalar leaf (mirroring `assert_sql`, where omitting `equals` means a row
merely has to exist). Add `equals` (a string, number, or boolean) to also
check the value; `equals` without `body_json` is a parse-time error. A
string `equals` may carry a `${VAR}` ref, resolved at probe time exactly
like `body_contains` (only the ref travels in the trace). Comparison has
two tiers: when both the leaf and `equals` are numbers, they compare
numerically (`150953` equals `150953`); otherwise they compare by exact
canonical text, so a string leaf never numeric-matches a number (the
leaf `"0953"` does not equal the number `953`). Only leaves compare, so
there is no deep object equality.

The extracted response value never enters the trace: only the request and
the raw expectation are stored, and the plucked value exists solely inside
the live comparison, re-fetched on both record and replay. The failure
modes are soft (the auto-wait loop keeps polling until they clear or the
timeout elapses): a non-JSON body reports "response body is not valid
JSON"; a path that runs off the document names the segment where it died
(`path 'results.0.balance' stops at segment 'balance'`); a path that lands
on an object or array reports "path resolves to a non-scalar; assert a leaf
value".

`header` asserts on a response header, alongside `status`, `body_contains`,
and `body_json` (all may appear on one step; they are checked in the order
status, then body_contains, then body_json, then header). The header NAME is
case-insensitive, per HTTP: `header: Content-Type` matches a response that
spells it `content-type`. If the response repeats the header, its values are
joined with ", " (HTTP field-value semantics) before matching. One header per
step; to assert several headers, use several steps.

`header` on its own is an existence check: the header must be present
(mirroring `body_json` alone, where reaching a scalar leaf is the whole
assertion). Add `header_equals` (exact value) or `header_contains` (a
substring) to also check the value; at most one of the two per step, and
either without `header` is a parse-time error. Value comparison is
case-SENSITIVE (unlike the name). A `header_equals`/`header_contains` value
may carry a `${VAR}` ref, resolved at probe time exactly like `body_contains`
(only the ref travels in the trace). The live header value never enters the
trace: it exists solely inside the comparison, re-fetched on both record and
replay. The failure modes are soft: an absent header reports "response has no
'<name>' header (status <code>)", and a value mismatch reports "header
'<name>' is '<actual>', expected <equals|contains> '<want>' (status <code>)".

`count` (exactly N) and `count_at_least` (a minimum) ask how many elements
are in the array at `body_json`. Either requires `body_json`, at most one of
the two may appear, and neither pairs with `equals` (a count needs an array,
`equals` needs a scalar leaf) - all three are parse-time errors. When the
path resolves to something other than an array, the failure names what was
actually there: "path 'page' is an object, count requires an array (status
200)". A wrong count reports both sides: "path 'results' has 3 elements,
expected exactly 9 (status 200)". Both are soft failures, so on a `GET` they
auto-wait: "poll until the collection has N rows" is a real pattern.

### assert_spreadsheet: an exported file, read directly

```yaml
- assert_spreadsheet:
    path: ${captured.pir_export}   # may carry ${captured.x} / ${VAR} refs
    sheet: Sheet1                  # optional; the workbook's first sheet if absent
    at: B2                         # an absolute A1 reference ...
- assert_spreadsheet:
    path: ${captured.pir_export}
    column: Net Price              # ... OR a header + row anchor, not both
    row_contains: "100-100"
    equals: "12.50"
```

Reads the file directly (`calamine`) rather than through UI Automation over
Excel's own grid — the manual test's own second check ("open the file and
review it") is a screen a person can look at, but its GRID support over UIA
is untested and known-flaky, so the out-of-band read is the one that must
hold. `path` resolves `${captured.x}` then `${VAR}`, exactly like a typed
field — the common case is a path a `Wait until the download completes as
<name>` step captured moments earlier in another surface.

The cell is addressed EITHER by `at` (an absolute `A1` reference) OR by
`column`+`row_contains` together — never both, and never neither, both
parse-time errors. `column` resolves against the sheet's first row the same
two-rung ladder a web table cell uses: exact match after trim, then a
unique substring match; an ambiguous or missing header is a parse-time-shaped
failure naming what was asked for. `row_contains` is the unique data row
(excluding the header) where ANY cell's text contains it — ambiguous or
absent is reported the same way.

`equals`/`contains` compare the cell's text (its canonical rendering — a
number reads as `"12.5"`, not `"12.50000"`); at most one may be set, a
parse-time error otherwise. With neither, resolving the cell is the whole
assertion — mirroring `assert_sql`, where omitting `equals` means a row
merely has to exist. Like `assert_sql`, this is always a READ: a
just-landed download may still be mid-write when the first poll fires, so
the auto-wait loop keeps re-opening the file until it resolves or the bound
(`timeout_seconds`, default 10s) elapses.

### Retries: reads are polled, writes are sent once

A failing assertion auto-waits by RE-SENDING its probe until the bound
expires. That is right for a read (the API is still converging) and wrong
for a write, because the probe IS the mutation: polling a failing `POST`
delivers it once per tick, and a single failing step was measured
delivering 41 `POST`s inside the default 10s bound. So only `GET` and
`HEAD` are retried. `POST`, `PUT`, `PATCH` and `DELETE` are sent exactly
once, and their failure says so. (`DELETE` is idempotent per HTTP but not
side-effect-free, so it is grouped with the writes.) `assert_sql` is a
read and keeps polling.

Override per step when the default is wrong:

```yaml
- assert_api:                    # poll a write until it converges
    request: POST ${API}/jobs
    status: 202
    retry: true
- assert_api:                    # ask a read exactly once
    request: GET ${API}/jobs/1
    status: 200
    retry: false
```

On releases without `retry:`, `timeout_seconds: 0` is the mitigation: it
leaves no wait budget, so the probe fires once.
