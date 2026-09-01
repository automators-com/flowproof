---
status: done
---
# Plan 5 — Fiori field value commit

Plan 4 made a single flow shareable without a `suite.yaml`: credentials and
connection defaults come from `flowproof config`, while business data can live
next to the flow in `<flow-stem>.values.yaml`. During live Fiori verification
of that work, the generated flow reached the SAP WebGUI report screen and read
the sibling values file correctly, but exposed a separate runtime correctness
bug: a prefilled Fiori input could appear to accept a typed business value and
then restore its old value when SAP processed the field.

This plan fixes that globally for the risky Fiori/SAP WebGUI path. A
value-setting step must not pass merely because keystrokes were sent; it must
pass only after the application has accepted the value.

## Live finding

The live test used the generated artifact flow for the Fiori tile "Display
Purchasing Info Record by Supplier" with:

```yaml
MATERIAL: TG10
SUPPLIER: '10300001'
PLANT: '1010'
NET_PRICE: '12.35'
```

Credentials were read from the user's Fiori config profile; business data was
read from the sibling values file. The flow reached the selection screen and
set Material and Plant correctly. Supplier was different: the input was
prefilled with `10300016`. The initial clear/type path did not leave
`10300001` in the field after SAP processed the field, so Execute ran with the
wrong supplier and the result assertion failed.

The passing local artifact had to replace the Supplier step with a more
human-equivalent sequence:

```yaml
- Click the Supplier input inside iframe "Application"
- Press Control+A
- Press Backspace
- Type ${SUPPLIER}
- Press Tab
- Assert the Supplier field contains ${SUPPLIER}
```

That sequence passed, and deterministic replay passed, which proves the data
layer from plan 4 is working. The remaining issue is the web adapter's
field-driving contract for SAP/Fiori inputs.

## Problem

`crates/flowproof-adapters/src/web.rs` already has special handling for typed
targets inside frames:

- `FRAME_ACT(..., "type", ...)` resolves the element in the frame and returns
  a page-absolute click point for native inputs.
- `WebAppDriver::type_text` clicks that point, calls frame-side
  `select_all`, then sends real keystrokes.

That is close, but it is still too weak for classic SAP WebGUI controls inside
Fiori. It verifies neither that the old value was actually replaced nor that
SAP accepted the new value after the field's commit/blur cycle. In the live
case the visible field temporarily matched the requested supplier, then SAP
restored the old supplier once focus moved.

This is dangerous because a test can proceed with stale business data while
the trace still shows the intended placeholder. The failure may appear later
as "no result found" or an assertion miss, instead of failing at the field that
rejected the value.

## Decision

Implement a stronger framed-input value contract for Fiori/SAP WebGUI:

```text
When Flowproof sets a native input inside a Fiori/SAP WebGUI frame, the step
passes only after the field value is replaced, committed, and read back as the
requested value.
```

For the scoped Fiori/WebGUI path, setting a text field means:

1. Resolve the target inside the frame.
2. Click the native input with a real page-coordinate click.
3. Select the existing value through a browser keyboard chord or a verified
   frame-side selection path.
4. Remove the selected content.
5. Type the requested text with real keystrokes.
6. Commit the field with `Tab`.
7. Wait briefly for SAP's field processing to settle.
8. Read the same element value back from the frame.
9. Fail the step if the accepted value differs from the requested value.

This should live in the adapter/runtime, not in `author-from-doc` output. A
manual flow, an authored flow, and a deterministic replay should all get the
same protection.

## Scope

The stronger behavior applies to same-origin framed native inputs that are
identified as SAP/Fiori/WebGUI-like. Detection should be conservative, using
signals already visible to the adapter such as the Fiori `Application` frame,
SAP WebGUI DOM ids/classes, or other stable WebGUI markers found in the live
DOM. It should not blindly add `Tab` after every generic web input.

Generic top-level web inputs keep their current behavior. Generic framed web
inputs also keep current behavior unless they match the SAP/Fiori/WebGUI
signals. That avoids surprising normal web apps where typing into a field
should not automatically tab to the next control.

## Authoring impact

`author-from-doc` should not need to emit the workaround sequence. It should
continue emitting normal field-setting steps such as:

```yaml
- Type ${SUPPLIER} into the "Supplier" in the iframe "Application"
```

The runtime should make that step robust. Prompt guidance can be updated to
prefer field-scoped assertions for Fiori result screens, but correctness must
not depend on prompt wording.

## Trace and replay impact

Traces should preserve the semantic step the user wrote or the author emitted.
Replay should use the same stronger adapter behavior for the recorded
field-setting action. The trace should not need to expand into the internal
click/select/delete/type/tab sequence unless existing trace structure already
requires that representation.

Failure details should name the real problem while staying value-free:

```text
the field inside iframe 'Application' accepted a different value after commit
```

This deliberately diverges from the earlier accepted-vs-requested wording: by
the time the web adapter reads the field it only has resolved text, and cannot
know whether the requested value came from business data or a credential
placeholder. In this plan's intended use case the values are business data, but
the same field machinery can still receive credential placeholders on login
screens, so failure rendering must preserve the existing masking guarantees by
not printing either value.

## Tests

- Add a local framed HTML fixture that behaves like the live SAP field: it has
  a prefilled value, appears to accept typed input, and reverts on blur unless
  the correct commit path happens.
- Add a regression test proving `Type ${SUPPLIER} into ... in the iframe ...`
  replaces the prefilled value, commits it, and only passes after readback.
- Add a negative regression test proving a revert after commit fails at the
  value-setting step with a clear value-free commit/readback message.
- Add a non-SAP generic web fixture proving ordinary top-level typing does not
  gain an automatic `Tab`.
- Add a non-SAP generic framed fixture proving the stronger commit behavior is
  not applied without SAP/Fiori/WebGUI detection.
- Keep existing plan-4 values-file tests passing so the stacked branch remains
  compatible with the shareability PR.

## Docs

- Update `docs/authoring.md` or the closest existing Fiori authoring section
  to state that Fiori/WebGUI field typing is committed and verified by
  Flowproof.
- Update any Fiori example guidance to prefer frame-scoped assertions for
  application content, e.g. `the "css:body" in the iframe "Application" shows
  ${MATERIAL}` instead of top-level `page shows ${MATERIAL}`.
- Add this plan to `plans/README.md`.

## Out of scope

- Changing the plan-4 values-file design.
- Moving business data into `flowproof config`.
- Making every generic web input auto-commit with `Tab`.
- Re-recording committed example traces unless the implementation makes an
  existing trace invalid.
- Adding live Fiori credentials or live Fiori network tests to CI.

## Open questions

None. The design is intentionally scoped: fix the adapter/runtime contract for
Fiori/SAP WebGUI framed inputs, keep generic web behavior stable, and use local
fixtures for regression coverage.

## Landed

Implemented in `crates/flowproof-adapters/src/web.rs`: same-origin framed
native inputs that look like SAP WebGUI now use the real click, Control+A,
Backspace, type, Tab commit path and then read the field back before the step
passes. Generic top-level inputs and generic framed inputs keep their existing
non-auto-Tab behavior.

Added `crates/flowproof-cli/tests/fiori_field_commit_e2e.rs` with local browser
fixtures for the SAP-like commit path, the revert-after-commit failure, and the
two generic non-auto-Tab guards. Updated `docs/authoring.md` with the Fiori
commit/readback behavior and frame-scoped assertion guidance.

Verified with `cargo fmt --all --check`, `cargo check -j 1 -p flowproof-cli
--test fiori_field_commit_e2e`, `cargo clippy -j 1 -p flowproof-adapters
--all-targets --all-features -- -D warnings`, `cargo clippy -j 1 -p
flowproof-cli --all-targets --all-features -- -D warnings`, and `git diff
--check`. Runtime `cargo test` attempts for the new E2E and plan-4 values-file
compatibility test were blocked locally by the machine running out of disk space
while building executable artifacts; CI should run them with normal disk
headroom.
