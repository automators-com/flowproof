---
title: "Visual assertions and network config"
description: "Visual assertions, web network mocks, browser config, and pinning randomness and the clock for determinism."
---

```yaml
- assert_screenshot:
    name: dashboard              # baseline PNG name (no path, no extension)
    mask: ["css:.clock", "Sync"] # optional: selectors blanked before compare
    threshold: 0.001             # optional: fraction of pixels allowed to differ (default 0)
```

`record` captures the surface, blanks each mask's element rect, and mints
`<spec-stem>.baselines/<name>.png` next to the trace — re-recording (or
`record --reuse`) is how baselines refresh. Replay captures with the
**same masks** and compares pixel-exact (up to `threshold`); on failure
the run bundle gains `visual/<name>.actual.png` and `visual/<name>.diff.png`
(differing pixels in red) and the message names the diff percentage.
Masks take the same forms as quoted labels (text anchor, `css:`, `id:`)
and every mask must resolve — a silently-unmasked volatile region would
mint a flaky baseline. Pin the viewport with `browser:` so capture
dimensions stay stable across machines.

## Network mocks (web flows; spec-level, not steps)

```yaml
mock:
  - url_contains: /api/rates          # substring match on the request URL
    method: GET                       # optional; any method when absent
    status: 200                       # optional; default 200
    body:                             # any YAML: string served verbatim
      rate: 1.23                      #   (text/plain), anything else as
      source: mocked                  #   JSON; content_type: overrides
```

Requests matching a rule are answered inside the browser — the real host
is never contacted (it need not even exist). The rules travel in the
trace header and apply **identically at record and replay**: what was
mocked once is mocked always, which is what keeps the two executions
equivalent. Mocked responses carry permissive CORS headers and answer
preflights, so cross-origin `fetch()` calls just work. The tool for
third-party calls (payments, analytics) and hard-to-provoke server
states; for asserting on real APIs, use `assert_api` instead.

## Browser config (web flows; spec-level, not steps)

```yaml
browser:
  viewport:                   # device emulation, applied before navigation
    width: 390
    height: 844
    device_scale_factor: 3    # optional; default 1
    mobile: true              # optional; mobile layout + meta-viewport
    touch: true               # optional; emulate a touch screen
  user_agent: my-agent        # optional; navigator.userAgent override
  args: ["--lang=en-US"]      # optional; extra Chrome flags
  clock:                      # optional; pin the clock (GAP-P)
    at: "2026-01-15T12:00:00Z"   # required; RFC 3339, a mid-day time
    timezone: "Europe/Berlin"    # optional but recommended; IANA id
```

The config travels in the trace header and applies **identically at
record and replay** — a flow recorded on an emulated phone never replays
on a desktop viewport. This is how `*.mobile` test variants and
deterministic-seeding user agents (previously an env-var wrapper around
Chrome) become first-class. `args` forces a private (non-shared) browser
for the flow, since flags only apply at process start — expect its cold
start. A suite's `suite.yaml` may carry the same `browser:` block as a
default for every flow; a flow's own block wins outright.

### Pinning randomness

`browser.random` replaces the page's `Math.random` with a seeded PRNG,
injected before any page script for the same reason the clock's shim is: a
page that has already drawn a random number cannot be un-randomised
afterwards.

```yaml
browser:
  random:
    seed: 1234
```

Same argument as the clock, applied to the other source of per-run drift. A
page that mints a value from `Math.random` shows something different every
run, so the only honest thing to write against it is another read — and for
a value the flow must ENTER rather than compare, there is nothing to read.
Pinned, the value is a constant you can write by hand, and record and every
replay see the same one.

`seed` is a **literal**, never a `${VAR}`: a seed resolved from the
environment would make one trace mean different things on different
machines, which is the drift pinning exists to remove. Web-only; a
`random:` block on any other app kind is a spec error naming the
restriction, because there is no `Math.random` to pin on a desktop window
or an OCR frame.

Deliberately narrow, and stated rather than discovered:
`crypto.getRandomValues` is untouched (it is a security primitive, not a
convenience), web workers get their own real `Math.random`, and
server-side randomness is `mock:`'s job.

**The seed pins the SEQUENCE, not the position.** The page draws the same
series of numbers every run; which of them reaches the value you care about
depends on how many draws the page made first. A page whose earlier scripts
draw a *variable* number of times — an animation that fires once or twice
depending on timing — can still hand you a different value, taken from the
same series one place along. Observed once in the wild against a page that
generates on focus: the value was stable across six runs and shifted by
exactly one draw on a seventh.

So **assert the drawn value before you use it**:

```yaml
- assert: the "id:no1" shows 91        # the draw itself
- assert: the "id:no2" shows 98
- Type 189 into the "id:result" field  # the constant derived from it
```

Without the first two lines a shifted sequence types a confidently wrong
answer and fails somewhere else entirely, or worse, passes. With them, the
shift is the failure — which is the whole reason a pinned value is worth
asserting even though it is "constant".

### Pinning the clock

`browser.clock` freezes what the page reads as "now", so a date-dependent
flow is deterministic — a "last 7 days" filter, a "renews in N days"
label, a relative timestamp, a picker that opens on the current month. The
clock STARTS at `at` and advances at real wall rate (it is a fixed offset
on `Date`, not a hard freeze), so pick a **mid-day** `at` and no step will
straddle a pinned midnight. Both fields are literals, never `${VAR}`: a
precondition that varied by environment would not be one. Set `timezone`
whenever you set `at` — without it, local dates and week boundaries still
depend on the runner's zone.

What it does NOT cover, by design:

- **server-side "today"** — a date the SERVER computes (an SSR page, an API
  returning a relative window) is untouched; pin those with a `mock:` rule
  instead.
- **web workers** see the real clock; only the main frame's `Date` is
  pinned.
- **`performance.now()`** and timer scheduling are not shifted.

Clock control is web-only; a `clock:` block on any other app kind is a
parse error.
