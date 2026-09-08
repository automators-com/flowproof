---
title: "Test-context seeding"
description: "Sessions, fixtures, and navigation seeding, including declaring a shared identity once and referencing it by name."
---

Real app suites don't rebuild their starting state through the UI in
every test: they inject it and start on the page under test. That
covers two idioms, and the `session:` block handles both:

- **an authenticated session** (Playwright's storageState pattern), so
  a flow skips the login UI;
- **an app-state fixture** (a pre-filled cart, a chosen project, a
  dismissed banner), so a flow skips the setup clicks that are not what
  it is testing.

Declare either in the spec; it's applied **before the page loads**
(cookies via CDP, localStorage before any page script runs), travels in
the trace with `${VAR}` references intact, and is re-applied identically
at every replay. Seeding runs **once, on the flow's first document**:
state the flow mutates afterwards (an item added to the seeded cart)
survives mid-flow navigation and reload instead of being reset to the
fixture, and that holds across a navigation that changes ORIGIN (a login
host to an app host) as well as within one:

```yaml
name: Templates workspace
app: web
url: ${DM_BASE_URL}/templates          # env refs resolve at launch
session:
  cookies:
    - name: automators.session
      value: ${DM_SESSION_COOKIE}      # resolved at apply time, never stored
  local_storage:
    projectId: ${DM_PROJECT_ID}
steps:
  - Wait until page shows templates found within 30s
  - Go to /settings                    # same-origin navigation mid-flow
  - Reload the page
```

Fixture values that are not secrets can be plain literals. A checkout
flow that needs an item already in the cart seeds it directly instead of
clicking through the catalog first:

```yaml
name: Checkout with a seeded cart
app: web
url: http://localhost:3000/cart.html
session:
  cookies:
    - name: session-username
      value: standard_user             # a plain literal is fine here
  local_storage:
    cart-contents: "[4]"               # the fixture the flow starts from
steps:
  - assert: the "css:.cart_item" appears 1 time
  - Press the "Checkout" button
```

Two notes on values. Real credentials and tokens always go through
`${VAR}` references: they resolve when the session is applied and the
trace stores only the reference, never the value. And seeded `${VAR}`s
are not automatically part of an `assert_no_secret_leak` scan; a flow
that seeds `${SESSION_TOKEN}` and wants leak coverage for it must list
it in the assertion explicitly.

`Go to` takes a path (resolved against the flow URL's origin) or a full
URL.

### Shared identities: declare once, reference by name

An access-control suite runs the same flows as several identities (a viewer,
an admin), so repeating the `session:` mapping in every flow is noise. Declare
each identity ONCE in the suite manifest under `identities:`, and reference it
from a flow by name. Each entry is exactly the inline `session:` shape
(cookies plus `local_storage`, values `${VAR}` refs resolved at apply time,
never stored):

```yaml
# suite.yaml
identities:
  viewer:
    cookies:
      - name: app.session
        value: ${VIEWER_SESSION_COOKIE}     # resolved at apply time, never stored
  admin:
    cookies:
      - name: app.session
        value: ${ADMIN_SESSION_COOKIE}
    local_storage:
      role: admin
```

The flow's `session:` field is an untagged string-or-mapping, distinguished
by YAML type the same way `app:` and `window:` are: a bare STRING names a
suite identity, a MAPPING is the inline setup you already write. So nothing
shipped changes meaning; existing specs keep their inline mapping.

```yaml
session: viewer          # a string: resolved against the suite's identities
```

**Dereference is a load-time copy, not a runtime lookup.** When the flow is
LOADED, the named identity's `${VAR}`-bearing setup is copied into the trace
header EXACTLY as an inline `session:` mapping is copied today, so the trace
stays self-contained: it carries the identity's setup, not a pointer to the
suite. A later edit to the suite's identity definition is therefore a
re-record or heal event on the flows that use it, never a silent change to
existing traces (the same rule as editing an inline `session:`). A bare
`session: viewer` in a flow with no governing `suite.yaml` is a load-time
error naming the missing suite; an unknown name is an error listing the
identities the suite declares.

**An identity carries the browser session only.** Cookies and `local_storage`,
nothing else. An access-control flow that also probes an API with
`assert_api` needs a bearer token (`${VIEWER_TOKEN}`), and that token is NOT
part of the browser session, so it does not live in the identity block. API
credentials stay plain suite `env` by convention: `${VIEWER_TOKEN}` is a suite
variable resolved at apply time like any other. The identity block is a
faithful mirror of the shipped `session:` shape, not a general credential
container.

For the control-authoring forms these identities feed (the `control:` block,
the denial pattern, `assert_no_secret_leak`, and `flowproof audit`), see
[authoring.md](authoring.md#security-controls).
