---
title: "iframes and cookies"
description: "Same-origin iframe assertions and cookie controls for web flows."
---

An element inside an iframe is addressed with the same target-tail shape as
a container scope:

```yaml
- assert: the "css:#total" in the iframe "checkout" shows Total 42.00
- assert: the "Status" inside the iframe "checkout" is visible
- assert: the "css:#total" in the iframe "css:iframe[title=checkout]" shows Total 42.00
```

The frame names itself the way any target does: a quoted anchor matched
against the iframe's own `title`, `name`, `id`, or `aria-label`, or an
explicit `"css:<selector>"`. The phrase is cut out of the tail like the
container phrase, so every predicate composes without special casing, and a
role noun still goes before it.

**The frame is a fence, not a hint.** The inner target is looked up in the
frame's own document and nowhere else: if it is not in the frame, the
assertion fails even when an identically named element sits on the page
outside it. That is the whole point - a scope that silently fell back to
the main document would pass green on the wrong element.

Three failures are kept distinct so none of them can read as a pass:

| Situation | What happens |
|---|---|
| the named iframe is not on the page | fails naming the frames that ARE there (`iframe 'invoice' was never found (iframes present: checkout, receipt)`) |
| the iframe is cross-origin | the run ERRORS - the same-origin policy walls off the document, so the assertion cannot be checked, and it is never silently passed |
| the element is not inside the frame | an ordinary miss, reported as `inside iframe '<frame>'` so it is not confused with a page-wide miss |

Limits in v1, each for a reason rather than for later:

- **Value-driving actions, plus plain `Click` and `Press … button`.** `Type`,
  `Clear`, `Check`/`Uncheck`, `Remember` and `Scroll` work inside a frame,
  performed through the frame's own DOM - the same mechanism `Select` uses in
  the main document. `Click` and `Press … button` also work inside a frame:
  the driver computes the target element's page-absolute point (the frame's
  own offset in the parent document, plus the element's offset within the
  frame) and dispatches a REAL trusted click there via CDP, the same
  mechanism the top-level document's click already used - `isTrusted` is
  true, not an untrusted synthetic event.

  `Hover`, `Double-click`, `Right-click` and `Upload` remain a parse error
  naming the reason: no such point-computation is wired up for them yet, so
  each could only reach the frame as an untrusted event, which an
  application is free to ignore while the step still passes -
  release-without-effect.

  **`Replace … with` has no framed form**, even though `Clear` and `Type`
  individually do - express it as the two steps instead: `Clear the "X" in
  the iframe "Y"` followed by `Type <value> into the "X" in the iframe "Y"`.

  **A framed `Type` targeting a real `<input>`/`<textarea>` uses the SAME
  trusted keystroke path the main document does** (click the point, select
  any existing value, type for real) - not a synthetic value assignment.
  Some same-origin frames track field changes off real keyboard events
  (legacy widget frameworks, not only modern ones listening for
  `input`/`change`), and never see a value this driver set directly. Any
  other framed element (contenteditable, a custom widget with no native
  keystroke target) keeps the synthetic value-plus-event write: `.value` is
  set through the native setter and `input`/`change` fire on it. Both
  replace what was there rather than appending. Two guards keep either path
  honest rather than silent: the target must not be `disabled` or read-only
  (a value assignment succeeds on a disabled control where typing would be
  ignored - so it is refused by name), and the value is read BACK from the
  element afterwards, so a control that rejected or rewrote it fails the
  step.

  SAP WebGUI inside Fiori has one more guard. When Flowproof detects a
  WebGUI-like same-origin frame, a framed native `Type` commits the field
  with `Tab`, waits for SAP's field processing to settle, and reads the
  value back after that commit. This catches the prefilled-field failure
  mode where a field briefly displays the typed value, then restores its old
  value on blur. The failure is reported at the field-setting step, without
  printing either value, because the same machinery can drive credential
  fields.

  Fiori application content usually lives inside the `Application` iframe,
  so assertions about result-screen content should be frame-scoped too:

  ```yaml
  - assert: the "css:body" in the iframe "Application" shows ${MATERIAL}
  ```
- **Same-origin only.** A cross-origin frame's document is unreachable, and
  the CDP per-frame execution-context path is not deterministic enough to
  ship behind a grammar that looks identical.
- **One frame, no combining.** A frame scope cannot be nested inside a
  container or cell scope yet; one context per target.
- An ordinal cannot address a frame (`the 2nd iframe`): name it.
### Cookie controls (web, security)

"The session cookie is httpOnly" is a control that regresses SILENTLY: an
auth library config changes, the cookie becomes readable by page scripts,
and nothing about the UI looks different. These assertions pin it.

```yaml
control:
  id: sec.session.cookie-flags
  title: The session cookie is not readable by page scripts
steps:
  - assert: cookie "session_token" exists
  - assert: cookie "session_token" is httpOnly
  - assert: cookie "remember_me" is persistent
```

**A cookie's VALUE cannot be asserted, and never will be.** A session
cookie's value is a credential. There is no `cookie "x" is <value>` form,
no `contains`, and no redacted comparison: the moment a value can be
compared, the expected value has to live in the trace and the failure
message tempts someone to print the actual one. flowproof's traces are
meant to be safe to commit and safe to attach to a bug report. A failure
names the cookie, which fact failed, and - for a missing cookie - the NAMES
of the cookies that were set, which is what fixes a typo.

**The `is secure` honesty note.** Browsers exempt localhost from the secure
requirement, so `is secure` can pass over plain http and certify nothing
about production. The step still passes, because teams do run
TLS-terminated staging, but the run prints a warning saying it does not
certify production behaviour. Read that warning as a finding: a control
that has only ever passed over http is an unverified control.

Out of v1: `sameSite` (three-valued, so it does not fit the `is <flag>`
shape), exact expiry timestamps (nondeterministic across record and
replay), and domain/path matchers.
