# Dataset bindings

Use the smallest input mechanism that fits the test:

| Input | Use it for |
| --- | --- |
| Process environment | CI secrets and machine-specific overrides |
| Profile / `--var` | A named, reusable set of a few values |
| Sibling `*.values.yaml` | Reviewed scalar values for one flow |
| Suite `env_from` | One row minted by an external command before a suite |
| Dataset binding | The same flow or suite repeated over governed dataset rows |

A dataset binding belongs in `suite.yaml`, or in `<flow-stem>.data.yaml` for
one flow. Data Maker itself is not modified. Flowproof reads its public API
with a project `READ_ONLY` key.

```yaml
provider: datamaker
dataset: ${DM_DATASET_ID}
map:
  CUSTOMER_ID: customer_id
  FIRST_NAME: first_name
  EMAIL: contact.email
rows:
  mode: all
  limit: 1000
execution:
  concurrency: 1
  on_failure: continue
```

For a suite, nest the same object under `data:`. Set the connection without
putting credentials in the binding:

```text
DATAMAKER_API_URL=https://data.example.test/api
DATAMAKER_API_KEY=…
DATAMAKER_PROJECT_ID=project-id   # optional for a project-pinned key
```

Every invocation needs an explicit positive `limit`. CLI/CI accepts at most
10,000 rows; Desktop accepts at most 1,000. Execution is sequential by
default and in this release, preventing rows from colliding in shared SAP or
database systems.

## Deterministic selections

The first or all modes take rows in dataset order up to the limit. A range is
zero-based, start-inclusive and end-exclusive:

```yaml
rows: { mode: range, start: 1000, end: 2000, limit: 1000 }
```

A sample requires an integer seed. The same dataset identity, seed and limit
select the same ordinals:

```yaml
rows: { mode: sample, seed: 20260918, limit: 500 }
```

Shards are zero-based and do not overlap. Run each binding separately:

```yaml
# shard 0 of 4
rows: { mode: all, limit: 10000, shard: { index: 0, total: 4 } }
```

Change `index` through `1`, `2` and `3` for the other invocations.

## Precedence and evidence

From highest to lowest, values resolve from `--var`, an explicit or sibling
values file, the mapped dataset row, suite `env`/`env_from`, the process
environment, then machine credential config for missing values. Mapping
targets must be scalars; missing fields, arrays and objects fail with the row
ordinal and field path before that row's driver starts.

Each row runs in a fresh Flowproof process and therefore a fresh driver,
capture and export state. Retries reuse that process's exact mapped values.
Aggregate `result.json` and `junit.xml` contain only dataset ID, content hash,
ordinal and verdict—not the row payload or credentials.

Use `--checkpoint path --resume` to continue confirmed rows. The checkpoint
pins dataset identity, selection, mapping digest, shard and last completed
ordinal. Use `--retry-of RUN_ID` to create a new parent run containing only
the original failed ordinals; the original evidence remains unchanged.

Deleting a Data Maker dataset prevents future reads and reruns. Existing
Flowproof evidence remains useful: it retains the immutable dataset ID,
content hash, row ordinals and results, but never a copy of the row payload.
