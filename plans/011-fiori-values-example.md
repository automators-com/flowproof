---
status: draft
---
# Plan 11 - replace generated Fiori test data with a checked-in values example

The Fiori examples currently use `examples/fiori/suite.yaml` plus
`examples/fiori/mint-test-data.sh` to produce business data at run time:
`MATERIAL`, `SUPPLIER`, `PLANT`, and `NET_PRICE`. That made sense when the
goal was "safe data injection without hardcoding values in the flow", but it
now makes the examples harder to understand than the product path we actually
want users to copy.

Plan 4 already shipped the better primitive: business data belongs in a YAML
values file and stays out of the `.flow.yaml` steps. This plan replaces the
Fiori example's bespoke minting harness with a visible
`examples/fiori/values.yaml`, so users can see the pattern in one directory:

```text
examples/fiori/
  manage-info-records.flow.yaml
  purchasing-info-record-api.flow.yaml
  values.yaml
```

Credentials still stay in `flowproof config`, CI secrets, or the caller's
environment. This plan only moves non-secret test-case data out of
`suite.yaml`/`env_from` and into a checked-in example values file.

## The gap this closes

The current files accidentally teach the older path:

- `suite.yaml` says the Fiori suite's data is "MINTED, not hardcoded".
- `mint-test-data.sh` performs a live OData query before a run can even
  resolve `${MATERIAL}`.
- `docs/self-help.md` still explains the Fiori example through `env_from`.
- `crates/flowproof-cli/tests/examples_resolve.rs` asserts that the Fiori
  suite exists and points at `mint-test-data.sh`.

That is a lot of machinery for values like material number, supplier, plant,
and expected net price. Those values are not credentials, and a checked-in
example is a clearer demonstration of how a user keeps business data out of
the flow file without hiding it behind a shell script.

The distinction is still important: business data can be sensitive in a real
customer system, so docs should not say "all business data is public". The
claim is narrower: the sample values used by this repository's examples are
safe to treat as example inputs, while secrets and credentials are still never
checked in.

## What changes

1. Delete `examples/fiori/mint-test-data.sh`.
2. Delete `examples/fiori/suite.yaml` unless it is still needed for ordering
   or hooks after the implementation pass. The intended final shape has no
   Fiori suite manifest.
3. Add `examples/fiori/values.yaml` with the real demo/reference-system
   business data used by the Fiori examples:

   ```yaml
   MATERIAL: "..."
   SUPPLIER: "..."
   PLANT: "1010"
   NET_PRICE: "..."
   ```

   This is intentionally a committed example file, not a secret-backed CI
   fixture. The values are business inputs for the demo/reference system, not
   credentials. Keep credentials out of this file: do not include
   `FIORI_USER`, `FIORI_PASSWORD`, `SAP_USER`, `SAP_PASSWORD`, or
   `SAP_ODATA_BASIC_AUTH`.
4. Update Fiori example comments that currently say the data is minted by
   `suite.yaml`/`env_from`. They should say business data comes from
   `examples/fiori/values.yaml`, usually passed with `--vars`.
5. Update docs that use the Fiori examples to demonstrate `env_from`
   (`docs/self-help.md` is the main hit). `env_from` remains documented in
   the general suite/harness docs, but the Fiori example should no longer be
   the primary illustration of that mechanism.
6. Update `crates/flowproof-cli/tests/examples_resolve.rs` so it asserts the
   example values file parses as a YAML mapping with the expected keys,
   instead of asserting that `suite.yaml` contains `mint-test-data.sh`.

## Runtime shape

No new runtime feature is required for this plan. `flowproof run` already
accepts `--vars <path>` for both a single flow and directory runs, and the
suite-directory path applies those values per flow.

The intended commands become:

```console
$ flowproof run examples/fiori/display-info-record-by-supplier.flow.yaml --vars examples/fiori/values.yaml
$ flowproof run examples/fiori/purchasing-info-record-api.flow.yaml --vars examples/fiori/values.yaml
$ flowproof run examples/fiori/ --vars examples/fiori/values.yaml --missing skip
```

The existing sibling-file convention (`x.flow.yaml` -> `x.values.yaml`) stays
unchanged. This plan deliberately uses a shared `values.yaml` because several
Fiori flows exercise the same material/supplier/plant record. That makes it a
good example of the explicit `--vars` path, not the sibling auto-discovery
path.

Docs should still show the sibling-file path too, because it is the simplest
shape for one standalone flow:

```text
display-info-record-by-supplier.flow.yaml
display-info-record-by-supplier.values.yaml
```

The Fiori examples use the shared `--vars examples/fiori/values.yaml` form;
the docs can use a smaller generic snippet to demonstrate sibling
auto-discovery without duplicating the Fiori values file.

## CI impact

There is no current GitHub workflow that runs `examples/fiori/` as a live
Fiori suite. The SAP workflow runs `examples/sap/`, not `examples/fiori/`.
So the implementation does not need to change `.github/workflows/sap-e2e.yml`
unless another branch adds Fiori replay there first.

That gap is now tracked separately in
[#575](https://github.com/automators-com/flowproof/issues/575): CI runs SAP
GUI examples today, but not Fiori examples.

If a Fiori workflow is added later, it should pass
`--vars examples/fiori/values.yaml` explicitly. It should continue to source
credentials from GitHub secrets or `flowproof config`, not from the values
file.

## Non-goals

- Do not remove `suite.yaml`, `env_from`, `before_each`, `after_each`, or
  `order` from Flowproof. Suites still cover dynamic data minting,
  seed/cleanup effects, shared browser settings, identities, and ordered
  multi-flow harnesses.
- Do not add flow-level `env_from` or `values_from`. Plan 4 explicitly left
  dynamic minting in suite scope; this plan only changes the examples.
- Do not move credentials into `examples/fiori/values.yaml`.
- Do not re-record Fiori traces just to rename credential variables. Existing
  comments already explain that trace-bound names need a live re-record.

## Acceptance checks

- `rg "mint-test-data|MINTED|env_from" examples/fiori docs/self-help.md crates/flowproof-cli/tests/examples_resolve.rs`
  finds no stale Fiori-example claims.
- `examples/fiori/values.yaml` parses as a flat YAML mapping, and the example
  test asserts it contains `MATERIAL`, `SUPPLIER`, `PLANT`, and `NET_PRICE`.
- `examples/fiori/values.yaml` contains committed demo/reference-system
  business values, not placeholders and not credentials.
- `cargo test -p flowproof-cli --test examples_resolve -- --nocapture`
  passes.
- A dry parse of the affected Fiori flows still succeeds through the existing
  example-resolution tests.

## Resolved decisions

- Commit the actual demo/reference-system business values that the OData
  picker was fetching. They are example inputs, not credentials, so duplicating
  them into GitHub secrets would not add protection. Credentials and auth
  tokens still stay in secrets/config.
- Keep the Fiori implementation on one shared
  `examples/fiori/values.yaml`, because several flows target the same record.
- Update docs to show both business-data patterns: the shared `--vars` file
  used by the Fiori examples, and the sibling `<flow-stem>.values.yaml`
  convention for single-flow users.
- Track live Fiori CI coverage separately in issue #575 instead of expanding
  this plan beyond the values-file/example cleanup.
